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
                    if path.suffix in (".rs", ".toml", ".lock", ".cs", ".csproj", ".props", ".targets", ".c", ".sh", ".py"):
                        newest = max(newest, path.stat().st_mtime_ns)
        print(newest)
    elif action == "mtime":
        print(Path(args[0]).stat().st_mtime_ns)
    elif action == "size":
        print(Path(args[0]).stat().st_size)
    else:
        sys.exit(f"Unknown fixture operation: {action}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
