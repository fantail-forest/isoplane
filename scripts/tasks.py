"""Invoke the repository's Task graph with state inside the worktree."""

import os
from pathlib import Path
import shutil
import subprocess
import sys


def run(root, *tasks):
    root = Path(root).resolve()
    executable = shutil.which("task")
    if executable is None:
        installed = Path.home() / ".local/bin" / ("task.exe" if os.name == "nt" else "task")
        if not installed.is_file():
            raise RuntimeError("Task is missing; run scripts/setup.")
        executable = str(installed)
    environment = dict(os.environ, TASK_TEMP_DIR=str(root / ".build/task"),
                       ISOPLANE_PYTHON=sys.executable)
    subprocess.run([executable, "--taskfile", str(root / "Taskfile.yml"), *tasks],
                   cwd=root, env=environment, stdout=sys.stderr, check=True)


if __name__ == "__main__":
    run(Path(__file__).resolve().parents[1], *sys.argv[1:])
