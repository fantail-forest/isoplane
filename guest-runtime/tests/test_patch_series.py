import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    'source_tree', Path(__file__).resolve().parents[1] / 'source-tree.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PatchSeriesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.shared = self.root / 'shared'
        self.shared.mkdir()
        self.fork = self.root / 'fork'
        self.fork.mkdir()
        (self.shared / 'first.patch').write_text('patch')
        self.series = self.fork / 'series'
        self.series.write_text('# shared ../shared\n../shared/first.patch\n')

    def test_new_shared_patch_requires_explicit_classification(self):
        self.assertEqual(len(MODULE.patches(self.fork)), 1, 'included patch must be selected')
        (self.shared / 'new.patch').write_text('new')
        with self.assertRaisesRegex(ValueError, 'new.patch'):
            MODULE.patches(self.fork)
        with self.series.open('a') as output:
            output.write('../shared/new.patch\n')
        self.assertEqual(len(MODULE.patches(self.fork)), 2, 'newly included patch must be applied')

    def test_explicit_reasoned_omission_is_not_applied(self):
        self.series.write_text(
            '# shared ../shared\n'
            '# omit ../shared/first.patch -- fork already carries equivalent code\n')
        self.assertEqual(MODULE.patches(self.fork), [], 'explicit equivalent must not apply twice')

    def test_omission_requires_reason_and_existing_patch(self):
        self.series.write_text('# shared ../shared\n# omit ../shared/first.patch\n')
        with self.assertRaisesRegex(ValueError, 'reason'):
            MODULE.patches(self.fork)
        self.series.write_text('# shared ../shared\n# omit ../shared/gone.patch -- stale\n')
        with self.assertRaisesRegex(ValueError, 'does not exist'):
            MODULE.patches(self.fork)

    def test_shared_directory_must_exist(self):
        self.series.write_text('# shared ../missing\n')
        with self.assertRaisesRegex(ValueError, 'shared patch directory does not exist'):
            MODULE.patches(self.fork)

    def test_inclusion_and_omission_cannot_both_classify_a_patch(self):
        with self.series.open('a') as output:
            output.write('# omit ../shared/first.patch -- conflicting\n')
        with self.assertRaisesRegex(ValueError, 'more than once'):
            MODULE.patches(self.fork)


if __name__ == '__main__':
    unittest.main()
