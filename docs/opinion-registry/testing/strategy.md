# Testing Strategy

## Three layers, each proving different things

Unit tests prove isolated logic (a page diff, a route matcher, a budget calculation). Integration tests in a crate's `tests/` prove that crate's components collaborate through real interfaces (a cell dispatching a real wasm guest, the store fencing a real PostgreSQL row). Acceptance scenarios prove the platform delivers a behaviour through its real entry points: `isoplaned`'s listener, the `isoplane` command, a guest export. Most bugs live at the interfaces between components, so integration tests catch more per test than units do.

**Not this:** Hundreds of unit tests of engine internals wearing acceptance-scenario clothing, plus one end-to-end test.

**Exception:** Pure algorithmic code with high combinatorics (delta encoding, trap classification, validator rules) deserves focused unit tests with a fixture corpus.

## Make an acceptance scenario a test, not a report

Make a scenario assert: every check is pass or fail, runs under `cargo test`, and fails the build when it fails. A scenario whose output is a rendered report with verdict vocabulary, sample counts, and prose notes is a benchmark with a narrative, and its readers learn to trust what the assertions never checked.

**Not this:** A scenario runner whose numeric checks produce a third verdict that passes `cargo test` unless an environment variable is set.

**Exception:** A benchmark may render a report. It does not live under `cargo test`.

## Prefer behavior tests at the interface that owns it

Test observable contracts through the interfaces their consumers use, so internal refactors preserve that coverage. Focused tests may also exercise internal algorithms and state machines when they give clearer, faster or more thorough coverage. Those tests can change with the structure they exercise; they do not replace integration coverage.

**Not this:** Adding a production interface solely to avoid testing internal logic, or coupling a cross-crate contract test to private queue bookkeeping.

**Exception:** Tests qualifying an internal invariant may inspect internal state directly. Keep them with the implementation that owns the invariant.

## Cover critical paths deeply, not every line

Focus testing effort where bugs are expensive: the sandbox boundary, budget kills and tombstones, fencing, hibernation round-trips, tenant isolation at the proxy. Don't chase coverage on config plumbing or rendering.

**Not this:** Fifty-eight tests for a markdown table renderer.

**Exception:** When a coverage gap reveals an untested branch in critical logic, close it.

## Make every assertion say what is wrong

The [assertion-message rule](../../../AGENTS.md#structure) makes failures useful to someone who did not write the test. Compared values show the symptom; a message such as "a promoted version must be routable" identifies the broken system behaviour.

**Not this:** `assert_eq!(status, 200)`; or a message that restates the comparison.

**Exception:** None.

## Vertical increments, not horizontal layers

The [iteration workflow](../../../AGENTS.md#workflow-develop-validate-replan) makes each increment earn its place through a consumed interface. Building several unvalidated layers together can leave a complete-looking subsystem reachable only through fakes.

**Not this:** A durability crate with leases, inbox, outbox, alarms, migration, and rollout, of which the daemon wires a third and the rest is exercised only by the crate's own fakes.

**Exception:** None.

## Each test has a single reason to fail

The [one-behaviour rule](../../../AGENTS.md#structure) keeps a failing test diagnostic. A response status and body can jointly prove one behaviour; unrelated lifecycle checks under the same name hide which guarantee failed.

**Not this:** A scenario with eleven checks under one name.

**Exception:** A multi-assertion test is fine when every assertion verifies a facet of the same behaviour (status code and body of one response).

## Doubles isolate; they don't cover

A stub, fake, or mock lets a test exercise the code above a boundary without the thing below it. That is its whole contribution. The replaced component is exactly as tested as it was before, so give it its own tests, and the concerns its consumers rely on (ordering, fencing, error variants) are the ones to cover first.

**Not this:** A crate whose store is only ever a fake, described as tested because the fake's callers are.

**Exception:** A third-party service, a slow one, or one that takes more than a simple container to run is replaced by a recorded or simulated double in the default suite, and its contract is covered by a small number of tests against the real service run on demand.

## Ship a red test with its fix

The [prohibition on committing known failures](../../../AGENTS.md#absolute-prohibitions) keeps the suite usable as evidence. An ignored regression hides a defect, while an inverted assertion teaches future maintainers that the defect is intended.

**Not this:** A test asserting that a request returns the wrong status, with a comment saying the right status is coming later.

**Exception:** None.

## A fast suite is a feature

Validate the critical integrations (the SDK against the host, wasmtime, pre-initialisation, the execution engine) with integration tests through their real interfaces that run in seconds. Keep end-to-end tests through the daemon or the CLI few, covering what a customer sees, and only while they run in reasonable time. Replace a slow end-to-end test whenever an integration test could cover it, because a suite nobody runs before committing protects nothing.

**Not this:** A scenario suite measured in minutes that developers skip, standing in for integration tests that would have run in seconds.

**Exception:** A slow test that proves something nothing faster can (a hibernate-and-wake round trip of a real .NET guest) stays, tagged so it can be run on demand.
