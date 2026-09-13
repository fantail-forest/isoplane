#!/usr/bin/env python3
"""Fixture locking and content identities, shared by guest builders."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def source_files(root):
    root = Path(root)
    if root.is_file():
        return [root]
    if not root.is_dir():
        raise FileNotFoundError(f"fixture input does not exist: {root}")
    result = []
    for directory, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in ("obj", "obj-mono", "bin", "out", "target", ".git", "node_modules", "__pycache__"))
        for name in sorted(files):
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
            fcntl.flock(handle, fcntl.LOCK_EX)
            env = dict(os.environ, WISP_FIXTURE_LOCK=script)
            result = subprocess.run(["bash", script, *command_args], env=env)
            return result.returncode
    if action == "wisp-process":
        root = Path(args[0])
        target = root / "target" / "fixture-tools"
        result = subprocess.run(
            ["cargo", "build", "--quiet", "--release", "-p", "wisp-cli", "--bin", "wisp-process", "--target-dir", str(target)],
            cwd=root,
        )
        if result.returncode != 0:
            sys.exit(f"building the process supervisor failed (exit {result.returncode})")
        print(target / "release" / "wisp-process")
    elif action == "wisp-cli":
        # The `wisp` CLI whose `doctor` verdict gates every guest build here:
        # WISP_CLI when set, else this checkout's, built into its own target
        # directory so a fixture build started by `cargo test` never waits
        # on the lock that cargo holds.
        cli = os.environ.get("WISP_CLI")
        if not cli:
            root = Path(args[0])
            target = root / "target" / "fixture-tools"
            result = subprocess.run(
                ["cargo", "build", "--quiet", "--release", "-p", "wisp-cli", "--target-dir", str(target)],
                cwd=root,
            )
            if result.returncode != 0:
                sys.exit(f"building the wisp CLI failed (exit {result.returncode})")
            cli = str(target / "release" / "wisp")
        if not os.access(cli, os.X_OK):
            sys.exit(f"no executable wisp CLI at {cli}")
        print(cli)
    elif action == "runtime-identity":
        # The identity of the pack a feed offers a runtime (mono or
        # nativeaot): its version and the digest NuGet records for the
        # package, from the props the feed stage writes.
        props = Path(args[0]).read_text()
        prefix = {"mono": "WispGuestRuntimePack", "nativeaot": "WispGuestNativeAotPack"}[args[1]]
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
