"""Include runtime build checks in the build-script test suite."""

import importlib.util
from pathlib import Path
import unittest


def load_tests(loader, tests, pattern):
    runtime = Path(__file__).resolve().parents[2] / 'guest-runtime'
    suite = unittest.TestSuite()
    for directory in (runtime / 'tests',):
        for path in sorted(directory.glob('test_*.py')):
            name = 'runtime_' + '_'.join(path.relative_to(runtime).parts).removesuffix('.py')
            spec = importlib.util.spec_from_file_location(name, path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            suite.addTests(loader.loadTestsFromModule(module))
    return suite
