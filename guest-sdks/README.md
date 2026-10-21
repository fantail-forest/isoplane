# `guest-sdks/` — the guest SDKs

One directory per language, each referenced by the guests under `guests/`
rather than copied into them.

| | |
|---|---|
| [`dotnet`](dotnet) | **The target.** What a tenant writes: ordinary C# compiled to IL packages for the shared Mono WASM runtime. |
| [`rust`](rust) | The reference implementation of the guest contract — a few thousand lines with no bundled runtime, used to prove the ABI is implementable and to give the harness and benchmark suite a floor to measure against. |

Read the asymmetry as the documentation does: .NET is the ecosystem Isoplane exists
for, and the Rust SDK matters because it is what keeps the contract honest.

Both use the host contract defined by the same ABI crate. The platform runtime
provides the managed exports; Rust fixtures build their own. In both cases,
`crates/isoplane-abi` is compiled into the host and read by both — so the encoding
on the two sides of the sandbox boundary is one implementation rather than
several readings of a document.

## A note on the Rust SDK's workspace

Both SDKs live here. `rust/` additionally *belongs to* the **`guests/`** Cargo
workspace — a different question from where the directory is. It wants that
workspace's size-tuned wasm32 release profile and its `isoplane-abi` features,
which are the ones the guests linking it are built with; the root workspace
builds host crates for the native target and has neither.

Its `Cargo.toml` names that root explicitly, because Cargo finds a workspace
root by walking up and would otherwise reach the repo root.

Moving it to the root workspace does work, and produces functionally identical
modules — the only byte difference is the workspace-relative source paths the
guests embed in panic locations. It is not worth it: root `cargo test
--workspace` would then try to compile this crate's tests without the `json`
feature they need, and break.

`dotnet/` has the equivalent in `Isoplane.Guest.props` — the critical MSBuild
settings a guest imports rather than copies.
