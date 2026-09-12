"""The customer reference is generated, scoped, and checked against actual assemblies."""

from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PROJECT = ROOT / 'tools/ApiReference/ApiReference.csproj'


class SdkReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run(['dotnet', 'build', str(PROJECT), '--nologo'],
                                cwd=ROOT, capture_output=True, text=True, timeout=120)
        if result.returncode:
            raise AssertionError('The reference generator must build against current SDK sources:\n' + result.stdout + result.stderr)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='wisp-sdk-reference-test-')
        self.addCleanup(self.temporary.cleanup)
        self.output = Path(self.temporary.name)
        result = self.generate()
        self.assertEqual(result.returncode, 0, 'Reference generation must succeed: ' + result.stderr)

    def generate(self, check=False):
        command = ['dotnet', 'run', '--no-build', '--project', str(PROJECT), '--', str(self.output)]
        if check:
            command.append('--check')
        return subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=120)

    def test_generated_reference_matches_the_current_public_api(self):
        result = self.generate(check=True)
        self.assertEqual(result.returncode, 0, 'A newly generated reference must pass its drift check: ' + result.stderr)
        handler = self.output / 'sdk/Wisp.Sdk/Wisp.Durable/IWispHandler/HandleAsync.md'
        self.assertIn('Task<Response> HandleAsync(Request req, Ctx ctx)', handler.read_text(),
                      'The ordinary HTTP handler must remain documented despite its Durable namespace')
        configuration = self.output / 'sdk/Isoplane.Testing/Wisp.Testing/ApplicationOptions/Configuration.md'
        self.assertIn('get; init;', configuration.read_text(), 'The reference must preserve init-only API setters')

    def test_changed_or_missing_generated_pages_fail_the_check(self):
        page = self.output / 'sdk/Wisp.AspNetCore/Wisp.AspNetCore/WispHosting.md'
        page.write_text('stale documentation\n')
        changed = self.generate(check=True)
        self.assertEqual(changed.returncode, 1, 'Modified reference content must fail the drift check')
        self.assertIn('WispHosting.md', changed.stderr, 'The drift diagnostic must identify the stale API page')
        page.unlink()
        missing = self.generate(check=True)
        self.assertEqual(missing.returncode, 1, 'Missing generated API pages must fail the drift check')

    def test_retired_generated_pages_fail_the_check(self):
        retired = self.output / 'sdk/retired.md'
        retired.write_text('retired API\n')
        result = self.generate(check=True)
        self.assertEqual(result.returncode, 1, 'A retired page must not silently remain in the customer reference')

    def test_di_and_interop_are_excluded_without_hiding_the_http_handler(self):
        pages = [str(path.relative_to(self.output)) for path in self.output.rglob('*.md')]
        self.assertFalse(any('Wisp.Abi' in path or '/DurableInstance' in path or '/FromDispatch.md' in path for path in pages),
                         'Internal interop and DI APIs must not be presented as customer preview APIs')
        context = (self.output / 'sdk/Wisp.Sdk/Wisp/Ctx.md').read_text()
        self.assertNotIn('Fencing', context, 'The DI-only fencing member must be outside this reference')
        self.assertNotIn('Outbox', context, 'The DI-only outbox member must be outside this reference')
        self.assertTrue(any('/IWispHandler.md' in path for path in pages), 'The HTTP handler contract must remain discoverable')


if __name__ == '__main__':
    unittest.main()
