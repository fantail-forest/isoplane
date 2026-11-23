"""Checks shared by preparation of platform releases and their combined NuGet feed."""
import hashlib
import xml.etree.ElementTree as ET
import zipfile


SHARED_PACKAGES = (
    'Isoplane.Sdk', 'Isoplane.AspNetCore', 'Isoplane.AspNetCore.DataProtection.EntityFrameworkCore',
    'Isoplane.OpenTelemetry', 'Isoplane.Templates', 'Isoplane.Testing',
)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify_nuspec(path, package_id, version, revision):
    with zipfile.ZipFile(path) as archive:
        root = ET.fromstring(archive.read(f'{package_id}.nuspec'))
    metadata = root.find('{*}metadata')
    if (metadata is None or metadata.findtext('{*}id') != package_id
            or metadata.findtext('{*}version') != version):
        raise ValueError(f'{path}: NuSpec identity mismatch')
    repository = metadata.find('{*}repository')
    if repository is None or repository.get('commit') != revision:
        raise ValueError(f'{path}: source revision mismatch')
