# `guest-sdks/` — the guest SDKs

One directory per language, each referenced by the guests under `guests/`
rather than copied into them.

| | |
|---|---|
| [`dotnet`](dotnet) | **The target.** What a tenant writes: ordinary C# compiled by NativeAOT-LLVM to a core wasm module. |
| [`rust`](rust) | The reference implementation of the guest contract — a few thousand lines with no bundled runtime, used to prove the ABI is implementable and to give the harness and benchmark suite a floor to measure against. |

Read the asymmetry as the documentation does: .NET is the ecosystem WISP exists
for, and the Rust SDK matters because it is what keeps the contract honest.

Both build the *same* five exports against the *same* import module, and
`crates/wisp-abi` is compiled into the host and read by both — so the encoding
on the two sides of the sandbox boundary is one implementation rather than
several readings of a document.

## A note on the Rust SDK's workspace

`rust/` is a member of the **`guests/`** Cargo workspace, not the root one: it
targets wasm32 under the size-tuned release profile the guests that link it
use. Its `Cargo.toml` names that root explicitly, because Cargo would otherwise
walk up to the repo root, which excludes this directory for the same reason.

`dotnet/` has the equivalent in `Wisp.Guest.props` — the load-bearing MSBuild
settings a guest imports rather than copies.
