# Fixtures

## Consume fixtures prepared by the build tools

Follow the [preparation boundary](../workflow/tooling.md#prepare-test-prerequisites-in-the-build-graph): Task prepares declared prerequisites before the test runner starts, and shared fixture helpers resolve and load the resulting artifacts. Reuse immutable outputs across cases and binaries, with mutable state isolated per test. The build tools own freshness, ordering and coordination of writers.

**Not this:** A helper computing source fingerprints and taking build locks before spawning a compiler, or a test rebuilding a release as incidental setup.

**Exception:** A test whose subject is the build workflow may invoke it with isolated inputs. Release-installation and package-artifact checks consume explicitly prepared releases; they do not create source worktrees or change versions as incidental setup.

## Fail selected tests when prerequisites are unavailable

Under the [fixture failure rule](../../../AGENTS.md#common-operations), failed preparation fails the workflow, and missing or incompatible artifacts fail the selected test with the preparation command. A developer can explicitly select a suite or filter tests, but an automatic skip would make the same green result mean different coverage on different machines.

**Not this:** A pre-initialisation test that reports a gap and returns `Ok` when the module is not on disk.

**Exception:** None.

## Keep a hostile-guest corpus

For every rejection rule the validator enforces and every limit the engine imposes, keep a small module that trips it through the real validator or engine boundary. Run the corpus on every change to the engine or validator. Inline WAT, standalone WAT and compiled fixture projects are all suitable; choose the representation that makes the property clear and keeps the suite fast.

**Not this:** Inspecting a feature flag or testing a fake validator without showing that the real boundary rejects the hostile module.

**Exception:** Compiler- or SDK-specific behavior needs a fixture built with that toolchain; hand-written WAT alone cannot qualify that integration.

## Keep fixtures focused and share expensive artifacts

Keep a dedicated hostile fixture small enough that the rejection it exercises is clear. Related behavior probes may share a compiled module or runtime when their state is isolated and their outcomes are distinguishable. Samples may also supply regression coverage, provided they stay representative of customer code. Choose reuse by the behavior under test, not a requirement for a separate artifact per assertion.

**Not this:** Compiling the runtime again for each managed probe, copying a sample solely to call it a fixture, or adding hostile test modes to a customer sample.

**Exception:** None.

## One harness per crate, in `tests/common`

The [shared harness rule](../../../AGENTS.md#structure) gives a shared setup responsibility one place to evolve. Extract helpers when their callers need the same semantics, and keep those helpers in `tests/common`. Similar-looking setup for unrelated behavior can remain separate; a repetition count alone does not justify a configurable universal rig.

**Not this:** Fixing the same resource-lifetime defect in copied helpers, or adding mode flags to a shared rig whenever unrelated tests need different setup.

**Exception:** None.

## Don't multiply bindings implementations

Keep one production implementation of the engine's host bindings and one test implementation. A third one, in another crate's tests, means the contract has three interpretations.

**Not this:** Five host-bindings implementations across the engine tests, the acceptance harness, the cell, and two guest hostcheck harnesses.

**Exception:** A deny-all implementation for negative controls.
