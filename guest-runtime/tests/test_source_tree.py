import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'source-tree.py'


class SourceTreeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='isoplane-source-test-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.tree = self.root / 'source'
        self.archive = self.root / 'source.tar.gz'
        self.patches = self.root / 'patches'
        self.patches.mkdir()
        self.write_archive('old\n')
        self.write_patch('first')

    def write_archive(self, value, extra=None):
        with tarfile.open(self.archive, 'w:gz') as archive:
            root = tarfile.TarInfo('runtime')
            root.type = tarfile.DIRTYPE
            archive.addfile(root)
            files = {'value.txt': value, **(extra or {})}
            for name, content in files.items():
                data = content.encode()
                member = tarfile.TarInfo('runtime/' + name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        self.sha = hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def write_patch(self, value):
        (self.patches / '001.patch').write_text(
            'diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n'
            f'@@ -1 +1 @@\n-old\n+{value}\n')

    def run_source(self, action='materialize', env=None):
        return subprocess.run(['python3', str(SCRIPT), action, str(self.tree), str(self.archive),
                               self.sha, str(self.patches)], text=True, capture_output=True, env=env, timeout=20)

    def materialize(self):
        result = self.run_source()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_source_commands_do_not_launch_background_git_maintenance(self):
        trace = self.root / 'git-trace.json'
        env = dict(os.environ, GIT_TRACE2_EVENT=str(trace), GIT_CONFIG_COUNT='2',
                   GIT_CONFIG_KEY_0='maintenance.auto', GIT_CONFIG_VALUE_0='true',
                   GIT_CONFIG_KEY_1='gc.auto', GIT_CONFIG_VALUE_1='1')
        result = self.run_source(env=env)
        self.assertEqual(result.returncode, 0, 'source staging must finish without background maintenance racing cleanup: ' + result.stderr)
        children = [json.loads(line) for line in trace.read_text().splitlines()]
        maintenance = [event for event in children if event.get('event') == 'child_start'
                       and any(arg in ['maintenance', 'gc'] for arg in event.get('argv', []))]
        self.assertEqual(maintenance, [], 'temporary source repositories must not spawn detached maintenance writers')

    def test_reconciliation_preserves_lf_despite_windows_checkout_defaults(self):
        self.write_archive('old\n', {'.gitattributes': '* text=auto\n'})
        env = dict(os.environ, GIT_CONFIG_COUNT='2',
                   GIT_CONFIG_KEY_0='core.autocrlf', GIT_CONFIG_VALUE_0='true',
                   GIT_CONFIG_KEY_1='core.eol', GIT_CONFIG_VALUE_1='crlf')
        for value in ('first', 'second'):
            self.write_patch(value)
            result = self.run_source(env=env)
            self.assertEqual(result.returncode, 0, 'source reconciliation must tolerate Windows Git defaults: ' + result.stderr)
            self.assertEqual((self.tree / 'value.txt').read_bytes(), (value + '\n').encode(),
                             'source reconciliation must not convert the pinned LF bytes to CRLF')

    def test_ignored_archive_files_are_tracked_and_local_changes_are_refused(self):
        self.write_archive('old\n', {'.gitignore': '*.rsp\n', 'Directory.Build.rsp': 'upstream'})
        self.materialize()
        tracked = subprocess.check_output(['git', '-C', str(self.tree), 'ls-files'], text=True)
        self.assertIn('Directory.Build.rsp', tracked, 'all pinned archive files must participate in source identity despite upstream ignore patterns')
        (self.tree / 'Directory.Build.rsp').write_text('local edit')
        result = self.run_source('verify')
        self.assertNotEqual(result.returncode, 0, 'an ignored upstream build input must not evade verification after modification')

    def test_newly_tracked_ignored_files_are_preserved_before_reconciliation(self):
        self.write_archive('old\n', {'.gitignore': '*.rsp\n'})
        self.materialize()
        (self.tree / 'Directory.Build.rsp').write_text('previous local input')
        self.write_archive('old\n', {'.gitignore': '*.rsp\n', 'Directory.Build.rsp': 'new upstream'})
        self.materialize()
        saved = list((self.tree / '.git/isoplane-source-backups').glob('*/Directory.Build.rsp'))
        self.assertEqual(len(saved), 1, 'reconciliation must preserve an ignored local input before replacing it')
        self.assertEqual(saved[0].read_text(), 'previous local input', 'the original ignored input must remain recoverable byte for byte')
        self.assertEqual((self.tree / 'Directory.Build.rsp').read_text(), 'new upstream', 'the reconciled source must use the current pinned archive')

    def test_ignored_file_ancestor_is_preserved_before_new_source_directory(self):
        self.write_archive('old\n', {'.gitignore': 'foo\n'})
        self.materialize()
        (self.tree / 'foo').write_text('local ancestor payload')
        self.write_archive('old\n', {'.gitignore': 'foo\n', 'foo/bar.txt': 'upstream', 'foo/other.txt': 'other'})
        self.materialize()
        saved = list((self.tree / '.git/isoplane-source-backups').glob('*/foo'))
        self.assertEqual(len(saved), 1, 'an obstructing ignored ancestor must be backed up once')
        self.assertEqual(saved[0].read_text(), 'local ancestor payload', 'ancestor bytes must remain recoverable')
        self.assertEqual((self.tree / 'foo/bar.txt').read_text(), 'upstream', 'new source directory must be materialized')

    def test_ignored_symlink_ancestor_is_preserved_without_reading_its_target(self):
        self.write_archive('old\n', {'.gitignore': 'foo\n'})
        self.materialize()
        external = self.root / 'outside'
        external.mkdir()
        (external / 'bar.txt').write_text('outside payload')
        (self.tree / 'foo').symlink_to(external, target_is_directory=True)
        self.write_archive('old\n', {'.gitignore': 'foo\n', 'foo/bar.txt': 'upstream'})
        self.materialize()
        saved = list((self.tree / '.git/isoplane-source-backups').glob('*/foo'))
        self.assertEqual(len(saved), 1, 'ignored ancestor symlink must be retained')
        self.assertTrue(saved[0].is_symlink(), 'backup must preserve the symlink rather than its external contents')
        self.assertEqual(os.readlink(saved[0]), str(external), 'original symlink destination must remain recoverable')
        self.assertEqual((external / 'bar.txt').read_text(), 'outside payload', 'reconciliation must not modify external symlink targets')
        self.assertEqual((self.tree / 'foo/bar.txt').read_text(), 'upstream', 'incoming source must replace the local symlink')

    def test_ignored_directory_replacement_is_refused_without_losing_contents(self):
        self.write_archive('old\n', {'.gitignore': 'foo\n'})
        self.materialize()
        (self.tree / 'foo').mkdir()
        (self.tree / 'foo/local.txt').write_text('preserve directory')
        self.write_archive('old\n', {'.gitignore': 'foo\n', 'foo': 'upstream file'})
        result = self.run_source()
        self.assertNotEqual(result.returncode, 0, 'an ignored directory must not be silently replaced by a source file')
        self.assertIn('untracked directory', result.stderr, 'directory obstruction must explain manual preservation')
        self.assertEqual((self.tree / 'foo/local.txt').read_text(), 'preserve directory', 'refusal must retain local directory contents')

    def test_ignored_fifo_ancestor_is_refused_without_opening_it(self):
        self.write_archive('old\n', {'.gitignore': 'foo\n'})
        self.materialize()
        os.mkfifo(self.tree / 'foo')
        self.write_archive('old\n', {'.gitignore': 'foo\n', 'foo/bar.txt': 'upstream'})
        result = self.run_source()
        self.assertNotEqual(result.returncode, 0, 'a special file must not be copied or silently discarded')
        self.assertIn('special file', result.stderr, 'FIFO obstruction must report its unsupported preservation semantics')
        self.assertTrue((self.tree / 'foo').is_fifo(), 'refusal must retain the unopened local FIFO')

    def test_modified_patch_replaces_source_and_preserves_previous_commit(self):
        self.materialize()
        old_head = subprocess.check_output(['git', '-C', str(self.tree), 'rev-parse', 'HEAD'], text=True).strip()
        self.write_patch('second')
        self.assertNotEqual(self.run_source('verify').returncode, 0, 'a changed patch must invalidate verification')
        self.materialize()
        self.assertEqual((self.tree / 'value.txt').read_text(), 'second\n', 'changed patch must reach source')
        refs = subprocess.check_output(['git', '-C', str(self.tree), 'for-each-ref', '--format=%(objectname)', 'refs/isoplane/source-backups'], text=True)
        self.assertIn(old_head, refs, 'previous committed source must remain recoverable')

    def test_local_edits_are_preserved_and_refused(self):
        self.materialize()
        (self.tree / 'value.txt').write_text('local edit\n')
        self.write_patch('second')
        result = self.run_source()
        self.assertNotEqual(result.returncode, 0, 'dirty source must not be overwritten')
        self.assertIn('local source changes', result.stderr, 'dirty-tree refusal must explain which input needs preservation')
        self.assertEqual((self.tree / 'value.txt').read_text(), 'local edit\n', 'failed reconciliation must preserve local source edits')

    def test_upstream_pin_change_replaces_source(self):
        self.materialize()
        (self.patches / '001.patch').unlink()
        self.write_archive('new upstream\n')
        self.materialize()
        self.assertEqual((self.tree / 'value.txt').read_text(), 'new upstream\n', 'a changed pin must replace the actual upstream source')
        self.assertEqual(self.run_source('verify').returncode, 0, 'reconciled source must verify against its declared identity')

    def test_unrecorded_committed_change_does_not_pass_verification(self):
        self.materialize()
        subprocess.run(['git', '-C', str(self.tree), '-c', 'user.name=test', '-c', 'user.email=test@localhost',
                        'commit', '--allow-empty', '-qm', 'external commit'], check=True)
        self.assertNotEqual(self.run_source('verify').returncode, 0, 'an unrecorded source commit must not retain verified identity')

    def test_archive_hash_mismatch_cannot_change_source(self):
        self.materialize()
        self.sha = '0' * 64
        self.assertNotEqual(self.run_source().returncode, 0, 'a changed archive must fail its pinned digest')
        self.assertEqual((self.tree / 'value.txt').read_text(), 'first\n', 'unverified upstream bytes must not replace current sources')

    def test_unchanged_inputs_preserve_existing_source_head(self):
        self.materialize()
        before = (self.tree / '.git/isoplane-source.json').read_text()
        self.materialize()
        self.assertEqual((self.tree / '.git/isoplane-source.json').read_text(), before, 'unchanged inputs must retain the same verified source identity')
        self.assertEqual(self.run_source('verify').returncode, 0, 'reconciled source must verify against its declared identity')


if __name__ == '__main__':
    unittest.main()
