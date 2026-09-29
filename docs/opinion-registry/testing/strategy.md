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

Apply the [testing principles](../../../AGENTS.md#testing-standards): each test earns its build, execution and maintenance cost through distinct failure detection. Focus effort where bugs are expensive: the sandbox boundary, budget kills and tombstones, fencing, hibernation round-trips, tenant isolation at the proxy. Extend existing coverage when it can catch the regression. Remove redundant tests when the surviving coverage still detects the relevant failures; test count and line coverage are not goals.

**Not this:** Repeating an assignment in a test, checking incidental source spelling, or adding another expensive journey for a condition already covered at the interface that owns it.

**Exception:** Overlap is justified when tests catch distinct failures, such as isolated policy logic and its production wiring. A slow test that uniquely qualifies a critical boundary still earns its place.

## Make assertion failures useful

Follow the [assertion guidance](../../../AGENTS.md#structure): use equality, collection and error assertions that preserve useful values. Add context when the test name and assertion do not identify the broken behavior, especially for a step in a journey or a case in a table. Routine comparisons need no extra prose.

**Not this:** Replacing an equality assertion with `Assert.True(actual == expected, message)` and losing the compared values, or writing a message that merely restates the comparison.

**Exception:** None.

## Vertical increments, not horizontal layers

The [iteration workflow](../../../AGENTS.md#workflow-develop-validate-replan) makes each increment earn its place through a consumed interface. Building several unvalidated layers together can leave a complete-looking subsystem reachable only through fakes.

**Not this:** A subsystem whose features are exercised only through its own fakes while the daemon consumes a small fraction of its interface.

**Exception:** None.

## Keep scenarios coherent and failures diagnosable

The [test structure guidance](../../../AGENTS.md#structure) allows a coherent journey to verify several related transitions. Identify the step and input when a check fails. Split independent cases when that improves diagnosis or state isolation, and share immutable build artifacts and setup where safe. Separate test cases need not become separate executables or guest builds.

**Not this:** Rebuilding the same guest for every assertion in a lifecycle, or combining unrelated cases whose shared mutable state makes failures depend on execution order.

**Exception:** A case whose purpose is to test clean initialization or rebuilding needs fresh state or outputs. Ordinary behavior checks can reuse immutable artifacts.

## Doubles isolate; they don't cover

A stub, fake, or mock lets a test exercise the code above a boundary without the thing below it. That is its whole contribution. The replaced component is exactly as tested as it was before, so give it its own tests, and the concerns its consumers rely on (ordering, fencing, error variants) are the ones to cover first.

**Not this:** A crate whose store is only ever a fake, described as tested because the fake's callers are.

**Exception:** A third-party service, a slow one, or one that takes more than a simple container to run is replaced by a recorded or simulated double in the default suite, and its contract is covered by a small number of tests against the real service run on demand.

## Ship a red test with its fix

The [prohibition on committing known failures](../../../AGENTS.md#absolute-prohibitions) keeps the suite usable as evidence. An ignored regression hides a defect, while an inverted assertion teaches future maintainers that the defect is intended.

**Not this:** A test asserting that a request returns the wrong status, with a comment saying the right status is coming later.

**Exception:** None.

## A fast suite is a feature

Choose the least expensive level that catches the failure while retaining real integration coverage of the SDK against the host, wasmtime, pre-initialisation and execution. Replace an expensive end-to-end test when a cheaper test proves the same contract; keep end-to-end coverage for failures only the assembled product exposes. Account for fixture preparation and linking as well as test execution, and follow the [validation workflow](../../../AGENTS.md#workflow-develop-validate-replan) when selecting checks for a change.

**Not this:** A scenario suite measured in minutes that developers skip, standing in for integration tests that would have run in seconds.

**Exception:** A slow test that proves something nothing faster can stays in an explicitly selectable suite, required for changes affecting its contract and for the release it qualifies. Selection must report its scope; it must not silently skip missing prerequisites.
