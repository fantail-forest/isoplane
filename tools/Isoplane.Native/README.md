# Isoplane native support

Local guest execution and build-process ownership for Isoplane tooling. The managed CLI owns project discovery, publication,
watching, and rendering.

The native ABI is defined in `crates/isoplane-native/src`: the entry points in
`lib.rs` and the `#[repr(C)]` records they exchange. `task generate:native-bindings`
writes their C# declarations, `NativeMethods.g.cs`, with csbindgen. Managed
wrappers own buffers, callbacks, and session lifetimes.

Process ownership supports Linux, macOS and Windows. Handled shutdown and cancellation
clean up owned subprocesses; abrupt termination of the owning process does
not guarantee cleanup.
