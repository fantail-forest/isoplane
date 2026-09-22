#!/usr/bin/env python3
"""Materialize pinned runtime sources and patches without losing local edits."""
import argparse
import hashlib
import json
import os
import shutil
import stat
from pathlib import Path
import subprocess
import tarfile
import tempfile
import uuid


def git(tree, *args, **kwargs):
    return subprocess.run(["git", "-c", "gc.auto=0", "-c", "maintenance.auto=false", "-c", "core.autocrlf=false", "-c", "core.eol=lf", "-C", str(tree), *args], check=True, text=True,
                          stdout=subprocess.PIPE, **kwargs).stdout.strip()


def digest(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def patches(directory):
    series = directory / "series"
    if not series.exists():
        return sorted(directory.glob("*.patch"))
    selected, omitted, shared = [], [], []
    for line in series.read_text().splitlines():
        line = line.strip()
        if line.startswith("# shared "):
            shared.append(directory / line.removeprefix("# shared ").strip())
        elif line.startswith("# omit "):
            name, separator, reason = line.removeprefix("# omit ").partition(" -- ")
            if not separator or not reason.strip():
                raise ValueError(f"{series}: omitted patch needs an explicit reason: {line}")
            omitted.append(directory / name.strip())
        elif line and not line.startswith("#"):
            selected.append(directory / line)
    classified = [path.resolve() for path in selected + omitted]
    if len(classified) != len(set(classified)):
        raise ValueError(f"{series}: patch is classified more than once")
    for path in selected + omitted:
        if not path.is_file():
            raise ValueError(f"{series}: classified patch does not exist: {path}")
    expected = set()
    for root in shared:
        if not root.is_dir():
            raise ValueError(f"{series}: shared patch directory does not exist: {root}")
        expected.update(path.resolve() for path in root.glob("*.patch"))
    missing = expected - set(classified)
    if missing:
        raise ValueError(f"{series}: shared patches need inclusion or an explicit omission: "
                         + ", ".join(sorted(path.name for path in missing)))
    return selected


def identity(archive, expected, directory, exclude):
    actual = digest(archive)
    if actual != expected:
        raise ValueError(f"{archive}: SHA256 {actual}, expected {expected}")
    return {"format": 2, "archive": expected, "patches": [[p.name, digest(p)] for p in patches(directory)],
            "exclude": exclude}


def clean(tree):
    if git(tree, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError(f"{tree} has local source changes; commit or preserve them before rebuilding")


def obstructing_input(tree, relative):
    current = tree
    for component in Path(relative).parts:
        current = current / component
        try:
            mode = current.lstat().st_mode
        except FileNotFoundError:
            return None
        if not stat.S_ISDIR(mode):
            if not (stat.S_ISREG(mode) or stat.S_ISLNK(mode)):
                raise ValueError(f"incoming source would replace special file {current}; preserve it before rebuilding")
            return current
    raise ValueError(f"incoming source would replace untracked directory {current}; preserve it before rebuilding")


def preserve_untracked_inputs(tree, source, backup_id):
    previous = set(git(tree, "ls-files", "-z").split("\0"))
    incoming = set(git(source, "ls-files", "-z").split("\0"))
    saved = set()
    for relative in sorted(incoming - previous - {""}):
        current = obstructing_input(tree, relative)
        if current is None:
            continue
        original = current.relative_to(tree)
        if str(original) in previous or original in saved:
            continue
        destination = tree / ".git" / "isoplane-source-backups" / backup_id / original
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(current, destination, follow_symlinks=False)
        saved.add(original)
    if saved:
        print(f"    {tree.name}: preserved {len(saved)} untracked source files under .git/isoplane-source-backups/{backup_id}")


def materialize(tree, archive, desired, directory, exclude):
    state_path = tree / ".git" / "isoplane-source.json"
    exists = (tree / ".git").is_dir()
    if exists:
        clean(tree)
        if state_path.exists():
            previous = json.loads(state_path.read_text())
            if previous["inputs"] == desired and previous["head"] == git(tree, "rev-parse", "HEAD"):
                print(f"    {tree.name}: source identity verified")
                return
    elif tree.exists():
        raise ValueError(f"{tree} exists without a source Git repository; preserve it before rebuilding")

    tree.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{tree.name}-source-", dir=tree.parent) as temp:
        staged = Path(temp)
        with tarfile.open(archive) as tar:
            members = tar.getmembers()
            prefix = Path(members[0].name).parts[0]
            tar.extractall(staged, filter="data")
        source = staged / prefix
        git(source, "init", "-q")
        with open(source / ".git/info/exclude", "a") as ignored:
            ignored.write("\n.isoplane-applied\n")
        git(source, "add", "--force", "-A")
        git(source, "-c", "user.name=isoplane", "-c", "user.email=isoplane@localhost",
            "commit", "-qm", f"pristine sha256:{desired['archive']}")
        for patch in patches(directory):
            arguments = ["apply", "--index"]
            if exclude:
                arguments.append(f"--exclude={exclude}")
            git(source, *arguments, str(patch.resolve()))
            git(source, "-c", "user.name=isoplane", "-c", "user.email=isoplane@localhost",
                "commit", "-qm", patch.name)
        if exists:
            clean(tree)
            # Preserve all committed history, including edits not produced by this script.
            backup_id = uuid.uuid4().hex
            preserve_untracked_inputs(tree, source, backup_id)
            backup = f"refs/isoplane/source-backups/{backup_id}"
            git(tree, "update-ref", backup, "HEAD")
            git(tree, "fetch", "--quiet", str(source), "HEAD")
            git(tree, "reset", "--hard", "FETCH_HEAD")
            print(f"    {tree.name}: reconciled pinned source; previous HEAD retained at {backup}")
        else:
            os.rename(source, tree)
        state_path.write_text(json.dumps({"inputs": desired, "head": git(tree, "rev-parse", "HEAD")}, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["materialize", "verify"])
    parser.add_argument("tree", type=Path)
    parser.add_argument("archive", type=Path)
    parser.add_argument("sha256")
    parser.add_argument("patches", type=Path)
    parser.add_argument("--exclude")
    args = parser.parse_args()
    try:
        desired = identity(args.archive, args.sha256, args.patches, args.exclude)
        if args.action == "materialize":
            materialize(args.tree, args.archive, desired, args.patches, args.exclude)
        else:
            clean(args.tree)
            record = args.tree / ".git/isoplane-source.json"
            if not record.exists():
                raise ValueError(f"{args.tree}: no verified source identity; run guest-runtime/build.sh patch")
            state = json.loads(record.read_text())
            if state["inputs"] != desired or state["head"] != git(args.tree, "rev-parse", "HEAD"):
                raise ValueError(f"{args.tree}: applied source differs from declared inputs; run guest-runtime/build.sh patch")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"runtime source: {error}\n")


if __name__ == "__main__":
    main()
