# NativeAOT-LLVM

## Pin the compiler package in one file; upgrade in its own commit

The ILCompiler package comes from an experimental feed that ships new builds often and promises nothing. Keep its version in one props file, and pin the .NET SDK to a version, not a channel. Make an upgrade a commit that changes only those lines, with the guest fixtures rebuilt and the acceptance suite green before and after.

**Not this:** Installing whatever the feed or the channel serves today and discovering the PAL no longer links.

**Exception:** None.

## Make the PAL a closure over the BCL's WASI imports, and fail the build on any survivor

The runtime declares `wasi:*` imports the sandbox does not provide. Have the platform abstraction layer satisfy an enumerated whitelist by delegation to platform imports and trap on the rest. Fail the guest build if any `wasi:*` import survives linking, so a new BCL dependency that reaches for the filesystem is a build error, not a deploy-time rejection.

**Not this:** Letting the validator reject the module after the developer has finished writing the app.

**Exception:** None.

## Bind an import only with a wrapper and a test

Declare an extern in `Wisp.Abi` only when a `Wisp.Sdk` type calls it and a test exercises the call through a guest. A bound import with no wrapper is a capability the SDK claims and does not deliver, and a doc comment describing a method that does not exist is worse.

**Not this:** Externs for put, fetch, and wait-until declared and never wrapped, with SDK docs describing them as available.

**Exception:** None.

## Generate the entrypoint

Emit the exports every guest must provide from a source generator or a build step, driven by the handler type the developer names. A customer writes a handler, not a set of `UnmanagedCallersOnly` one-liners copied from a README.

**Not this:** An entrypoint file pasted into every guest.

**Exception:** Hostile fixtures that deliberately break the export contract write theirs by hand.

## Build guests serially with isolated intermediates

Guests that share the SDK project race on MSBuild intermediates when built concurrently. Have the guest build script build one at a time with per-guest intermediate directories, and let nothing else invoke MSBuild on those projects.

**Not this:** Parallel guest builds that pass on a quiet machine and fail intermittently under load.

**Exception:** None.
