"""Combine Linux-built managed packages with each platform's native support."""

from pathlib import PurePosixPath
import tempfile
from pathlib import Path
import xml.etree.ElementTree as ET
import zipfile


def attach_native(package, name, library, runtime_identifier):
    """Add native support without rebuilding or changing any managed assembly."""
    if name not in ("Isoplane.Native", "Isoplane.Cli"):
        return
    with zipfile.ZipFile(package) as source:
        if name == "Isoplane.Native":
            entry = f"runtimes/{runtime_identifier}/native/{library.name}"
        else:
            assemblies = [path for path in source.namelist() if path.endswith("/Isoplane.Cli.dll")]
            if len(assemblies) != 1:
                raise ValueError(f"Expected one CLI assembly in {package}")
            entry = str(PurePosixPath(assemblies[0]).parent / "runtime" / library.name)
        content_types = ET.fromstring(source.read("[Content_Types].xml"))
        namespace = "http://schemas.openxmlformats.org/package/2006/content-types"
        extension = library.suffix.lstrip(".")
        if not any(item.get("Extension") == extension for item in content_types):
            ET.SubElement(content_types, f"{{{namespace}}}Default",
                          Extension=extension, ContentType="application/octet")
        with tempfile.TemporaryDirectory(prefix=".native-package-", dir=package.parent) as temporary:
            staged = Path(temporary) / package.name
            with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED) as output:
                for info in source.infolist():
                    data = (ET.tostring(content_types, encoding="utf-8", xml_declaration=True)
                            if info.filename == "[Content_Types].xml" else source.read(info))
                    output.writestr(info, data)
                info = zipfile.ZipInfo(entry, date_time=source.infolist()[0].date_time)
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o100644 << 16
                output.writestr(info, library.read_bytes())
            # Windows cannot replace the input archive while it is open.
            source.close()
            staged.replace(package)
