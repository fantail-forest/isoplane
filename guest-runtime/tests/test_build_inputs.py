"""Exercise generated-output invalidation without building a runtime."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]


class BuildInputsTests(unittest.TestCase):
    def run_script(self, statements, setup=None):
        temporary = tempfile.TemporaryDirectory(prefix='isoplane-build-inputs-')
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        shutil.copy(ROOT / 'manifest.env', root)
        shutil.copy(ROOT / 'source-tree.py', root)
        shutil.copy(ROOT / 'feed-identity.py', root)
        (root / 'patches/icu').mkdir(parents=True)
        if setup is not None:
            setup(root)
        script = (ROOT / 'build.sh').read_text().split('\nstages=("$@")')[0]
        (root / 'build.sh').write_text(script + '\n' + statements)
        environment = dict(os.environ, ISOPLANE_RUNTIME_SOURCE=str(root), ISOPLANE_RUNTIME_WORK=str(root / 'work'))
        result = subprocess.run(['bash', str(root / 'build.sh')], env=environment,
                                capture_output=True, text=True, timeout=30)
        return root, result

    def run_case(self, recorded):
        statements = '''
mkdir -p "$WORK/test/artifacts"
echo stale > "$WORK/test/artifacts/object.o"
'''
        if recorded == 'matching':
            statements += 'inputs_digest > "$WORK/receipt"\n'
        elif recorded == 'changed':
            statements += 'echo old > "$WORK/receipt"\n'
        statements += '''prepare_pack_outputs "$WORK/test" "$WORK/receipt"
if [ -e "$WORK/test/artifacts/object.o" ]; then echo retained; else echo invalidated; fi
'''
        _, result = self.run_script(statements)
        self.assertEqual(result.returncode, 0, 'generated output qualification failed: ' + result.stderr)
        return result.stdout.strip().splitlines()[-1]

    def test_inputs_changed_during_compilation_do_not_receive_a_success_receipt(self):
        root, result = self.run_script('''
expected="$(inputs_digest)"
echo changed >> "$HERE/manifest.env"
fake_source_check() { return 0; }
complete_pack_build "$expected" "$WORK/receipt" fake_source_check
''')
        self.assertNotEqual(result.returncode, 0, 'changed inputs must fail the build completion')
        self.assertIn('inputs changed during compilation', result.stderr, 'the failure must explain that source changed while building')
        self.assertFalse((root / 'work/receipt').exists(), 'failed compilation must not create a success receipt')

    def test_feed_refuses_packages_or_version_files_changed_after_compilation(self):
        for changed in ['package.nupkg', 'pack-version']:
            with self.subTest(changed=changed):
                root, result = self.run_script('''
runtime_source() { return 0; }
require_build_inputs() { return 0; }
inputs_digest > "$WORK/runtime-build-inputs"
echo version > "$WORK/pack-version"
echo package > "$WORK/package.nupkg"
sha256sum "$WORK/pack-version" "$WORK/package.nupkg" > "$WORK/runtime-pack-artifacts"
''' + f'echo changed >> "$WORK/{changed}"\nstage_feed\n')
                self.assertNotEqual(result.returncode, 0, 'changed package identity must prevent publication')
                self.assertIn('package bytes or version identity changed', result.stderr, 'publication must diagnose modified pack artifacts')
                self.assertFalse((root / 'out/feed/Isoplane.GuestRuntime.props').exists(), 'unverified packages must not receive feed metadata')

    def test_toolchain_rejects_an_unverified_archive_before_replacing_an_existing_install(self):
        root, result = self.run_script('''
mkdir -p "$WASI_SDK"
echo preserve > "$WASI_SDK/local-tool"
echo corrupt > "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz"
stage_toolchain
''')
        self.assertNotEqual(result.returncode, 0, 'a standalone toolchain stage must verify its archive')
        self.assertIn('pinned SHA256', result.stderr, 'archive failure must name the violated pin')
        self.assertEqual((root / 'work/wasi-sdk/local-tool').read_text(), 'preserve\n', 'unverified bytes must not replace an existing toolchain')

    def test_a_broken_ninja_launcher_is_rejected_before_compilation(self):
        _, result = self.run_script('''
mkdir -p "$WORK/tools"
printf '#!/bin/sh\\nexit 1\\n' > "$WORK/tools/ninja"
chmod +x "$WORK/tools/ninja"
PATH="$WORK/tools"
require_ninja
''')
        self.assertNotEqual(result.returncode, 0, 'finding a Ninja path is insufficient when its launcher cannot execute')
        self.assertIn('Ninja cannot run', result.stderr, 'the failure must diagnose the unusable selected launcher')

    def test_a_missing_ninja_is_reported_before_compilation(self):
        _, result = self.run_script('PATH="$WORK/missing-tools"\nrequire_ninja\n')
        self.assertNotEqual(result.returncode, 0, 'runtime compilation must fail early without Ninja')
        self.assertIn('Ninja is missing', result.stderr, 'the prerequisite diagnostic must name Ninja')

    def icu_case(self, after_build='', mutate_inputs=False):
        def setup(root):
            downloads = root / 'work/downloads'
            downloads.mkdir(parents=True)
            for mode in ['mono']:
                with zipfile.ZipFile(downloads / f'icu-{mode}-fixture.nupkg', 'w') as archive:
                    archive.writestr('runtimes/wasi-wasm/native/lib/icudt.dat', 'filtered')
                    archive.writestr('runtimes/wasi-wasm/native/lib/libicuuc.a', mode + '-native-library')
                    archive.writestr('runtimes/wasi-wasm/native/include/unicode.h', mode + '-header')

        statements = '''
icu_source() { return 0; }
ICU_MONO_TRANSPORT_VERSION=fixture
ICU_MONO_TRANSPORT_SHA256="$(sha256sum "$DOWNLOADS/icu-mono-fixture.nupkg" | cut -d' ' -f1)"
mkdir -p "$ICU/icu/icu4c/source"
printf '#!/bin/sh\\ntouch Makefile\\n' > "$ICU/icu/icu4c/source/runConfigureICU"
chmod +x "$ICU/icu/icu4c/source/runConfigureICU"
export ICU_DATA_FILTER_FILE=must-not-filter
make() {
  [ -z "${ICU_DATA_FILTER_FILE:-}" ] || return 1
  mkdir -p data/out
  echo complete-data > "data/out/$ICU_DATA_FILE"
'''
        if mutate_inputs:
            statements += '  echo changed >> "$HERE/manifest.env"\n'
        statements += '}\nstage_icu\n' + after_build
        return self.run_script(statements, setup)

    def test_full_icu_data_preserves_each_runtime_native_transport(self):
        root, result = self.icu_case()
        self.assertEqual(result.returncode, 0, 'staging complete ICU data must succeed: ' + result.stderr)
        for mode in ['mono']:
            native = root / f'out/icu/{mode}/runtimes/wasi-wasm/native'
            self.assertEqual((native / 'lib/icudt.dat').read_text(), 'complete-data\n', 'each runtime must receive the complete data archive')
            self.assertEqual((native / 'lib/libicuuc.a').read_text(), mode + '-native-library', 'ICU data updates must preserve that runtime transport native implementation')
            self.assertEqual((native / 'include/unicode.h').read_text(), mode + '-header', 'native headers must remain paired with their transport libraries')
        self.assertTrue((root / 'work/icu-build-inputs').exists(), 'successful data staging must record verified inputs')

    def test_changed_icu_build_inputs_do_not_receive_a_success_receipt(self):
        root, result = self.icu_case(mutate_inputs=True)
        self.assertNotEqual(result.returncode, 0, 'ICU inputs changing during compilation must fail the stage')
        self.assertIn('ICU inputs changed', (root / 'work/logs/icu-build.log').read_text(), 'the failure must identify the changed ICU build inputs')
        self.assertFalse((root / 'work/icu-build-inputs').exists(), 'changed ICU inputs must not receive a success receipt')

    def test_modified_staged_icu_artifacts_are_refused_before_pack_compilation(self):
        _, result = self.icu_case('''
echo changed >> "$ICU_MONO_NATIVE/lib/icudt.dat"
require_ninja() { return 0; }
require_build_inputs
''')
        self.assertNotEqual(result.returncode, 0, 'changed native data artifacts must not enter a runtime pack')
        self.assertIn('staged ICU native libraries or data changed', result.stderr, 'the build must name the invalidated ICU artifact receipt')

    def test_openssl_completion_records_the_installed_headers_and_archives(self):
        root, result = self.run_script('''openssl_source() { return 0; }
openssl_inputs_digest() { echo openssl; }
mkdir -p "$OUT/openssl/include/openssl" "$OUT/openssl/lib" "$WASI_SDK/bin"
mkdir -p "$OPENSSL_BUILD"
touch "$OPENSSL_BUILD/Makefile"
echo openssl > "$OUT/openssl/.isoplane-build-identity"
echo "$WASI_SDK_SHA256" > "$WASI_SDK/.isoplane-archive-sha256"
printf '#!/bin/sh\\nexit 0\\n' > "$WASI_SDK/bin/clang"
chmod +x "$WASI_SDK/bin/clang"
make() {
  if [ "$1" = install_dev ]; then
    echo library > "$OUT/openssl/lib/libcrypto.a"
    echo header > "$OUT/openssl/include/openssl/ssl.h"
  fi
}
stage_openssl
sha256sum --check --status "$WORK/openssl-build-artifacts"
''')
        self.assertEqual(result.returncode, 0, 'OpenSSL completion must produce a valid output receipt: ' + result.stderr)
        receipt = (root / 'work/openssl-build-artifacts').read_text()
        self.assertIn('libcrypto.a', receipt, 'the native archive must be bound to its build')
        self.assertIn('ssl.h', receipt, 'installed headers must be bound to their build')

    def test_openssl_reconfigures_when_work_is_evicted_but_outputs_survive(self):
        root, result = self.run_script(r'''openssl_source() { return 0; }
openssl_inputs_digest() { echo openssl; }
mkdir -p "$OUT/openssl/include/openssl" "$OUT/openssl/lib" "$WASI_SDK/bin" "$OPENSSL"
echo openssl > "$OUT/openssl/.isoplane-build-identity"
echo "$WASI_SDK_SHA256" > "$WASI_SDK/.isoplane-archive-sha256"
printf '#!/bin/sh\nexit 0\n' > "$WASI_SDK/bin/clang"
chmod +x "$WASI_SDK/bin/clang"
cat > "$OPENSSL/Configure" <<'PERL'
use strict;
my ($prefix) = map { /^--prefix=(.*)$/ ? $1 : () } @ARGV;
open(my $makefile, '>', 'Makefile') or die $!;
print $makefile "build_libs:\n\tprintf rebuilt > libcrypto.a\ninstall_dev:\n\tcp libcrypto.a '$prefix/lib/libcrypto.a'\n\tprintf header > '$prefix/include/openssl/ssl.h'\n";
close($makefile) or die $!;
PERL
mkdir -p "$OPENSSL_BUILD"
echo stale > "$OPENSSL_BUILD/Makefile"
rm -rf "$OPENSSL_BUILD"
stage_openssl
sha256sum --check --status "$WORK/openssl-build-artifacts"
''')
        self.assertEqual(result.returncode, 0, 'OpenSSL must rebuild after intermediate eviction: ' + result.stderr)
        self.assertEqual((root / 'out/openssl/lib/libcrypto.a').read_text(), 'rebuilt',
                         'a surviving receipt must not prevent regenerating the evicted build configuration')

    def test_modified_openssl_headers_and_archives_are_refused(self):
        for changed in ['lib/libcrypto.a', 'include/openssl/ssl.h']:
            with self.subTest(changed=changed):
                _, result = self.run_script('''require_ninja() { return 0; }
icu_source() { return 0; }
openssl_source() { return 0; }
icu_inputs_digest() { echo icu; }
openssl_inputs_digest() { echo openssl; }
echo icu > "$WORK/icu-build-inputs"
echo icu > "$WORK/icu-data"
sha256sum "$WORK/icu-data" > "$WORK/icu-build-artifacts"
mkdir -p "$OUT/openssl/lib" "$OUT/openssl/include/openssl" "$WASI_SDK/bin"
echo openssl > "$OUT/openssl/.isoplane-build-identity"
echo library > "$OUT/openssl/lib/libcrypto.a"
echo header > "$OUT/openssl/include/openssl/ssl.h"
find "$OUT/openssl/include" "$OUT/openssl/lib" -type f -print0 | sort -z | xargs -0 sha256sum > "$WORK/openssl-build-artifacts"
printf '#!/bin/sh\\nexit 0\\n' > "$WASI_SDK/bin/clang"
chmod +x "$WASI_SDK/bin/clang"
echo "$WASI_SDK_SHA256" > "$WASI_SDK/.isoplane-archive-sha256"
''' + f'echo changed >> "$OUT/openssl/{changed}"\nrequire_build_inputs\n')
                self.assertNotEqual(result.returncode, 0, 'modified OpenSSL bytes must not enter a runtime pack')
                self.assertIn('OpenSSL headers or archives changed', result.stderr, 'the build must diagnose its invalid native dependency receipt')

    def test_changed_inputs_invalidate_generated_native_and_managed_outputs(self):
        self.assertEqual(self.run_case('changed'), 'invalidated', 'changed sources must invalidate generated objects')

    def test_unattributed_outputs_are_not_used_to_build_a_new_pack(self):
        self.assertEqual(self.run_case('missing'), 'invalidated', 'unattributed objects must not be reused')

    def test_matching_inputs_preserve_incremental_outputs(self):
        self.assertEqual(self.run_case('matching'), 'retained', 'matching build inputs must permit incremental outputs')


if __name__ == '__main__':
    unittest.main()
