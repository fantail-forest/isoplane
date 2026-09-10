#!/usr/bin/env python3
"""Portable fixture locking and source timestamps, shared by guest builders."""

import fcntl
import os
from pathlib import Path
import subprocess
import sys


def main():
    action, *args = sys.argv[1:]
    if action == "lock":
        lock, script, *command_args = args
        with open(lock, "a") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            env = dict(os.environ, WISP_FIXTURE_LOCK=script)
            result = subprocess.run(["bash", script, *command_args], env=env)
            return result.returncode
    if action == "newest":
        newest = 0
        for root in args:
            if Path(root).is_file():
                newest = max(newest, Path(root).stat().st_mtime_ns)
                continue
            for directory, dirs, files in os.walk(root):
                dirs[:] = [d for d in dirs if not d.startswith(("obj", "bin"))
                           and d not in ("out", "target", ".git", "node_modules")]
                for name in files:
                    path = Path(directory) / name
                    if path.name == "Dockerfile" or path.suffix in (".rs", ".toml", ".lock", ".cs", ".csproj", ".props", ".targets", ".c", ".h", ".sh", ".py", ".patch", ".env", ".nupkg"):
                        newest = max(newest, path.stat().st_mtime_ns)
        print(newest)
    elif action == "mtime":
        print(Path(args[0]).stat().st_mtime_ns)
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
        # The identity of the runtime pack a feed offers: its version and the
        # digest NuGet records for the package, from the props the feed stage
        # writes.
        props = Path(args[0]).read_text()
        def field(name):
            start = props.find(f"<{name}>")
            end = props.find(f"</{name}>")
            if start < 0 or end < 0:
                sys.exit(f"{args[0]} names no {name}")
            return props[start + len(name) + 2:end].strip()
        print(f"{field('WispGuestRuntimePackVersion')}@sha512:{field('WispGuestRuntimePackSha512')}")
    elif action == "size":
        print(Path(args[0]).stat().st_size)
    else:
        sys.exit(f"Unknown fixture operation: {action}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
