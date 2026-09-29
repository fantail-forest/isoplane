#!/usr/bin/env python3
"""Build portable release modules and NuGet packages on Linux."""

import importlib.util
import os
from pathlib import Path
import subprocess
import shutil
import sys

ROOT = Path(__file__).resolve().parents[1]


def main():
    if sys.platform != 'linux':
        raise SystemExit('Build shared release outputs on Linux first.')
    spec = importlib.util.spec_from_file_location('preview', ROOT / 'scripts/build-preview.py')
    preview = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(preview)
    revision = preview.source_revision(ROOT)
    modules = ROOT / '.build/wasm32/release-runtime'
    compiled = ROOT / '.build/clr-anycpu/release'
    environment = dict(os.environ, ISOPLANE_MANAGED_ARTIFACTS=str(compiled / 'artifacts'))
    subprocess.run(['bash', str(ROOT / 'guest-sdks/dotnet/MonoRuntime/build.sh'),
                    '--output', str(modules), '--source-revision', revision],
                   cwd=ROOT, env=environment, check=True)
    for project in preview.PROJECTS.values():
        subprocess.run(['dotnet', 'pack', str(ROOT / project), '-c', 'Release',
                        '-o', str(compiled / 'packages'), '--artifacts-path', str(compiled / 'artifacts'),
                        '--disable-build-servers', f'-p:RepositoryCommit={revision}',
                        f'-p:SourceRevisionId={revision}', '-p:EnableSourceControlManagerQueries=false',
                        '-p:ContinuousIntegrationBuild=true', '-p:IsoplaneManagedOnly=true', '-p:UseAppHost=false',
                        f'-p:IsoplaneRuntimeModuleDirectory={modules}'],
                       cwd=ROOT, env=environment, check=True)
    notices = compiled / 'notices'
    if notices.exists():
        shutil.rmtree(notices)
    notices.mkdir()
    preview.load(ROOT / 'scripts/toolchain-notices.py').managed_notices(compiled / 'artifacts', notices)


if __name__ == '__main__':
    main()
