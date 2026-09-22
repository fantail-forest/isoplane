"""Content receipts must distinguish current guest artifacts from stale ones."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("fixture_tools", Path(__file__).parents[1] / "fixture-tools.py")
tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tools)


class FixtureReceipts(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = self.root / "src"
        self.source.mkdir()
        self.code = self.source / "App.cs"
        self.code.write_text("class Original {}")
        self.output = self.root / "guest.wasm"
        self.output.write_bytes(b"built guest")
        self.sidecar = self.root / "guest.wasm.runtime"
        self.sidecar.write_text("pack-one@sha512:digest")
        self.receipt = self.root / "receipt.json"
        self.inputs = tools.fingerprint([self.source])
        self.outputs = [self.output, self.sidecar]
        tools.write_receipt(self.receipt, self.inputs, self.outputs)

    def test_runtime_module_does_not_replace_the_sdk_guest_output(self):
        (self.root / "dotnet.wasm").write_bytes(b"runtime template")
        self.assertEqual(tools.published_guest(self.root), self.output,
                         "only the SDK output with runtime provenance is the guest")

    def test_multiple_identified_guest_outputs_are_refused(self):
        (self.root / "another.wasm").write_bytes(b"other guest")
        (self.root / "another.wasm.runtime").write_text("pack-two@sha512:digest")
        with self.assertRaises(ValueError, msg="ambiguous guest outputs must not be guessed"):
            tools.published_guest(self.root)

    def test_an_output_without_runtime_provenance_is_refused(self):
        self.sidecar.unlink()
        with self.assertRaises(ValueError, msg="unattributed runtime output must not become a fixture"):
            tools.published_guest(self.root)

    def fresh(self):
        return tools.is_fresh(self.receipt, tools.fingerprint([self.source]), self.outputs)

    def test_unchanged_inputs_and_outputs_are_fresh(self):
        self.assertTrue(self.fresh(), "unchanged fixture artifacts should be reused")

    def test_content_change_is_stale_even_when_timestamps_are_preserved(self):
        stamp = self.code.stat()
        self.code.write_text("class Modified {}")
        os.utime(self.code, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
        self.assertFalse(self.fresh(), "checkout timestamps must not hide changed guest source")

    def test_adding_or_removing_a_source_invalidates_the_receipt(self):
        extra = self.source / "Other.cs"
        extra.write_text("class Other {}")
        self.assertFalse(self.fresh(), "new compile inputs must trigger fixture publication")
        tools.write_receipt(self.receipt, tools.fingerprint([self.source]), self.outputs)
        extra.unlink()
        self.assertFalse(self.fresh(), "deleted compile inputs must trigger fixture publication")

    def test_generated_intermediates_do_not_invalidate_sources(self):
        generated = self.source / "obj" / "nested"
        generated.mkdir(parents=True)
        (generated / "generated.cs").write_text("generated")
        self.assertTrue(self.fresh(), "build intermediates must not cause perpetual rebuilds")

    def test_editor_test_downloads_do_not_invalidate_sources(self):
        downloaded = self.source / ".vscode-test" / "editor"
        downloaded.mkdir(parents=True)
        (downloaded / "executable").write_bytes(b"downloaded editor")
        self.assertTrue(self.fresh(), "editor test downloads are not fixture source inputs")

    def test_submodule_metadata_is_ignored_but_dependency_source_is_hashed(self):
        dependency = self.source / "dependency"
        dependency.mkdir()
        (dependency / ".git").write_text("gitdir: /another/worktree/.git/modules/dependency")
        self.assertTrue(self.fresh(), "submodule checkout metadata must not invalidate compiled artifacts")
        (dependency / "Backend.cs").write_text("class Backend {}")
        self.assertFalse(self.fresh(), "the submodule's source must still participate in fixture identity")

    def test_source_directories_are_not_excluded_by_name_prefix(self):
        source = self.source / "binary-resources"
        source.mkdir()
        (source / "payload.bin").write_bytes(b"embedded")
        self.assertFalse(self.fresh(), "a source directory beginning with bin is not build output")

    def test_modified_module_and_sidecar_are_each_rejected(self):
        for path in self.outputs:
            with self.subTest(path=path.name):
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                self.assertFalse(self.fresh(), "modified published artifacts must not be trusted")
                path.write_bytes(original)

    def test_embedded_resources_are_inputs_regardless_of_extension(self):
        for name in ("message.txt", "certificate.der", "payload.bin"):
            resource = self.source / name
            resource.write_bytes(b"original")
            tools.write_receipt(self.receipt, tools.fingerprint([self.source]), self.outputs)
            stamp = resource.stat()
            resource.write_bytes(b"modified")
            os.utime(resource, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
            self.assertFalse(self.fresh(), "embedded resources must participate in fixture freshness")

    def test_missing_or_invalid_receipt_requires_rebuild(self):
        self.receipt.unlink()
        self.assertFalse(self.fresh(), "unattributed artifacts require rebuilding")
        self.receipt.write_text("not json")
        self.assertFalse(self.fresh(), "corrupt receipts must not authorize artifact reuse")

    def test_missing_artifact_requires_rebuild(self):
        self.output.unlink()
        self.assertFalse(self.fresh(), "receipt alone cannot authorize a missing module")


if __name__ == "__main__":
    unittest.main()
