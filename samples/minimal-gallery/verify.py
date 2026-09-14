import argparse
import importlib.util
import json
import hashlib
import subprocess
import tempfile
import time
import re
import urllib.request
import signal
import uuid
import io
from pathlib import Path
from contextlib import contextmanager
import boto3
from PIL import Image

def parse_limits_json(value):
    """Read numeric application budgets; the Rust manifest loader validates fields."""
    try:
        limits = json.loads(value)
    except json.JSONDecodeError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if not isinstance(limits, dict) or any(
        not isinstance(key, str) or type(number) is not int or number < 0
        for key, number in limits.items()
    ):
        raise argparse.ArgumentTypeError("limits must be a JSON object of nonnegative integer budgets")
    return limits


def limits_toml(limits):
    """Encode the numeric record as a TOML inline table without interpreting keys."""
    return "{ " + ", ".join(
        f"{json.dumps(key)} = {number}" for key, number in limits.items()
    ) + " }"


parser = argparse.ArgumentParser(
    description="Verify a published gallery against disposable PostgreSQL and S3 storage."
)
parser.add_argument("--module", type=Path, required=True)
parser.add_argument("--name", default="minimal-gallery")
parser.add_argument("--mode", required=True)
parser.add_argument("--limits-json", type=parse_limits_json, default={'module_bytes_compressed': 52428800, 'instance_memory_max': 134217728, 'cpu_ms_per_http_dispatch': 30000, 'subrequests_per_dispatch': 200, 'pending_futures_per_dispatch': 64}, help="Application resource budgets as a numeric JSON object")
parser.add_argument("--wisp", type=Path, required=True)
parser.add_argument("--schema", type=Path, required=True)
parser.add_argument("--records", type=Path, required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parents[2]
root = args.records.resolve()
root.mkdir(parents=True, exist_ok=True)
spec = importlib.util.spec_from_file_location(
    "pg", repo / "scenario-tests/developer-experience/postgres.py"
)
pg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pg)
wisp = args.wisp.resolve()


def make_fixture(root):
    image = Image.new("RGB", (600, 400))
    image.putdata(
        [
            ((x * 7 + y) % 256, (y * 5 + x) % 256, (x + y * 3) % 256)
            for y in range(400)
            for x in range(600)
        ]
    )
    exif = Image.Exif()
    exif[271] = "Wisp Fixture"
    exif[272] = "Gallery 171"
    exif[274] = 6
    exif[34665] = {36867: "2026:01:02 03:04:05"}
    image.save(root / "original.jpg", quality=90, exif=exif)
    with Image.open(root / "original.jpg") as read:
        assert read.size == (
            600,
            400,
        ), "Fixture dimensions must match workload contract"
        assert read.getexif()[274] == 6, "Fixture must contain actual orientation"
        assert (
            read.getexif().get_ifd(34665)[36867] == "2026:01:02 03:04:05"
        ), "Fixture must contain actual EXIF sub-IFD timestamp"


@contextmanager
def objects(d):
    key = "GK" + uuid.uuid4().hex
    secret = uuid.uuid4().hex + uuid.uuid4().hex
    cfg = d / "garage.toml"
    cfg.write_text(
        'metadata_dir="/tmp/meta"\ndata_dir="/tmp/data"\ndb_engine="lmdb"\nreplication_factor=1\nrpc_bind_addr="127.0.0.1:3901"\nrpc_public_addr="127.0.0.1:3901"\nrpc_secret="'
        + uuid.uuid4().hex
        + uuid.uuid4().hex
        + '"\n[s3_api]\ns3_region="garage"\napi_bind_addr="0.0.0.0:3900"\n'
    )
    cfg.chmod(0o600)
    credentials = d / "garage.env"
    credentials.write_text(
        f"GARAGE_DEFAULT_ACCESS_KEY={key}\nGARAGE_DEFAULT_SECRET_KEY={secret}\nGARAGE_DEFAULT_BUCKET=gallery\n"
    )
    credentials.chmod(0o600)
    container = pg.command(
        [
            "docker",
            "run",
            "--detach",
            "--rm",
            "--name",
            "wisp-gallery-" + uuid.uuid4().hex,
            "--label",
            "wisp.test=gallery-171",
            "--publish",
            "127.0.0.1::3900",
            "--env-file",
            str(credentials),
            "--mount",
            f"type=bind,source={cfg},target=/etc/garage.toml,readonly",
            "dxflrs/garage:v2.3.0",
            "/garage",
            "server",
            "--single-node",
            "--default-bucket",
        ]
    )
    try:
        port = int(
            pg.command(["docker", "port", container, "3900/tcp"]).rsplit(":", 1)[1]
        )
        endpoint = f"http://127.0.0.1:{port}"
        client = boto3.client(
            "s3",
            endpoint_url=endpoint,
            region_name="garage",
            aws_access_key_id=key,
            aws_secret_access_key=secret,
        )
        deadline = time.monotonic() + 60
        while True:
            try:
                client.head_bucket(Bucket="gallery")
                break
            except Exception:
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.1)
        yield port, endpoint, key, secret, client
    finally:
        pg.command(["docker", "rm", "--force", container])


def request(url, method, path, data=None):
    with urllib.request.urlopen(
        urllib.request.Request(url + path, data=data, method=method), timeout=120
    ) as r:
        return r.status, r.read()


def verify_image(url, container, s3, original):
    assert request(url, "GET", "/images") == (
        200,
        b"[]",
    ), "Guest must read initially empty PostgreSQL"
    status, data = request(url, "POST", "/images", original)
    created = json.loads(data)
    assert status == 201, "Image ingest must return created"
    assert (
        created["width"],
        created["height"],
        created["thumbnailWidth"],
        created["thumbnailHeight"],
    ) == (
        600,
        400,
        256,
        170,
    ), "Thumbnail must preserve pixel aspect ratio without applying EXIF orientation"
    assert created["exif"] == {
        "make": "Wisp Fixture",
        "model": "Gallery 171",
        "dateTimeOriginal": "2026:01:02 03:04:05",
        "orientation": 6,
    }, "Real EXIF fields must persist"
    status, data = request(url, "GET", "/images")
    assert status == 200 and json.loads(data) == [
        created
    ], "Gallery list must return persisted metadata"
    stored = pg.command(
        [
            "docker",
            "exec",
            container,
            "psql",
            "-U",
            "postgres",
            "-d",
            "todo",
            "-Atc",
            'SELECT "ExifJson" FROM "GalleryImages"',
        ]
    )
    assert (
        json.loads(stored) == created["exif"]
    ), "Independent PostgreSQL client must observe EXIF"
    id = created["id"]
    assert request(url, "GET", f"/images/{id}/original") == (
        200,
        original,
    ), "Retrieved original must be byte-identical including EXIF"
    status, thumbnail = request(url, "GET", f"/images/{id}/thumbnail")
    assert status == 200, "Thumbnail retrieval must succeed"
    with Image.open(io.BytesIO(thumbnail)) as image:
        assert image.format == "PNG" and image.size == (
            256,
            170,
        ), "Independent decoder must verify thumbnail format and dimensions"
        assert not image.getexif(), "Thumbnail must omit source EXIF"
        assert all(
            low < high for low, high in image.convert("RGB").getextrema()
        ), "Thumbnail must preserve nonconstant source image content"
    prefix = id.replace("-", "")
    assert (
        s3.get_object(Bucket="gallery", Key=prefix + "/original")["Body"].read()
        == original
    ), "Independent S3 client must observe exact original"
    assert (
        s3.get_object(Bucket="gallery", Key=prefix + "/thumbnail.png")["Body"].read()
        == thumbnail
    ), "Independent S3 client must observe thumbnail"


make_fixture(root)
with tempfile.TemporaryDirectory(prefix="run-", dir=root) as tmp:
    d = Path(tmp)
    with pg.database(d) as (container, port, password), objects(d) as (
        s3port,
        endpoint,
        key,
        secret,
        s3,
    ):
        pg.command(
            [
                "docker",
                "exec",
                "-i",
                container,
                "psql",
                "-U",
                "postgres",
                "-d",
                "todo",
                "-v",
                "ON_ERROR_STOP=1",
            ],
            data=args.schema.read_text(),
        )
        mode = args.mode
        project = d / mode
        project.mkdir()
        config_values = {
            "ConnectionStrings:Gallery": f"Host=127.0.0.1;Port={port};Database=todo;Username=postgres;Password={password}",
            "Gallery:S3:Endpoint": endpoint,
            "Gallery:S3:Region": "garage",
            "Gallery:S3:Bucket": "gallery",
            "Gallery:S3:AccessKey": key,
            "Gallery:S3:SecretKey": secret,
        }
        module = args.module.resolve()
        (project / "wisp.toml").write_text(
            f'name="{args.name}"\ntenant="local-development"\nmodule="{module}"\nlimits={limits_toml(args.limits_json)}\n[dev]\ninit_sandbox="runtime-bringup"\nallow_endpoints=["127.0.0.1:{port}","127.0.0.1:{s3port}","[::ffff:127.0.0.1]:{s3port}"]\n[dev.trust]\nbundle="{d}/ca.crt"\nsha256="{hashlib.sha256((d/'ca.crt').read_bytes()).hexdigest()}"\n'
        )
        logpath = root / f"{mode}-ingest.log"
        with logpath.open("w") as log:
            process = subprocess.Popen(
                [
                    str(wisp),
                    "dev",
                    "--watch",
                    str(project),
                    "--no-build",
                    "--no-watch",
                    "--port",
                    "0",
                    "--runtime-config-stdin",
                ],
                stdin=subprocess.PIPE,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
            )
            try:
                process.stdin.write(json.dumps(config_values) + "\n")
                process.stdin.flush()
                deadline = time.monotonic() + 300
                while True:
                    match = re.search(
                        rf"ready {re.escape(args.name)} at (http://127.0.0.1:\d+)",
                        logpath.read_text(),
                    )
                    if match:
                        break
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("Guest readiness failed: " + mode)
                    time.sleep(0.25)
                url = match.group(1)
                verify_image(url, container, s3, (root / "original.jpg").read_bytes())
                print(
                    mode
                    + ": real guest EXIF/resize/S3/verified-TLS PostgreSQL ingest and retrieval passed",
                    flush=True,
                )
            finally:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    raise
        for value in [password, key, secret]:
            assert (
                value not in logpath.read_text()
            ), "Credentials must not appear in guest logs"
