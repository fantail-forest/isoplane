"""The source ABI audit must account for every selected declaration."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'audit-pinvokes.py'


class PInvokeAuditTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='isoplane-pinvoke-test-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def audit(self):
        return subprocess.run(['python3', str(SCRIPT), str(self.root)], text=True, capture_output=True)

    def test_default_entry_point_is_the_managed_name_without_a_library_prefix(self):
        self.write('src/native/libs/System.Native/pal_test.h', 'PALEXPORT int32_t ExactName(int32_t value);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.SystemNative)] internal static partial int ExactName(int value);')
        result = self.audit()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('1 P/Invoke declarations match', result.stdout, 'the selected import must be included in the audit')

    def test_every_overload_is_checked_instead_of_the_last_declaration_winning(self):
        self.write('src/native/libs/System.Native/pal_test.h', 'PALEXPORT int32_t NativeName(int32_t value);')
        self.write('src/libraries/Common/Test.cs', '''
[LibraryImport(Libraries.SystemNative, EntryPoint = "NativeName")] internal static partial long Wrong(int value);
[LibraryImport(Libraries.SystemNative, EntryPoint = "NativeName")] internal static partial int Correct(int value);
''')
        result = self.audit()
        self.assertNotEqual(result.returncode, 0, 'a mismatched overload must not be shadowed by another declaration')
        self.assertIn('NativeName', result.stdout, 'ABI mismatch diagnostics must identify the offending import')

    def test_an_unresolved_native_prototype_fails_the_audit(self):
        self.write('src/native/libs/System.Native/pal_test.h', 'PALEXPORT int32_t Other(void);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.SystemNative)] internal static partial int Missing();')
        result = self.audit()
        self.assertNotEqual(result.returncode, 0, 'an unresolved native prototype must fail qualification')
        self.assertIn('no parsed native prototype', result.stdout, 'qualification must name a missing native prototype')

    def test_compression_export_and_calling_convention_macros_are_understood(self):
        self.write('src/native/libs/System.IO.Compression.Native/pal_test.h', 'FUNCTIONEXPORT void FUNCTIONCALLINGCONVENTION Close(void* state);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.CompressionNative)] internal static partial void Close(IntPtr state);')
        result = self.audit()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_brotli_external_headers_supply_the_actual_prototypes(self):
        self.write('src/native/external/brotli/c/include/brotli/decode.h', 'BROTLI_DEC_API BrotliDecoderState* Create(void);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.CompressionNative)] internal static partial IntPtr Create();')
        result = self.audit()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_openssl_stack_type_macros_are_pointer_return_types(self):
        self.write('src/native/libs/System.Security.Cryptography.Native/openssl.h', 'PALEXPORT STACK_OF(X509) * NewStack(void);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.CryptoNative)] internal static partial IntPtr NewStack();')
        result = self.audit()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_nullable_reference_returns_are_checked(self):
        self.write('src/native/libs/System.Native/pal_test.h', 'PALEXPORT char* SearchPath(int32_t folder);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.SystemNative)] internal static partial string? SearchPath(int folder);')
        result = self.audit()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('1 P/Invoke declarations match', result.stdout, 'nullable reference returns must remain in the audit')

    def test_an_unparsed_selected_declaration_cannot_be_silently_skipped(self):
        self.write('src/native/libs/System.Native/pal_test.h', 'PALEXPORT int32_t ExactName(void);')
        self.write('src/libraries/Common/Test.cs', '[LibraryImport(Libraries.SystemNative)] unrecognized_signature;')
        result = self.audit()
        self.assertNotEqual(result.returncode, 0, 'every selected declaration must be accounted for')
        self.assertIn('cannot parse selected LibraryImport', result.stderr, 'the failure must identify unsupported declaration syntax')

    def test_an_empty_or_wrong_source_directory_cannot_pass(self):
        self.assertNotEqual(self.audit().returncode, 0, 'checking no signatures does not qualify a runtime')


if __name__ == '__main__':
    unittest.main()
