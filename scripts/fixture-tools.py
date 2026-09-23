#!/usr/bin/env python3
"""Fixture locking and content identities, shared by guest builders."""

import errno
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import venv


def lock_file(handle):
    if os.name == 'nt':
        import msvcrt
        handle.seek(0)
        while True:
            try:
                msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
                return
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EDEADLK):
                    raise
    else:
        import fcntl
        fcntl.flock(handle, fcntl.LOCK_EX)


def source_files(root):
    root = Path(root)
    if root.is_file():
        return [root]
    if not root.is_dir():
        raise FileNotFoundError(f"fixture input does not exist: {root}")
    result = []
    for directory, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in ("obj", "obj-mono", "bin", "out", "target", ".git", ".vscode-test", "node_modules", "__pycache__"))
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
            record = json.dumps([index, str(root), name, file_digest(path)], separators=(",", ":"))
            digest.update(record.encode())
            digest.update(b"\n")
    return digest.hexdigest()


def output_identity(outputs):
    return {str(Path(path)): file_digest(path) for path in outputs}


def is_fresh(receipt, inputs, outputs):
    try:
        recorded = json.loads(Path(receipt).read_text())
        return recorded == {"version": 1, "inputs": inputs, "outputs": output_identity(outputs)}
    except (OSError, ValueError):
        return False


def write_receipt(receipt, inputs, outputs):
    receipt = Path(receipt)
    record = {"version": 1, "inputs": inputs, "outputs": output_identity(outputs)}
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


def main():
    action, *args = sys.argv[1:]
    if action == "debugger-editor":
        root = Path(args[0]).resolve()
        extension = root / "tools" / "vscode-isoplane"
        target = root / "target" / "debugger-editor"
        target.mkdir(parents=True, exist_ok=True)
        identity = fingerprint([extension / "package.json", extension / "package-lock.json"])
        installed = extension / "node_modules" / ".package-lock.json"
        with (target / "fixture.lock").open("a") as handle:
            lock_file(handle)
            receipt = target / "complete.json"
            if not is_fresh(receipt, identity, [installed]):
                log = target / "install.log"
                with log.open("w") as output:
                    result = subprocess.run(["npm", "ci"], cwd=extension,
                                            stdout=output, stderr=subprocess.STDOUT, timeout=600)
                if result.returncode:
                    sys.exit(f"installing the editor test dependencies failed; see {log}")
                write_receipt(receipt, identity, [installed])
        print(extension / "test" / "run.js")
        return 0
    if action == "dap-python":
        root = Path(args[0]).resolve()
        version = "1.8.20"
        identity = hashlib.sha256((sys.version + version).encode()).hexdigest()
        target = root / "target" / "dap-python" / identity
        target.mkdir(parents=True, exist_ok=True)
        executable = target / ("Scripts/python.exe" if os.name == 'nt' else "bin/python")
        with (target / "fixture.lock").open("a") as handle:
            lock_file(handle)
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
    if action == "lock":
        lock, script, *command_args = args
        with open(lock, "a") as handle:
            lock_file(handle)
            env = dict(os.environ, ISOPLANE_FIXTURE_LOCK=script)
            result = subprocess.run(["bash", script, *command_args], env=env)
            return result.returncode
    if action == "isoplane-native":
        root = Path(args[0]).resolve()
        profile = args[1]
        if profile not in ("debug", "release"):
            sys.exit("native fixture profile must be debug or release")
        target = root / "target" / "native-support"
        target.mkdir(parents=True, exist_ok=True)
        environment = dict(os.environ, CARGO_BUILD_BUILD_DIR=str(target / "build"))
        with (target / "fixture.lock").open("a") as handle:
            lock_file(handle)
            log = target / "build.log"
            with log.open("w") as output:
                result = subprocess.run(
                    ["cargo", "build", "--locked", "-p", "isoplane-native", "--target-dir", str(target)]
                    + (["--release"] if profile == "release" else []),
                    cwd=root, env=environment, stdout=output, stderr=subprocess.STDOUT,
                )
            if result.returncode != 0:
                sys.exit(f"building native Isoplane support failed; see {log}:\n{log.read_text()}")
        return 0
    if action == "debugger-protocol":
        root = Path(args[0]).resolve()
        target = root / "target" / "debugger-protocol"
        target.mkdir(parents=True, exist_ok=True)
        assembly = target / "artifacts/bin/Isoplane.Debugging.Tests/debug/Isoplane.Debugging.Tests.dll"
        with (target / "fixture.lock").open("a") as handle:
            lock_file(handle)
            log = target / "build.log"
            with log.open("w") as output:
                result = subprocess.run(
                    ["dotnet", "build", str(root / "tools/Isoplane.Debugging.Tests/Isoplane.Debugging.Tests.csproj"),
                     "--artifacts-path", str(target / "artifacts"), "--disable-build-servers"],
                    cwd=root, stdout=output, stderr=subprocess.STDOUT, timeout=600,
                )
            if result.returncode:
                sys.exit(f"building the debugger protocol test driver failed; see {log}:\n{log.read_text()}")
        print(assembly)
        return 0
    if action == "isoplane-cli":
        root = Path(args[0]).resolve()
        target = root / "target" / "managed-cli"
        target.mkdir(parents=True, exist_ok=True)
        with (target / "fixture.lock").open("a") as handle:
            lock_file(handle)
            log = target / "build.log"
            with log.open("w") as output:
                result = subprocess.run(
                    ["dotnet", "build", str(root / "tools/Isoplane.Cli/Isoplane.Cli.csproj"),
                     "--artifacts-path", str(target / "artifacts"), "--disable-build-servers"],
                    cwd=root, stdout=output, stderr=subprocess.STDOUT,
                )
            if result.returncode != 0:
                sys.exit(f"building Isoplane CLI failed; see {log}:\n{log.read_text()}")
        print(target / "artifacts/bin/Isoplane.Cli/debug/Isoplane.Cli.dll")
    elif action == "runtime-identity":
        # The identity of the pack a feed offers a runtime (mono or
        # nativeaot): its version and the digest NuGet records for the
        # package, from the props the feed stage writes.
        props = Path(args[0]).read_text()
        prefix = {"mono": "IsoplaneGuestRuntimePack", "nativeaot": "IsoplaneGuestNativeAotPack"}[args[1]]
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
