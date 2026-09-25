"""The shared runtime receipt records what the SDK package will be checked against."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "runtime_receipt", Path(__file__).parents[2] / "guest-sdks/dotnet/MonoRuntime/receipt.py")
receipt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(receipt)


class RuntimeReceipt(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "Directory.Build.props").write_text("<Project><PropertyGroup><Version>1.2.3-test</Version></PropertyGroup></Project>")
        self.runtime = self.root / "runtime"
        self.runtime.mkdir()
        for name in receipt.MODULES:
            (self.runtime / name).write_bytes(name.encode())
            (self.runtime / f"{name}.runtime").write_text("10.0.11-test@sha512:cGFjaw==\n")

    def test_the_receipt_names_the_sdk_source_pack_and_modules(self):
        receipt.main(self.runtime, self.root, "abc123")
        recorded = json.loads((self.runtime / "runtime-build.json").read_text())
        self.assertEqual(recorded, {
            "sdkVersion": "1.2.3-test",
            "sourceRevision": "abc123",
            "monoPack": "10.0.11-test@sha512:cGFjaw==",
            "modules": {name: hashlib.sha256(name.encode()).hexdigest() for name in receipt.MODULES},
        }, "the receipt must identify exactly what the pack gate checks")

    def test_the_runtime_identity_names_the_same_sdk_source_pack_and_modules(self):
        receipt.main(self.runtime, self.root, "abc123")
        recorded = json.loads((self.runtime / "runtime.json").read_text())
        self.assertEqual(recorded, {
            "sdkVersion": "1.2.3-test",
            "sourceRevision": "abc123",
            "monoPack": {"version": "10.0.11-test", "sha512": "cGFjaw=="},
            "modules": {name: {"sha256": hashlib.sha256(name.encode()).hexdigest()} for name in receipt.MODULES},
        }, "the identity the SDK ships and packages carry must describe exactly the modules built")

    def test_modules_linked_against_different_packs_are_refused(self):
        (self.runtime / f"{receipt.MODULES[1]}.runtime").write_text("10.0.12-test@sha512:b3RoZXI=\n")
        with self.assertRaises(SystemExit, msg="one SDK must not carry runtimes built from different Mono packs"):
            receipt.main(self.runtime, self.root, "abc123")
        self.assertFalse((self.runtime / "runtime-build.json").exists() or (self.runtime / "runtime.json").exists(),
                         "a refused build must leave no receipt or identity")


if __name__ == "__main__":
    unittest.main()
