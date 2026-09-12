#!/usr/bin/env python3
"""Start or stop a disposable TLS PostgreSQL server for this sample."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import tempfile
import time

PROJECT = Path(__file__).resolve().parent
STATE = PROJECT / '.local-postgres.json'
LABEL = 'isoplane.sample.owner'


def run(arguments, *, data=None):
    result = subprocess.run(arguments, input=data, text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise RuntimeError(f'{arguments[0]} failed: {result.stderr.strip()}')
    return result.stdout.strip()


def remove(state):
    found = subprocess.run(['docker', 'inspect', '--format', '{{json .Config.Labels}}', state['container']],
                           text=True, capture_output=True, timeout=30)
    if found.returncode == 0:
        labels = json.loads(found.stdout)
        if labels.get(LABEL) != state['owner']:
            raise RuntimeError('Container ownership differs; refusing to remove it.')
        run(['docker', 'rm', '--force', '--volumes', state['container']])
    elif 'No such object' not in found.stderr and 'No such container' not in found.stderr:
        raise RuntimeError(f'Docker could not inspect the sample container: {found.stderr.strip()}')
    directory = Path(state['directory'])
    if directory.name != 'isoplane-postgres-' + state['owner'] or directory.is_symlink():
        raise RuntimeError('The recorded sample directory is invalid; refusing to remove it.')
    if directory.exists():
        shutil.rmtree(directory)
    STATE.unlink()


def start():
    if STATE.exists():
        raise RuntimeError('A local database is already recorded. Stop it before starting another.')
    for tool in ['docker', 'openssl', 'dotnet']:
        if not shutil.which(tool):
            raise RuntimeError(f'{tool} is required for the optional local database.')
    os.umask(0o077)
    owner = secrets.token_hex(12)
    directory = Path(tempfile.gettempdir()) / ('isoplane-postgres-' + owner)
    directory.mkdir(mode=0o700)
    state = {'owner': owner, 'container': 'isoplane-todo-' + owner, 'directory': str(directory)}
    try:
        with open(STATE, 'x', opener=lambda path, flags: os.open(path, flags, 0o600)) as output:
            json.dump(state, output)
    except OSError:
        directory.rmdir()
        raise
    try:
        run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
             '-keyout', str(directory / 'ca.key'), '-out', str(directory / 'ca.crt'),
             '-days', '7', '-subj', '/CN=Isoplane local sample CA',
             '-addext', 'basicConstraints=critical,CA:TRUE'])
        run(['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
             '-keyout', str(directory / 'server.key'), '-out', str(directory / 'server.csr'),
             '-subj', '/CN=127.0.0.1'])
        (directory / 'server.ext').write_text('subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')
        run(['openssl', 'x509', '-req', '-in', str(directory / 'server.csr'),
             '-CA', str(directory / 'ca.crt'), '-CAkey', str(directory / 'ca.key'),
             '-CAcreateserial', '-out', str(directory / 'server.crt'), '-days', '7',
             '-extfile', str(directory / 'server.ext')])
        password = secrets.token_hex(24)
        credentials = directory / 'postgres.env'
        credentials.write_text(f'POSTGRES_PASSWORD={password}\nPOSTGRES_DB=todo\n')
        credentials.chmod(0o600)
        run(['docker', 'run', '--detach', '--rm', '--name', state['container'],
             '--label', f'{LABEL}={owner}', '--publish', '127.0.0.1::5432',
             '--env-file', str(credentials),
             '--mount', f'type=bind,source={directory},target=/certificates,readonly',
             '--entrypoint', 'bash', 'postgres:16', '-ceu',
             'cp /certificates/server.key /tmp/server.key; '
             'cp /certificates/server.crt /tmp/server.crt; '
             'chown postgres:postgres /tmp/server.key /tmp/server.crt; '
             'chmod 600 /tmp/server.key; '
             'exec docker-entrypoint.sh postgres -c ssl=on '
             '-c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key'])
        deadline = time.monotonic() + 60
        while subprocess.run(['docker', 'exec', state['container'], 'pg_isready', '-h', '127.0.0.1',
                             '-U', 'postgres', '-d', 'todo'], capture_output=True, timeout=10).returncode:
            if time.monotonic() >= deadline:
                raise RuntimeError('PostgreSQL did not become ready before the startup deadline.')
            time.sleep(0.1)
        run(['docker', 'exec', state['container'], 'bash', '-ceu',
             "printf 'local all all trust\\nhostnossl all all all reject\\nhostssl all all all scram-sha-256\\n' > /var/lib/postgresql/data/pg_hba.conf"])
        sql = ['docker', 'exec', '-i', state['container'], 'psql', '-U', 'postgres', '-d', 'todo', '-v', 'ON_ERROR_STOP=1']
        run(sql, data='SELECT pg_reload_conf();\n' + (PROJECT / 'schema.sql').read_text())
        port = int(run(['docker', 'port', state['container'], '5432/tcp']).rsplit(':', 1)[1])
        connection = f'Host=127.0.0.1;Port={port};Database=todo;Username=postgres;Password={password}'
        run(['dotnet', 'user-secrets', 'init', '--project', str(PROJECT)])
        run(['dotnet', 'user-secrets', 'set', '--project', str(PROJECT)],
            data=json.dumps({'ConnectionStrings:Todo': connection}))
        ca = directory / 'ca.crt'
        print('Created disposable PostgreSQL and set ConnectionStrings:Todo in User Secrets.')
        print('Replace allow_endpoints inside [dev] in wisp.toml with:')
        print(f'allow_endpoints = ["127.0.0.1:{port}"]')
        print('Add this separate table at the end of wisp.toml:')
        print(f'\n[dev.trust]\nbundle = {json.dumps(str(ca))}\nsha256 = "{hashlib.sha256(ca.read_bytes()).hexdigest()}"')
        print('\nStop the application before running: python3 local-postgres.py stop')
    except BaseException as error:
        try:
            remove(state)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as cleanup_error:
            raise RuntimeError(f'{error}; cleanup also failed: {cleanup_error}. Run stop again.') from error
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['start', 'stop'])
    arguments = parser.parse_args()
    if arguments.action == 'start':
        start()
    elif STATE.exists():
        remove(json.loads(STATE.read_text()))
        print('Removed the sample database and temporary certificates.')
        print('Remove its setting with: dotnet user-secrets remove ConnectionStrings:Todo')
        print('Remove [dev.trust] and the local endpoint from wisp.toml before using another database.')
    else:
        print('No local sample database is recorded.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error)) from error
