#!/usr/bin/env python3
"""Fixture build tools and content identities, shared by guest builders."""

import hashlib
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import venv


def target_directory(root):
    metadata = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root)
    return Path(json.loads(metadata)["target_directory"])


def source_files(root):
    root = Path(root)
    if root.is_file():
        return [root]
    if not root.is_dir():
        raise FileNotFoundError(f"fixture input does not exist: {root}")
    result = []
    for directory, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in ("obj", "obj-mono", "bin", "out", "target", ".git", ".vscode-test", "node_modules", "__pycache__", ".build", ".artifacts", ".test-runs"))
        for name in sorted(files):
            if name == ".git":
                continue
            path = Path(directory) / name
            result.append(path)
    return result


def file_digest(path):
    with Path(path).open("rb") as stream:
        digest = hashlib.sha256()
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
        return digest.hexdigest()


def fingerprint(roots):
    digest = hashlib.sha256()
    for index, root in enumerate(roots):
        root = Path(root)
        for path in source_files(root):
            name = path.name if root.is_file() else path.relative_to(root).as_posix()
            record = json.dumps([index, name, file_digest(path)], separators=(",", ":"))
            digest.update(record.encode())
            digest.update(b"\n")
    return digest.hexdigest()


def output_identity(outputs):
    return [file_digest(path) for path in outputs]


def is_fresh(receipt, inputs, outputs):
    try:
        recorded = json.loads(Path(receipt).read_text())
        return recorded == {"version": 2, "inputs": inputs, "outputs": output_identity(outputs)}
    except (OSError, ValueError):
        return False


def write_receipt(receipt, inputs, outputs):
    receipt = Path(receipt)
    record = {"version": 2, "inputs": inputs, "outputs": output_identity(outputs)}
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", dir=receipt.parent, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(record, stream, sort_keys=True)
            stream.write("\n")
        temporary.replace(receipt)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def published_guest(directory):
    candidates = [path for path in Path(directory).glob("*.wasm")
                  if path.is_file() and Path(str(path) + ".runtime").is_file()
                  and Path(str(path) + ".runtime").read_text().strip()]
    if len(candidates) != 1:
        raise ValueError(f"Expected one SDK guest with runtime provenance in {directory}; found {len(candidates)}")
    return candidates[0]


def build_managed_tool(root, action, project, assembly_name):
    import build_storage
    with build_storage.product(action, root=root / ".build/clr-anycpu", name=action) as target:
        artifacts = target / "artifacts"
        assembly = artifacts / f"bin/{assembly_name}/debug/{assembly_name}.dll"
        inputs = fingerprint([root / name for name in (
            "tools", "guest-sdks/dotnet", "Directory.Build.props", "global.json",
            "nuget.config", "scripts/fixture-tools.py")])
        inputs += subprocess.check_output(["dotnet", "--version"], cwd=root, text=True).strip()
        receipt = target / "complete.json"
        outputs = sorted(path for path in (artifacts / "bin").rglob("*") if path.is_file())
        if not assembly.is_file() or not is_fresh(receipt, inputs, outputs):
            if sys.platform != "linux":
                raise RuntimeError(f"Build {action}'s shared managed assemblies on Linux first.")
            log = target / "build.log"
            with log.open("w") as output:
                result = subprocess.run(
                    ["dotnet", "build", str(root / project), "--artifacts-path", str(artifacts),
                     "--disable-build-servers", "-p:IsoplaneManagedOnly=true", "-p:UseAppHost=false"],
                    cwd=root, stdout=output, stderr=subprocess.STDOUT, timeout=600)
            if result.returncode:
                raise RuntimeError(f"Building {action} failed; see {log}")
            outputs = sorted(path for path in (artifacts / "bin").rglob("*") if path.is_file())
            write_receipt(receipt, inputs, outputs)
        if action == "isoplane-cli":
            return stage_native_cli(root, assembly, inputs)
        return assembly


def stage_native_cli(root, assembly, managed_inputs):
    import build_storage
    inputs = managed_inputs + fingerprint([root / name for name in ("crates", "Cargo.toml", "Cargo.lock", ".cargo")])
    inputs += subprocess.check_output(["rustc", "-vV"], cwd=root, text=True)
    with build_storage.product("cli", root=build_storage.build_root(root), name="cli") as target:
        binary = target / "bin" / assembly.name
        receipt = target / "complete.json"
        outputs = sorted(path for path in (target / "bin").rglob("*") if path.is_file())
        if binary.is_file() and is_fresh(receipt, inputs, outputs):
            return binary
        native = Path(subprocess.check_output(
            [sys.executable, str(root / "scripts/fixture-tools.py"), "isoplane-native", str(root), "debug"],
            cwd=root, text=True).strip())
        if (target / "bin").exists():
            shutil.rmtree(target / "bin")
        shutil.copytree(assembly.parent, target / "bin")
        runtime = target / "bin/runtime"
        runtime.mkdir(exist_ok=True)
        shutil.copy2(native, runtime / native.name)
        outputs = sorted(path for path in (target / "bin").rglob("*") if path.is_file())
        write_receipt(receipt, inputs, outputs)
        return binary


def main():
    action, *args = sys.argv[1:]
    if action == "output-directory":
        import build_storage
        root, target, name = Path(args[0]).resolve(), args[1], args[2]
        if target not in ("wasm32", "clr-anycpu"):
            sys.exit(f"unsupported portable fixture target: {target}")
        with build_storage.product('fixture:' + name, root=root / '.build' / target, name=name) as directory:
            print(directory.as_posix())
        return 0
    if action == "require-linux":
        if sys.platform != "linux":
            sys.exit("Shared fixtures are missing or stale; build them on Linux in this worktree first.")
        return 0
    if action == "target-directory":
        print(target_directory(Path(args[0]).resolve()))
        return 0
    if action == "dap-python":
        root = Path(args[0]).resolve()
        version = "1.8.20"
        identity = hashlib.sha256((sys.version + version).encode()).hexdigest()
        target = target_directory(root) / "dap-python" / identity
        target.mkdir(parents=True, exist_ok=True)
        executable = target / ("Scripts/python.exe" if os.name == 'nt' else "bin/python")
        receipt = target / "complete"
        if not receipt.exists() or not executable.is_file():
            venv.create(target, with_pip=True)
            log = target / "install.log"
            with log.open("w") as output:
                result = subprocess.run(
                    [str(executable), "-m", "pip", "install", "--disable-pip-version-check", "debugpy==" + version],
                    stdout=output, stderr=subprocess.STDOUT, timeout=600,
                )
            if result.returncode:
                sys.exit(f"installing the pinned debugpy test dependency failed; see {log}")
            receipt.write_text(version)
        verification = subprocess.run(
            [str(executable), "-c", "import debugpy; print(debugpy.__version__)"],
            capture_output=True, text=True, timeout=30,
        )
        if verification.returncode or verification.stdout.strip() != version:
            receipt.unlink(missing_ok=True)
            sys.exit(f"the pinned debugpy test environment is incomplete: {verification.stderr}")
        print(executable)
        return 0
    if action == "published-guest":
        try:
            print(published_guest(args[0]))
        except ValueError as error:
            sys.exit(str(error))
        return 0
    if action == "fingerprint":
        print(fingerprint(args))
        return 0
    if action == "fresh":
        receipt, inputs, *outputs = args
        return 0 if is_fresh(receipt, inputs, outputs) else 1
    if action == "record":
        receipt, inputs, *outputs = args
        write_receipt(receipt, inputs, outputs)
        return 0
    if action == "isoplane-native":
        root = Path(args[0]).resolve()
        profile = args[1]
        if profile not in ("debug", "release"):
            sys.exit("native fixture profile must be debug or release")
        import build_storage
        with build_storage.product("native-support", root=build_storage.build_root(root), name="native") as target:
            target.mkdir(parents=True, exist_ok=True)
            environment = dict(os.environ, CARGO_BUILD_BUILD_DIR=str(target))
            log = target / "build.log"
            with log.open("w") as output:
                result = subprocess.run(
                    ["cargo", "build", "--locked", "-p", "isoplane-native", "--target-dir", str(target)]
                    + (["--release"] if profile == "release" else []),
                    cwd=root, env=environment, stdout=output, stderr=subprocess.STDOUT,
                )
            if result.returncode != 0:
                sys.exit(f"building native Isoplane support failed; see {log}:\n{log.read_text()}")
            library = {"win32": "isoplane_native.dll", "darwin": "libisoplane_native.dylib"}.get(sys.platform, "libisoplane_native.so")
            print(target / profile / library)
            return 0
    if action == "debugger-protocol":
        print(build_managed_tool(Path(args[0]).resolve(), action,
              "tools/Isoplane.Debugging.Tests/Isoplane.Debugging.Tests.csproj", "Isoplane.Debugging.Tests"))
        return 0
    if action == "isoplane-cli":
        print(build_managed_tool(Path(args[0]).resolve(), action,
              "tools/Isoplane.Cli/Isoplane.Cli.csproj", "Isoplane.Cli"))
    elif action == "runtime-identity":
        props = Path(args[0]).read_text()
        if args[1] != "mono":
            sys.exit("runtime-identity supports only the platform Mono runtime")
        prefix = "IsoplaneGuestRuntimePack"
        def field(name):
            start = props.find(f"<{name}>")
            end = props.find(f"</{name}>")
            if start < 0 or end < 0:
                sys.exit(f"{args[0]} names no {name}")
            return props[start + len(name) + 2:end].strip()
        print(f"{field(prefix + 'Version')}@sha512:{field(prefix + 'Sha512')}")
    elif action == "size":
        print(Path(args[0]).stat().st_size)
    else:
        sys.exit(f"Unknown fixture operation: {action}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
