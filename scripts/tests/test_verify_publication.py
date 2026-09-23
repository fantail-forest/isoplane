"""Publication checks exercise the complete verifier with small package archives."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('publication', Path(__file__).parents[1] / 'verify-publication.py')
publication = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publication)


class Objects:
    def __init__(self):
        self.objects = {}

    def put(self, key, data):
        self.objects[key] = data
        return hashlib.sha256(data).hexdigest()

    def fetch(self, key, output, limit):
        output.write(self.objects[key])

    def exists(self, key):
        if key not in self.objects:
            raise ValueError('missing toolchain')


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / 'verified'
        self.version = '0.1.0-preview.3'
        self.objects = Objects()
        self.manifest = {'version': self.version, 'sourceRevision': 'a' * 40, 'packages': {}, 'releases': {}}
        for rid in publication.PLATFORMS:
            prefix = f'isoplane-{rid}-{self.version}'
            archive_hash = self.objects.put(prefix + '/toolchain.tgz', b'toolchain')
            release = {'packageVersion': self.version, 'runtimeIdentifier': rid, 'sourceRevision': 'a' * 40,
                       'toolchain': {'archive': 'toolchain.tgz', 'archiveSha256': archive_hash}}
            self.manifest['releases'][rid] = self.objects.put(prefix + '/release.json', json.dumps(release).encode())
        for package_id in publication.package_ids():
            self.manifest['packages'][package_id] = self.objects.put(self.package_key(package_id), self.package(package_id))

    def package_key(self, package_id):
        return f'nuget/{self.version}/{package_id}.{self.version}.nupkg'

    def package(self, package_id):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, 'w') as archive:
            archive.writestr('package.nuspec', f'<package><metadata><id>{package_id}</id><version>{self.version}</version></metadata></package>')
        return stream.getvalue()

    def verify(self, expected=None):
        actual = self.objects.put(f'publication/{self.version}/manifest.json', json.dumps(self.manifest).encode())
        publication.verify(self.version, expected or actual, self.directory, self.objects)

    def test_verified_packages_publish_cli_last(self):
        self.verify()
        paths = json.loads((self.directory / 'packages.json').read_text())
        self.assertEqual(len(paths), len(publication.package_ids()), 'every package must be staged')
        self.assertTrue(Path(paths[-1]).name.startswith('Isoplane.Cli.'), 'CLI must publish after its dependencies')

    def test_changed_manifest_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
            self.verify('0' * 64)

    def test_changed_package_is_rejected(self):
        self.objects.objects[self.package_key('Isoplane.Sdk')] = b'changed'
        with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
            self.verify()
        self.assertFalse((self.directory / 'packages.json').exists(), 'failure must not authorize any pushes')

    def test_wrong_nuspec_identity_is_rejected(self):
        self.manifest['packages']['Isoplane.Sdk'] = self.objects.put(self.package_key('Isoplane.Sdk'), self.package('Isoplane.Wrong'))
        with self.assertRaisesRegex(ValueError, 'identity mismatch'):
            self.verify()

    def test_missing_package_is_rejected(self):
        del self.manifest['packages']['Isoplane.Native']
        with self.assertRaisesRegex(ValueError, 'package set mismatch'):
            self.verify()

    def test_missing_platform_is_rejected(self):
        del self.manifest['releases']['osx-arm64']
        with self.assertRaisesRegex(ValueError, 'platform releases'):
            self.verify()

    def test_mixed_source_revisions_are_rejected(self):
        self.manifest['sourceRevision'] = 'b' * 40
        with self.assertRaisesRegex(ValueError, 'release identity mismatch'):
            self.verify()

    def test_absent_toolchain_is_rejected(self):
        del self.objects.objects[f'isoplane-win-x64-{self.version}/toolchain.tgz']
        with self.assertRaisesRegex(ValueError, 'missing toolchain'):
            self.verify()

    def test_path_in_version_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'invalid release version'):
            publication.verify('../escape', '0' * 64, self.directory, self.objects)


class DownloadTransportTests(unittest.TestCase):
    def setUp(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        from unittest.mock import patch

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == '/redirect':
                    self.send_response(302)
                    self.send_header('Location', '/artifact')
                    self.end_headers()
                    return
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'package bytes')

            def do_HEAD(self):
                self.send_response(200 if self.path == '/artifact' else 404)
                self.end_headers()

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join)
        self.addCleanup(server.shutdown)
        patched = patch.object(publication, 'ORIGIN', f'http://127.0.0.1:{server.server_port}')
        patched.start()
        self.addCleanup(patched.stop)
        self.downloads = publication.Downloads()

    def test_transport_downloads_original_bytes(self):
        output = io.BytesIO()
        self.downloads.fetch('artifact', output, 100)
        self.assertEqual(output.getvalue(), b'package bytes', 'transport must preserve package bytes')
        self.downloads.exists('artifact')

    def test_transport_refuses_redirects(self):
        from urllib.error import HTTPError
        with self.assertRaises(HTTPError) as result:
            self.downloads.fetch('redirect', io.BytesIO(), 100)
        result.exception.close()
        self.assertEqual(result.exception.code, 302, 'artifact redirects must not escape the configured origin')

    def test_transport_enforces_download_limit(self):
        with self.assertRaisesRegex(ValueError, 'download limit'):
            self.downloads.fetch('artifact', io.BytesIO(), 2)

    def test_transport_rejects_absent_toolchains(self):
        from urllib.error import HTTPError
        with self.assertRaises(HTTPError) as result:
            self.downloads.exists('missing')
        result.exception.close()
        self.assertEqual(result.exception.code, 404, 'missing assets must block publication')
