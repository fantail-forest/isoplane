import json
from pathlib import Path
import re
import unittest


class MacBuildInputsTests(unittest.TestCase):
    def test_macos_linkers_match_the_shared_runtime_toolchain(self):
        root = Path(__file__).resolve().parents[2]
        runtime = (root / 'guest-runtime/manifest.env').read_text()
        version = re.search(r'^WASI_SDK_VERSION=(.+)$', runtime, re.MULTILINE).group(1)
        inputs = json.loads((root / 'scripts/macos-build-inputs.json').read_text())
        for rid, architecture in [('osx-arm64', 'arm64'), ('osx-x64', 'x86_64')]:
            with self.subTest(rid=rid):
                self.assertTrue(inputs[rid]['url'].endswith(f'/wasi-sdk-{version}-{architecture}-macos.tar.gz'),
                                'the host linker must match the runtime ABI and native architecture')
                self.assertRegex(inputs[rid]['sha256'], r'^[a-f0-9]{64}$',
                                 'every downloaded toolchain must have a pinned checksum')


if __name__ == '__main__':
    unittest.main()
