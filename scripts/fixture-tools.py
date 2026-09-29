#!/usr/bin/env python3
"""Fixture build tools and content identities, shared by guest builders."""

import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import venv


def target_directory(root):
    metadata = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root)
    return Path(json.loads(metadata)["target_directory"])


def published_guest(directory):
    candidates = [path for path in Path(directory).glob("*.wasm")
                  if path.is_file() and Path(str(path) + ".runtime").is_file()
                  and Path(str(path) + ".runtime").read_text().strip()]
    if len(candidates) != 1:
        raise ValueError(f"Expected one SDK guest with runtime provenance in {directory}; found {len(candidates)}")
    return candidates[0]


MANAGED_TOOLS = {
    "cli": ("isoplane-cli", "Isoplane.Cli"),
    "debugger": ("debugger-protocol", "Isoplane.Debugging.Tests"),
}


def managed_assembly(root, tool):
    directory, assembly = MANAGED_TOOLS[tool]
    return root / f".build/clr-anycpu/{directory}/artifacts/bin/{assembly}/debug/{assembly}.dll"


def compile_managed(root, tool):
    if sys.platform != "linux":
        raise RuntimeError("Shared managed outputs are missing or stale; build them on Linux first.")
    directory, assembly = MANAGED_TOOLS[tool]
    subprocess.run(["dotnet", "build", str(root / f"tools/{assembly}/{assembly}.csproj"),
                    "--artifacts-path", str(root / f".build/clr-anycpu/{directory}/artifacts"),
                    "--disable-build-servers", "-p:IsoplaneManagedOnly=true", "-p:UseAppHost=false"],
                   cwd=root, check=True)


def stage_native_cli(root):
    import build_storage
    target = build_storage.build_root(root) / "cli/bin"
    library = {"win32": "isoplane_native.dll", "darwin": "libisoplane_native.dylib"}.get(sys.platform, "libisoplane_native.so")
    native = build_storage.build_root(root) / "native/debug" / library
    shutil.copytree(managed_assembly(root, "cli").parent, target, dirs_exist_ok=True)
    (target / "runtime").mkdir(exist_ok=True)
    shutil.copy2(native, target / "runtime" / library)
    return target / "Isoplane.Cli.dll"


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
        import build_storage
        target = build_storage.build_root(root) / "dap-python"
        target.mkdir(parents=True, exist_ok=True)
        executable = target / ("Scripts/python.exe" if os.name == 'nt' else "bin/python")
        if not executable.is_file():
            venv.create(target, with_pip=True)
        log = target / "install.log"
        with log.open("w") as output:
            result = subprocess.run(
                [str(executable), "-m", "pip", "install", "--disable-pip-version-check", "debugpy==" + version],
                stdout=output, stderr=subprocess.STDOUT, timeout=600,
            )
        if result.returncode:
            sys.exit(f"installing the pinned debugpy test dependency failed; see {log}")
        verification = subprocess.run(
            [str(executable), "-c", "import debugpy; print(debugpy.__version__)"],
            capture_output=True, text=True, timeout=30,
        )
        if verification.returncode or verification.stdout.strip() != version:
            sys.exit(f"the pinned debugpy test environment is incomplete: {verification.stderr}")
        print(executable)
        return 0
    if action == "published-guest":
        try:
            print(published_guest(args[0]))
        except ValueError as error:
            sys.exit(str(error))
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
    if action in ("compile-managed", "stage-cli", "debugger-protocol", "isoplane-cli"):
        import tasks
        import build_storage
        root = Path(__file__).resolve().parents[1]
        if action == "compile-managed":
            compile_managed(root, args[0])
        elif action == "stage-cli":
            stage_native_cli(root)
        elif action == "debugger-protocol":
            tasks.run(root, "managed:debugger")
            print(managed_assembly(root, "debugger"))
        else:
            tasks.run(root, "cli")
            print(build_storage.build_root(root) / "cli/bin/Isoplane.Cli.dll")
        return 0
    if action == "runtime-identity":
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
