"""PostgreSQL behavior using a package prepared by Task."""
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from common import application, command, database, prepared_project, StartupRejected, verify_crud


class PostgreSql(unittest.TestCase):
    def test_guest_crud_uses_verified_tls_without_disclosing_secrets(self):
        arguments = SimpleNamespace(
            cli=Path(os.environ['ISOPLANE_TEST_CLI_ASSEMBLY']),
            dotnet=Path(os.environ['ISOPLANE_TEST_DOTNET']),
            sample=Path(os.environ['ISOPLANE_TEST_SAMPLE']),
            configuration='Release')
        scratch = Path(__file__).resolve().parents[2] / '.test-runs/postgres'
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='run-', dir=scratch) as directory:
            execute(arguments, Path(directory))


def execute(arguments, records):
    print(f'Records: {records}', flush=True)
    with tempfile.TemporaryDirectory(prefix='application-', dir=records) as work:
        directory = Path(work)
        project = directory / 'app'
        prepared_project(arguments.sample, project)
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
            configuration = arguments.configuration
            trusted = env | {'SSL_CERT_FILE': str(directory / 'ca.crt')}
            with application(arguments.dotnet.resolve(), project, configuration, trusted, records) as address:
                assert password.encode() not in (project / '.isoplane/package/application.tar').read_bytes(), 'runtime secrets must not be embedded in the application package'
                verify_crud(address, container)
            try:
                with application(arguments.dotnet.resolve(), project, configuration, env, records, "-untrusted"):
                    raise AssertionError('database warmup must refuse startup without the trusted root')
            except StartupRejected:
                pass
            rejected = (records / f'{configuration}-untrusted.log').read_text()
            assert 'System.Security.Authentication.AuthenticationException' in rejected, 'removing trust must fail TLS authentication, not merely return an unrelated server error'
            assert 'outbound operation denied' not in rejected, 'the untrusted-root test must reach the database'
            print(f'{configuration}: verified-TLS CRUD and untrusted-root rejection passed', flush=True)
            for record in records.glob('*.log'):
                assert password not in record.read_text(), 'development credentials must not appear in build or runtime logs'
