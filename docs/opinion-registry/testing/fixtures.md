# Fixtures

## Build fixtures on demand, through one helper

Have a test that needs a guest module ask one shared helper for it by name, and have the helper build it when missing or stale by running the same script a developer would. Give the helper one lock so concurrent test binaries do not race on the build, and give the guests their own target directory so a nested cargo never waits on the lock its parent holds. Never let a test shell out to a build on its own.

**Not this:** Three test binaries each invoking the guest build script their own way; or a test that stops and tells the developer which command to run.

**Exception:** None.

## Fail, don't skip, when a fixture cannot be built

When a fixture cannot be built, because the toolchain is absent or the build broke, fail the test with the reason. A test that prints "not proven" and passes is a green light that means nothing. A developer who wants to skip those tests filters by name.

**Not this:** A pre-initialisation test that reports a gap and returns `Ok` when the module is not on disk.

**Exception:** None.

## Keep a hostile-guest corpus

For every rejection rule the validator enforces and every limit the engine imposes, keep a small module that trips it: an undeclared import, a shared memory, a function bomb, a spinner, an allocator that never stops. The corpus is the proof that the sandbox is a sandbox; run it on every change to the engine or the validator. The validator corpus test is the reference pattern.

**Not this:** Rules tested only against WAT strings inline in a test, so that a build-configuration change re-enabling threads is caught by a reader.

**Exception:** Hostile fixtures may be written in any language that compiles to WASM, because the sandbox does not care.

## Make a fixture prove one property

Write a fixture guest to trip one rule or exercise one mechanism, and keep it as small as that allows. A large application with a database layer and a long README is a sample or a benchmark workload, not a fixture, so describe it as what it is.

**Not this:** A multi-thousand-line C# guest with an ORM and a measurement report as its README, referenced by tests as if it were a fixture.

**Exception:** None.

## One harness per crate, in `tests/common`

Keep shared setup (a rig, a fixed resolver, a wait helper) in one `tests/common/` module per crate. Copy-pasting it into every test file means several versions that drift and several places a bug has to be fixed.

**Not this:** The same `rig()`, `services()`, and `wait_for()` copied verbatim into every integration test file of a crate.

**Exception:** None.

## Don't multiply bindings implementations

Keep one production implementation of the engine's host bindings and one test implementation. A third one, in another crate's tests, means the contract has three interpretations.

**Not this:** Five host-bindings implementations across the engine tests, the acceptance harness, the cell, and two guest hostcheck harnesses.

**Exception:** A deny-all implementation for negative controls.
