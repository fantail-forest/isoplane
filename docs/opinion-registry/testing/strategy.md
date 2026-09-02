# Testing Strategy

## Three layers, each proving different things

Unit tests prove isolated logic (a page diff, a route matcher, a budget calculation). Integration tests in a crate's `tests/` prove that crate's components collaborate through real interfaces (a cell dispatching a real wasm guest, the store fencing a real SQLite row). Acceptance scenarios prove the platform delivers a behaviour through its real entry points: `wispd`'s listener, the `wisp` command, a guest export. Most bugs live at the interfaces between components, so integration tests catch more per test than units do.

**Not this:** Hundreds of unit tests of engine internals wearing acceptance-scenario clothing, plus one end-to-end test.

**Exception:** Pure algorithmic code with high combinatorics (delta encoding, trap classification, validator rules) deserves focused unit tests with a fixture corpus.

## Make an acceptance scenario a test, not a report

Make a scenario assert: every check is pass or fail, runs under `cargo test`, and fails the build when it fails. A scenario whose output is a rendered report with verdict vocabulary, sample counts, and prose notes is a benchmark with a narrative, and its readers learn to trust what the assertions never checked.

**Not this:** A scenario runner whose numeric checks produce a third verdict that passes `cargo test` unless an environment variable is set.

**Exception:** A benchmark may render a report. It lives in the benchmark crate.

## Test behaviour through public interfaces, not implementation

A good test says what the system does ("a tombstoned instance refuses the next call"), not how ("the slot's state hint is set to 3"). If you refactor the internals and no behaviour changes, zero tests should break.

**Not this:** Asserting on a lock's wait count or a queue's internal depth from a test in another crate.

**Exception:** None. If you need to verify internal calls, the design needs an interface at a different boundary.

## Cover critical paths deeply, not every line

Focus testing effort where bugs are expensive: the sandbox boundary, budget kills and tombstones, fencing, hibernation round-trips, tenant isolation at the proxy. Don't chase coverage on config plumbing or rendering.

**Not this:** Fifty-eight tests for a markdown table renderer.

**Exception:** When a coverage gap reveals an untested branch in critical logic, close it.

## Make every assertion say what is wrong

An assertion failure is read by someone who did not write the test, often months later, from a CI log. The values the macro prints tell them what was compared; the message tells them what it means. Write the message in the system's terms, as the sentence a reader needs: "a promoted version must be routable", not "status was wrong". Clippy's `missing_assert_message` lint enforces this in application code and skips test code by design, so in tests it is a review rule.

**Not this:** `assert_eq!(status, 200)`; or a message that restates the comparison.

**Exception:** None.

## Vertical increments, not horizontal layers

When adding a feature, write one test and one implementation at a time, through the interface the feature is consumed by. Don't write all tests first, then all implementation, and don't write the implementation for a layer that has no test yet.

**Not this:** A durability crate with leases, inbox, outbox, alarms, migration, and rollout, of which the daemon wires a third and the rest is exercised only by the crate's own fakes.

**Exception:** None.

## Each test has a single reason to fail

One behaviour per test. If a test name contains "and", split it. When a test fails, the name alone should say what broke.

**Not this:** A scenario with eleven checks under one name.

**Exception:** A multi-assertion test is fine when every assertion verifies a facet of the same behaviour (status code and body of one response).

## Doubles isolate; they don't cover

A stub, fake, or mock lets a test exercise the code above a boundary without the thing below it. That is its whole contribution. The replaced component is exactly as tested as it was before, so give it its own tests, and the concerns its consumers rely on (ordering, fencing, error variants) are the ones to cover first.

**Not this:** A crate whose store is only ever a fake, described as tested because the fake's callers are.

**Exception:** A third-party service, a slow one, or one that takes more than a simple container to run is replaced by a recorded or simulated double in the default suite, and its contract is covered by a small number of tests against the real service run on demand.

## Ship a red test with its fix

A test that fails on the commit that adds it is a bug report, and bug reports go in the tracker, not the suite. Committing it ignored hides the failure; committing it with the assertion inverted so it passes records the defect as the intended behaviour and breaks the day the defect is fixed. Let a test enter the suite only green, and never leave the suite in a state anyone intends to be red.

**Not this:** A test asserting that a request returns the wrong status, with a comment saying the right status is coming later.

**Exception:** None.

## A fast suite is a feature

Validate the critical integrations (the SDK against the host, wasmtime, pre-initialisation, the execution engine) with integration tests through their real interfaces that run in seconds. Keep end-to-end tests through the daemon or the CLI few, covering what a customer sees, and only while they run in reasonable time. Replace a slow end-to-end test whenever an integration test could cover it, because a suite nobody runs before committing protects nothing.

**Not this:** A scenario suite measured in minutes that developers skip, standing in for integration tests that would have run in seconds.

**Exception:** A slow test that proves something nothing faster can (a hibernate-and-wake round trip of a real .NET guest) stays, tagged so it can be run on demand.
