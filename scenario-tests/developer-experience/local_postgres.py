"""Validate the packaged PostgreSQL helper; requires installed dotnet, Docker and OpenSSL."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


class Sample:
    def __init__(self, release, directory):
        self.project = directory / 'Todo'
        shutil.copytree(release / 'samples/aspnet-todo', self.project)
        self.state_path = self.project / '.local-postgres.json'
        self.env = os.environ.copy()
        self.env['APPDATA'] = str(directory / 'settings')
        self.env['TMPDIR'] = str(directory / 'temporary')
        Path(self.env['TMPDIR']).mkdir()
        self.run(['dotnet', 'nuget', 'add', 'source', str(release / 'packages'),
                  '--name', 'isoplane-preview', '--configfile', 'nuget.config'])

    def run(self, arguments, *, data=None, success=True, env=None):
        result = subprocess.run(arguments, cwd=self.project, env=env or self.env,
                                input=data, text=True, capture_output=True, timeout=150)
        if success and result.returncode:
            raise RuntimeError(f'{arguments[0]} failed: {result.stderr}')
        return result

    def helper(self, action, **kwargs):
        return self.run([sys.executable, 'local-postgres.py', action], **kwargs)

    def assert_removed(self, state):
        assert not Path(state['directory']).exists(), 'temporary credentials must be removed'
        assert not self.state_path.exists(), 'cleanup must remove the recorded state'
        assert self.run(['docker', 'inspect', state['container']], success=False).returncode != 0, \
            'cleanup must remove the owned database container'


def verifies_tls_and_cleanup(sample):
    started = sample.helper('start')
    state = json.loads(sample.state_path.read_text())
    directory = Path(state['directory'])
    password = (directory / 'postgres.env').read_text().splitlines()[0].split('=', 1)[1]
    assert password not in started.stdout + started.stderr, 'setup must not print the password'
    assert directory.stat().st_mode & 0o777 == 0o700, 'temporary credentials must be private'
    assert (directory / 'ca.key').stat().st_mode & 0o077 == 0, 'the CA private key must be private'
    binding = sample.run(['docker', 'port', state['container'], '5432/tcp']).stdout.strip()
    assert binding.startswith('127.0.0.1:') and '\n' not in binding, 'database must bind only loopback'
    client = ['docker', 'exec', '-i', state['container'], 'bash', '-ceu',
              'read -r PGPASSWORD; export PGPASSWORD; psql "$@"', '--']
    connection = 'host=127.0.0.1 dbname=todo user=postgres sslmode=verify-full sslrootcert=/certificates/ca.crt'
    query = ['-Atc', 'select count(*) from "Todos";']
    assert sample.run(client + [connection] + query, data=password + '\n').stdout.strip() == '0', \
        'verified TLS must reach the initialized empty schema'
    cases = [
        (connection.replace('sslmode=verify-full', 'sslmode=disable'), password, 'plaintext'),
        (connection, 'wrong-password', 'incorrect credentials'),
        (connection.replace('/certificates/ca.crt', '/etc/ssl/certs/ca-certificates.crt'), password, 'untrusted CA'),
    ]
    for configuration, credential, reason in cases:
        result = sample.run(client + [configuration] + query, data=credential + '\n', success=False)
        assert result.returncode != 0, f'the database must reject {reason}'
    assert sample.helper('start', success=False).returncode != 0, 'repeated start must refuse existing ownership'
    altered = dict(state, owner='another-owner')
    sample.state_path.write_text(json.dumps(altered))
    try:
        assert sample.helper('stop', success=False).returncode != 0, 'stop must refuse mismatched ownership'
        sample.run(['docker', 'inspect', state['container']])
    finally:
        sample.state_path.write_text(json.dumps(state))
    sample.run(['dotnet', 'user-secrets', 'set', 'Unrelated', 'preserved'])
    sample.helper('stop')
    sample.assert_removed(state)
    sample.helper('stop')
    sample.run(['dotnet', 'user-secrets', 'remove', 'ConnectionStrings:Todo'])
    listed = sample.run(['dotnet', 'user-secrets', 'list']).stdout
    assert 'Unrelated = preserved' in listed, 'cleanup must preserve unrelated User Secrets'
    assert 'ConnectionStrings:Todo' not in listed, 'documented cleanup must remove the database setting'


def failed_setup_cleans_resources(sample, directory):
    stubs = directory / 'tools'
    stubs.mkdir()
    executable = stubs / 'dotnet'
    receipt = directory / 'failed-state.json'
    executable.write_text('#!/bin/sh\ncp .local-postgres.json "$FAILURE_RECEIPT"\n'
                          'echo "intentional User Secrets failure" >&2\nexit 42\n')
    executable.chmod(0o700)
    env = dict(sample.env, PATH=str(stubs) + os.pathsep + sample.env['PATH'], FAILURE_RECEIPT=str(receipt))
    result = sample.helper('start', env=env, success=False)
    assert result.returncode != 0 and 'intentional User Secrets failure' in result.stderr, \
        'the helper must surface failed User Secrets setup'
    sample.assert_removed(json.loads(receipt.read_text()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', required=True, type=Path)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='wisp-local-postgres-scenario-') as temporary:
        directory = Path(temporary)
        sample = Sample(arguments.release.resolve(), directory)
        try:
            verifies_tls_and_cleanup(sample)
            failed_setup_cleans_resources(sample, directory)
        finally:
            if sample.state_path.exists():
                sample.helper('stop')
    print('PASS: packaged helper TLS, schema, authentication, loopback binding, ownership and cleanup')


if __name__ == '__main__':
    main()
