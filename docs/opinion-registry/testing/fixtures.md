# Fixtures

## Build fixtures on demand, through one helper

The [shared fixture rules](../../../AGENTS.md#common-operations) make a clean checkout and a warm checkout exercise the same test. A lock shared across test binaries prevents competing builds; a separate guest target directory prevents nested Cargo from waiting on its parent's lock.

**Not this:** Three test binaries each invoking the guest build script their own way; or a test that stops and tells the developer which command to run.

**Exception:** None.

## Fail, don't skip, when a fixture cannot be built

Under the [fixture failure rule](../../../AGENTS.md#common-operations), missing tooling or a failed build remains visible as a test failure. A developer can explicitly filter tests by name, but an automatic skip would make the same green result mean different coverage on different machines.

**Not this:** A pre-initialisation test that reports a gap and returns `Ok` when the module is not on disk.

**Exception:** None.

## Keep a hostile-guest corpus

For every rejection rule the validator enforces and every limit the engine imposes, keep a small module that trips it through the real validator or engine boundary. Run the corpus on every change to the engine or validator. Inline WAT, standalone WAT and compiled fixture projects are all suitable; choose the representation that makes the property clear and keeps the suite fast.

**Not this:** Inspecting a feature flag or testing a fake validator without showing that the real boundary rejects the hostile module.

**Exception:** Compiler- or SDK-specific behavior needs a fixture built with that toolchain; hand-written WAT alone cannot qualify that integration.

## Make a fixture prove one property

Write a fixture guest to trip one rule or exercise one mechanism, and keep it as small as that allows. A large application with a database layer and a long README is a sample or a benchmark workload, not a fixture, so describe it as what it is.

**Not this:** A multi-thousand-line C# guest with an ORM and a measurement report as its README, referenced by tests as if it were a fixture.

**Exception:** None.

## One harness per crate, in `tests/common`

The [shared harness rule](../../../AGENTS.md#structure) gives setup one place to evolve. Copies of a rig, resolver, or wait helper drift and multiply the places a harness defect must be fixed.

**Not this:** The same `rig()`, `services()`, and `wait_for()` copied verbatim into every integration test file of a crate.

**Exception:** None.

## Don't multiply bindings implementations

Keep one production implementation of the engine's host bindings and one test implementation. A third one, in another crate's tests, means the contract has three interpretations.

**Not this:** Five host-bindings implementations across the engine tests, the acceptance harness, the cell, and two guest hostcheck harnesses.

**Exception:** A deny-all implementation for negative controls.
