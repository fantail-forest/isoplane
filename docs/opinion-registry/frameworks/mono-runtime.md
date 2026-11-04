# Shared Mono runtime

## Bind an import only with a wrapper and a test

Add an ABI import together with its SDK consumer and a real guest test. Generate
representations from the host contract, or drift-test them against it.

**Not this:** Unused capability declarations or hand-maintained copies.

**Exception:** None.

## Prepare shared runtime fixtures before tests

Follow the [testing opinions](../testing.md#build-static-compiler-outputs-in-advance).
Declare runtime and application fixture dependencies in Task, leaving native build
coordination to the build tools. Test helpers load the prepared artifacts; missing
toolchains fail preparation and missing artifacts fail selected tests with an
actionable diagnostic.

**Not this:** Test binaries coordinating MSBuild writers with their own lock protocol,
or silently skipped fixtures.

**Exception:** Tests of the build workflow may invoke it with isolated inputs.
