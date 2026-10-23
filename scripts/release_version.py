"""The release identity policy shared by pinning and publication."""
from semver import Version


def validate_release_version(value):
    """Reject noncanonical SemVer or build metadata, returning the unchanged identity."""
    try:
        version = Version.parse(value)
    except (TypeError, ValueError) as error:
        raise ValueError(f'invalid release version: {value!r}') from error
    # NuGet removes build metadata from package identity, while our paths retain the version.
    if version.build is not None or str(version) != value:
        raise ValueError(f'invalid release version: {value!r}; build metadata and normalization are unsupported')
    if max(version.major, version.minor, version.patch) > 2 ** 31 - 1:
        raise ValueError(f'invalid release version: {value!r}; NuGet components cannot exceed 2147483647')
    return value
