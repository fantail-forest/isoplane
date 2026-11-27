"""Create the editor tests' project from the packaged customer template, without the template's Program.cs."""
import os
from pathlib import Path
import subprocess
import shutil
import sys
import xml.etree.ElementTree as ET

root = Path(__file__).resolve().parents[3]
destination = Path(sys.argv[1]).resolve()
packages = Path(os.environ['ISOPLANE_TEST_PACKAGE_FEED'])
version = subprocess.run(['dotnet', 'msbuild', str(root / 'Directory.Build.props'), '-nologo', '-getProperty:Version'],
                         check=True, capture_output=True, text=True).stdout.strip()
hive = destination / 'hive'
app = destination / 'app'
for cache in (hive, app, destination / 'nuget'):
    if cache.exists():
        shutil.rmtree(cache)
subprocess.run(['dotnet', 'new', 'install', str(packages / f'Isoplane.Templates.{version}.nupkg'),
                '--force', '--debug:custom-hive', str(hive)], check=True)
subprocess.run(['dotnet', 'new', 'isoplane-web', '--name', 'EditorDebug', '--force',
                '--output', str(app), '--debug:custom-hive', str(hive)], check=True)
config = ET.Element('configuration')
sources = ET.SubElement(config, 'packageSources')
ET.SubElement(sources, 'clear')
ET.SubElement(sources, 'add', key='local', value=str(packages))
ET.SubElement(sources, 'add', key='nuget', value='https://api.nuget.org/v3/index.json')
settings = ET.SubElement(config, 'config')
ET.SubElement(settings, 'add', key='globalPackagesFolder', value=str(destination / 'nuget'))
mapping = ET.SubElement(config, 'packageSourceMapping')
for name, pattern in [('local', 'Isoplane.*'), ('nuget', '*')]:
    ET.SubElement(ET.SubElement(mapping, 'packageSource', key=name), 'package', pattern=pattern)
ET.indent(config)
ET.ElementTree(config).write(app / 'nuget.config', encoding='utf-8', xml_declaration=True)
# The tests have a Program.cs of their own, which Task installs once this project exists.
(app / 'Program.cs').unlink()
