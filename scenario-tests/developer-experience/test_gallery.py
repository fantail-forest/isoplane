import json
import subprocess
import tempfile
import time
import urllib.request
import urllib.error
import os
import shutil
import selectors
import xml.etree.ElementTree as ET
import uuid
import io
from pathlib import Path
from contextlib import contextmanager
import boto3
from PIL import Image

import unittest
from types import SimpleNamespace
import common as pg

repo = Path(__file__).resolve().parents[2]


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
    exif[271] = "Isoplane Fixture"
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
            "isoplane-gallery-" + uuid.uuid4().hex,
            "--label",
            "isoplane.test=gallery-171",
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
    try:
        with urllib.request.urlopen(
            urllib.request.Request(url + path, data=data, method=method), timeout=120
        ) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as error:
        with error:
            body = error.read().decode("utf-8", errors="replace")
        raise AssertionError(f"{method} {path} returned HTTP {error.code}: {body}") from error


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
        "make": "Isoplane Fixture",
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


class Gallery(unittest.TestCase):
    def test_images_round_trip_through_database_and_object_storage(self):
        args = SimpleNamespace(
            sample=os.environ['ISOPLANE_TEST_SAMPLE_NAME'],
            dotnet=Path(os.environ['ISOPLANE_TEST_DOTNET']),
            prepared=Path(os.environ['ISOPLANE_TEST_SCENARIO_DIRECTORY']),
            schema=Path(os.environ['ISOPLANE_TEST_SCHEMA']),
            name='gallery', configuration='Release')
        isoplane = Path(os.environ['ISOPLANE_TEST_CLI_ASSEMBLY'])
        scratch = repo / '.test-runs/gallery'
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="run-", dir=scratch) as tmp:
            d = root = Path(tmp)
            make_fixture(root)
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
                mode = args.configuration
                if args.sample == 'aspnet-gallery':
                    shutil.copytree(repo / 'samples/minimal-gallery', d / 'minimal-gallery',
                                    ignore=shutil.ignore_patterns('bin', 'obj', 'out', '.isoplane'))
                project = d / args.sample
                pg.prepared_project(args.prepared / args.sample, project)
                env = os.environ.copy()
                env["APPDATA"] = str(d / "user-secrets")
                dotnet = str(args.dotnet.resolve())
                env["DOTNET_ROOT"] = str(args.dotnet.resolve().parent)
                env["PATH"] = str(isoplane.parent) + os.pathsep + str(args.dotnet.resolve().parent) + os.pathsep + env["PATH"]
                pg.command([dotnet, "user-secrets", "init", "--project", str(project)], env=env)
                config_values = {
                    "ConnectionStrings:Gallery": f"Host=127.0.0.1;Port={port};Database=todo;Username=postgres;Password={password}",
                    "Gallery:S3:Endpoint": endpoint,
                    "Gallery:S3:Region": "garage",
                    "Gallery:S3:Bucket": "gallery",
                    "Gallery:S3:AccessKey": key,
                    "Gallery:S3:SecretKey": secret,
                }
                pg.command([dotnet, "user-secrets", "set", "--project", str(project)],
                           env=env, data=json.dumps(config_values))
                env["SSL_CERT_FILE"] = str(d / "ca.crt")
                logpath = root / f"{mode}-ingest.log"
                with logpath.open("w") as log:
                    process = subprocess.Popen(
                        [
                            str(args.dotnet.resolve()),
                            str(isoplane),
                            "dev",
                            "--no-build",
                            "--project",
                            str(project),
                            "--configuration",
                            args.configuration,
                            "--no-watch",
                            "--port",
                            "0",
                            "--json",
                            "--shutdown-on-stdin-close",
                        ],
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=log,
                        env=env,
                        text=True,
                    )
                    try:
                        with selectors.DefaultSelector() as selector:
                            selector.register(process.stdout, selectors.EVENT_READ)
                            if not selector.select(900):
                                raise RuntimeError("Guest readiness exceeded its deadline; inspect " + str(logpath))
                            line = process.stdout.readline()
                        if not line:
                            raise RuntimeError("Guest exited before readiness; inspect " + str(logpath))
                        ready = json.loads(line)
                        assert ready["event"] == "ready", "The CLI must report readiness before requests"
                        url = ready["url"]
                        verify_image(url, container, s3, (root / "original.jpg").read_bytes())
                        print(
                            mode
                            + ": real guest EXIF/resize/S3/verified-TLS PostgreSQL ingest and retrieval passed",
                            flush=True,
                        )
                    except Exception:
                        print(logpath.read_text(), flush=True)
                        raise
                    finally:
                        process.stdin.close()
                        try:
                            process.wait(timeout=30)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                            raise
                pg.command(["docker", "exec", container, "psql", "-U", "postgres", "-d", "todo", "-v", "ON_ERROR_STOP=1",
                            "-c", 'ALTER TABLE "GalleryImages" RENAME TO "UnavailableGalleryImages"'])
                config_values["Gallery:ThumbnailEdge"] = "128"
                pg.command([dotnet, "user-secrets", "set", "--project", str(project)],
                           env=env, data=json.dumps(config_values))
                try:
                    with pg.application(dotnet, project, mode, env, root, "-missing-table"):
                        raise AssertionError("Gallery preparation must run its read query before reporting readiness")
                except pg.StartupRejected:
                    diagnostic = (root / f"{mode}-missing-table.log").read_text()
                    assert "initialization failed" in diagnostic and "Npgsql.PostgresException" in diagnostic, \
                        "Preparation must report the database failure from its read warmup"
                    database_log = subprocess.run(["docker", "logs", container], capture_output=True, text=True,
                                                  check=True, timeout=30)
                    assert 'relation "GalleryImages" does not exist' in database_log.stdout + database_log.stderr, \
                        "PostgreSQL must observe the initialization query against the missing gallery table"
                print(mode + ": gallery read warmup rejects a missing database table before readiness", flush=True)
                for value in [password, key, secret]:
                    assert (
                        all(value not in path.read_text() for path in root.glob("*.log"))
                    ), "Credentials must not appear in guest logs"
