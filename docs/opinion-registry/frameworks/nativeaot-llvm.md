# Shared Mono runtime

## Customers select an SDK release

Build customer applications as managed IL packages. Record the SDK's runtime
identity in each package, and admit it only against the platform's installed
runtime for that release. Platform builds compile Mono and its PAL to WASM;
customer projects supply no runtime module or native compilation profile.

**Not this:** Per-application native compilation or selectable execution modes.

**Exception:** Minimal sandbox and engine fixtures can supply WASM directly to
the integration under test.

## Prepare the runtime before the application

Initialize the platform runtime into a shared baseline, then initialize each
application package into its own layer. Require both initialization exports.
Keep runtime identity and memory-limit checks at admission and restoration.

**Not this:** Treating a missing initialization stage as successful preparation.

**Exception:** Portable local preparation on platforms without mapped layers
runs both stages and captures the resulting template.

## Bind an import only with a wrapper and a test

Add an ABI import together with its SDK consumer and a real guest test. Generate
representations from the host contract, or drift-test them against it.

**Not this:** Unused capability declarations or hand-maintained copies.

**Exception:** None.

## Serialize shared fixture builds

Use the shared fixture helper, per-build intermediates and its cross-process
lock. A nested build must not acquire its parent's lock. Missing toolchains fail
tests with an actionable diagnostic.

**Not this:** Parallel MSBuild writers sharing intermediates or silently skipped fixtures.

**Exception:** None.
