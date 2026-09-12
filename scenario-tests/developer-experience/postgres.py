"""Exercise installed tools, User Secrets and verified-TLS PostgreSQL in both guest modes."""

import argparse
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import selectors
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import xml.etree.ElementTree as ET
import os
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
             '-days', '1', '-subj', '/CN=WISP local development test CA',
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
    container = command(['docker', 'run', '--detach', '--rm', '--name', 'wisp-dx-pg-' + uuid.uuid4().hex,
                         '--label', 'wisp.test=developer-experience', '--publish', '127.0.0.1::5432',
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
        assert response.headers.get('X-Todo-App') == 'host-server', 'ASP.NET response callbacks must execute'
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
                raise RuntimeError('The installed CLI exited before readiness; inspect the runtime log')
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
            if process.returncode != 0:
                if failure := sys.exception():
                    failure.add_note(f'The installed CLI exited with status {process.returncode}.')
                else:
                    raise AssertionError('the installed CLI must stop cleanly')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--dotnet', type=Path, required=True)
    parser.add_argument('--package-feed', type=Path, required=True)
    parser.add_argument('--records', type=Path, default=None)
    parser.add_argument('--sample', type=Path, default=Path(__file__).resolve().parents[2] / 'samples/aspnet-todo')
    parser.add_argument('--configuration', action='append', choices=['WispInterpretedDebug', 'WispNativeAotRelease'])
    arguments = parser.parse_args()
    records = arguments.records or Path(tempfile.mkdtemp(prefix='wisp-dx-postgres-records-'))
    records.mkdir(parents=True, exist_ok=True)
    print(f'Records: {records}', flush=True)
    with tempfile.TemporaryDirectory(prefix='wisp-dx-postgres-') as work:
        directory = Path(work)
        project = directory / 'app'
        shutil.copytree(arguments.sample, project, ignore=shutil.ignore_patterns('bin', 'obj', 'out'))
        config = ET.parse(project / 'nuget.config')
        ET.SubElement(config.getroot().find('packageSources'), 'add',
                      {'key': 'local-preview', 'value': str(arguments.package_feed.resolve())})
        settings = ET.SubElement(config.getroot(), 'config')
        ET.SubElement(settings, 'add', {'key': 'globalPackagesFolder', 'value': str(directory / 'packages')})
        config.write(project / 'nuget.config')
        env = os.environ.copy()
        env['APPDATA'] = str(directory / 'user-secrets')
        env['PATH'] = str(arguments.cli.resolve().parent) + os.pathsep + env['PATH']
        env['DOTNET_ROOT'] = str(arguments.dotnet.resolve().parent)
        dotnet = str(arguments.dotnet.resolve())
        command([dotnet, 'user-secrets', 'init', '--project', str(project)], env=env)
        print(f'Working directory: {directory}', flush=True)
        with database(directory) as (container, port, password):
            command(['docker', 'exec', '-i', container, 'psql', '-U', 'postgres', '-d', 'todo', '-v', 'ON_ERROR_STOP=1'],
                    data=(project / 'schema.sql').read_text())
            connection = f'Host=127.0.0.1;Port={port};Database=todo;Username=postgres;Password={password}'
            command([dotnet, 'user-secrets', 'set', '--project', str(project)], env=env,
                    data=json.dumps({'ConnectionStrings:Todo': connection}))
            manifest = project / 'wisp.toml'
            base = manifest.read_text().replace('127.0.0.1:5432', f'127.0.0.1:{port}')
            trust = f'\n[dev.trust]\nbundle = {json.dumps(str(directory / "ca.crt"))}\nsha256 = "{hashlib.sha256((directory / "ca.crt").read_bytes()).hexdigest()}"\n'
            for configuration in arguments.configuration or ['WispInterpretedDebug', 'WispNativeAotRelease']:
                manifest.write_text(base + trust)
                with (records / f'{configuration}-build.log').open('w') as log:
                    subprocess.run([dotnet, 'build', '-c', configuration, '--disable-build-servers'],
                                   cwd=project, env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=900)
                with application(arguments.dotnet.resolve(), project, configuration, env, records) as address:
                    shutil.copy2(project / 'out/Todo.wasm', records / f'{configuration}.wasm')
                    verify_crud(address, container)
                manifest.write_text(base)
                with application(arguments.dotnet.resolve(), project, configuration, env, records, "-untrusted") as address:
                    try:
                        request(address, 'GET', '/todos')
                    except urllib.error.HTTPError as error:
                        assert error.code == 500, 'an untrusted database certificate must fail the request'
                    else:
                        raise AssertionError('the guest accepted a database certificate without its trusted root')
                rejected = (records / f'{configuration}-untrusted.log').read_text()
                assert 'System.Security.Authentication.AuthenticationException' in rejected, 'removing trust must fail TLS authentication, not merely return an unrelated server error'
                assert 'outbound operation denied' not in rejected, 'the untrusted-root test must reach the database'
                print(f'{configuration}: verified-TLS CRUD and untrusted-root rejection passed', flush=True)
            for record in records.glob('*.log'):
                assert password not in record.read_text(), 'development credentials must not appear in build or runtime logs'
            for module in records.glob('*.wasm'):
                assert password.encode() not in module.read_bytes(), 'runtime secrets must not be embedded in the published guest'


if __name__ == '__main__':
    main()
