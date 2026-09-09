# Local Isoplane CLI

The .NET tool owns publication and source watching; its bundled native runtime
validates, prepares and executes WASM. This package currently targets the Linux
host used to build it. macOS and Windows packages are not validated.

From the repository root, build and pack into a local feed:

```sh
cargo build -p wisp-cli
dotnet pack tools/Isoplane.Cli -p:WispRuntimePath="$PWD/target/debug/wisp" -o /tmp/wisp-preview-feed
dotnet tool install Isoplane.Cli --version 0.1.0 --tool-path /tmp/isoplane-tools --add-source /tmp/wisp-preview-feed
/tmp/isoplane-tools/isoplane --version
```

Use the version of the package you built when installing. `isoplane --version`
reports the installed CLI version and its source revision when available.

The tool requires the .NET 10 SDK. Publishing guests also
requires the SDK packages, wasi-sdk and wasm-tools described in the
[ASP.NET guide](../../guest-sdks/dotnet/AspNetCore/README.md).

Run a generated app with `/tmp/isoplane-tools/isoplane dev --project /path/to/app`.
Publication follows the module path in `wisp.toml`. Source edits rebuild into a
staging directory and replace the module after successful publication; failed
builds leave the last working app serving. Ctrl+C stops the session and its
active child processes. `isoplane doctor path/to/module.wasm` runs the native
validator.

On the Ubuntu SDK installation that reports an unsupported `ubuntu.24.04-x64`
runtime identifier, prefix **tool installation only** with
`DOTNET_RUNTIME_ID=linux-x64`. The SDK otherwise reports a misleading missing
`DotnetToolSettings.xml` error. This process-local workaround does not change
the SDK or the packaged tool.
