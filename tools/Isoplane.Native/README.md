# Isoplane native support

Local guest execution and build-process ownership for Isoplane tooling. The managed CLI owns project discovery, publication,
watching, and rendering.

The native ABI is defined in `crates/isoplane-native/src/lib.rs`. Regenerate its
C# declarations with `ISOPLANE_GENERATE_BINDINGS=1 scripts/dev cargo build -p isoplane-native`;
the Rust test suite checks for drift. csbindgen 1.9.8 generates the C ABI types
and calls; managed wrappers own buffers, callbacks, and session lifetimes.

Process ownership supports Linux, macOS and Windows. Handled shutdown and cancellation
clean up owned subprocesses; abrupt termination of the owning process does
not guarantee cleanup.
