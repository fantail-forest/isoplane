"""Exercise installed tools, User Secrets and verified-TLS PostgreSQL."""

from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import selectors
import shutil
import subprocess
import time
import urllib.error
import urllib.request
import uuid
import sys


def command(arguments, *, env=None, cwd=None, data=None, timeout=120):
    result = subprocess.run(arguments, input=data, text=True, capture_output=True,
                            env=env, cwd=cwd, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} failed ({result.returncode}): {result.stderr}")
    return result.stdout.strip()


@contextmanager
def database(directory):
    command(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
             '-keyout', str(directory / 'ca.key'), '-out', str(directory / 'ca.crt'),
             '-days', '1', '-subj', '/CN=Isoplane local development test CA',
             '-addext', 'basicConstraints=critical,CA:TRUE'])
    command(['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
             '-keyout', str(directory / 'server.key'), '-out', str(directory / 'server.csr'),
             '-subj', '/CN=127.0.0.1'])
    (directory / 'server.ext').write_text('subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')
    command(['openssl', 'x509', '-req', '-in', str(directory / 'server.csr'),
             '-CA', str(directory / 'ca.crt'), '-CAkey', str(directory / 'ca.key'),
             '-CAcreateserial', '-out', str(directory / 'server.crt'), '-days', '1',
             '-extfile', str(directory / 'server.ext')])
    password = uuid.uuid4().hex
    credentials = directory / 'postgres.env'
    credentials.write_text(f'POSTGRES_PASSWORD={password}\nPOSTGRES_DB=todo\n')
    credentials.chmod(0o600)
    container = command(['docker', 'run', '--detach', '--rm', '--name', 'isoplane-dx-pg-' + uuid.uuid4().hex,
                         '--label', 'isoplane.test=developer-experience', '--publish', '127.0.0.1::5432',
                         '--env-file', str(credentials),
                         '--mount', f'type=bind,source={directory},target=/certificates,readonly',
                         '--entrypoint', 'bash', 'postgres:16', '-ceu',
                         'cp /certificates/server.key /tmp/server.key; '
                         'cp /certificates/server.crt /tmp/server.crt; '
                         'chown postgres:postgres /tmp/server.key /tmp/server.crt; '
                         'chmod 600 /tmp/server.key; '
                         'exec docker-entrypoint.sh postgres -c ssl=on '
                         '-c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key'])
    try:
        port = int(command(['docker', 'port', container, '5432/tcp']).rsplit(':', 1)[1])
        deadline = time.monotonic() + 60
        while subprocess.run(['docker', 'exec', container, 'pg_isready', '-h', '127.0.0.1', '-U', 'postgres', '-d', 'todo'],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode:
            if time.monotonic() >= deadline:
                raise RuntimeError('PostgreSQL did not become ready before its startup deadline')
            time.sleep(0.1)
        command(['docker', 'exec', container, 'bash', '-ceu',
                 "printf 'local all all trust\\nhostnossl all all all reject\\nhostssl all all all scram-sha-256\\n' > /var/lib/postgresql/data/pg_hba.conf"])
        command(['docker', 'exec', container, 'psql', '-U', 'postgres', '-d', 'todo', '-c', 'SELECT pg_reload_conf()'])
        refused = subprocess.run(['docker', 'exec', container, 'psql',
                                 'host=127.0.0.1 user=postgres dbname=todo sslmode=disable', '-c', 'SELECT 1'],
                                capture_output=True, text=True, timeout=30)
        assert refused.returncode != 0 and 'no encryption' in refused.stderr, 'the database must refuse unencrypted connections'
        yield container, port, password
    finally:
        command(['docker', 'rm', '--force', container])
        credentials.unlink()


def rows(container):
    query = '''SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM
        (SELECT "Id" AS id, "Title" AS title, "Done" AS done FROM "Todos" ORDER BY "Id") t'''
    return json.loads(command(['docker', 'exec', container, 'psql', '-U', 'postgres', '-d', 'todo', '-Atc', query]))


def request(address, method, path, value=None):
    outgoing = urllib.request.Request(f'{address}{path}', method=method,
                                     data=None if value is None else json.dumps(value).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(outgoing, timeout=60) as response:
        body = response.read()
        return response.status, json.loads(body) if body else None


def verify_crud(address, container):
    assert request(address, 'GET', '/todos') == (200, []), 'the guest must read the real empty database'
    status, created = request(address, 'POST', '/todos', {'title': 'installed guest', 'done': False})
    assert status == 201 and created['title'] == 'installed guest', 'the guest must create a TODO'
    assert rows(container) == [created], 'the independent SQL client must observe the inserted row'
    status, updated = request(address, 'PUT', f"/todos/{created['id']}", {'title': 'updated ✓', 'done': True})
    assert status == 200 and updated['done'] and updated['title'] == 'updated ✓', 'the guest must update the row'
    assert rows(container) == [updated], 'the independent SQL client must observe the update'
    assert request(address, 'DELETE', f"/todos/{created['id']}") == (204, None), 'the guest must delete the row'
    assert rows(container) == [], 'the independent SQL client must observe the deletion'


class StartupRejected(RuntimeError):
    """The CLI refused preparation and exited before reporting readiness."""


@contextmanager
def application(dotnet, project, configuration, env, records, suffix=""):
    records = records.resolve()
    with (records / f'{configuration}{suffix}.log').open('w') as errors:
        process = subprocess.Popen([str(dotnet), 'run', '--project', str(project), '-c', configuration,
                                    '--no-build', '--', '--port', '0', '--json', '--shutdown-on-stdin-close'],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors, env=env, text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(600):
                    raise RuntimeError('Application startup exceeded its deadline; inspect the runtime log')
                line = process.stdout.readline()
            if not line:
                status = process.wait(timeout=30)
                assert status != 0, 'the CLI must not exit successfully without reporting readiness'
                raise StartupRejected(f'The installed CLI rejected startup with status {status}: {(records / f"{configuration}{suffix}.log").read_text()}')
            ready = json.loads(line)
            assert ready['event'] == 'ready', 'the CLI must report readiness before requests'
            yield ready['url']
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=30)
                raise RuntimeError('The CLI failed to stop after its owner closed input')
            process.stdout.close()
            if process.returncode != 0:
                if failure := sys.exception():
                    failure.add_note(f'The installed CLI exited with status {process.returncode}.')
                else:
                    raise AssertionError('the installed CLI must stop cleanly')


def prepared_project(source, destination):
    """Isolate mutable scenario settings while reusing prepared compiler outputs."""
    if not (source / 'bin').is_dir() or not (source / 'obj/project.assets.json').is_file():
        raise FileNotFoundError(f'Prepare {source} with task scenarios:postgres')
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns('bin', 'obj', 'out', '.isoplane'))
    (destination / 'bin').symlink_to((source / 'bin').resolve(), target_is_directory=True)
    (destination / 'obj').mkdir()
    for pattern in ('project.assets.json', '*.nuget.g.props', '*.nuget.g.targets'):
        for metadata in (source / 'obj').glob(pattern):
            shutil.copy2(metadata, destination / 'obj' / metadata.name)
