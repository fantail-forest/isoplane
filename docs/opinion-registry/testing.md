# Testing

## Keep the suite fast

Give a change the one test that would catch its regression most cheaply, not a test at every level. Test logic you can call directly with a unit test. Use an integration test when the bug would be in how real components work together. Add an end-to-end test, through `isoplaned` or the `isoplane` command, only when nothing cheaper exercises the behaviour, and extend an existing one before adding another. Don't rebuild what has already been built, and let independent tests run in parallel.

## Go deep where failure is costly

Cover critical behaviour thoroughly: the sandbox boundary, budget enforcement, fencing, hibernation and wake, tenant isolation. Don't write tests that are unlikely ever to fail.

## Test the real code

Make sure tests exercise the real code, not some bullshit you made up.

## Any test can run on its own

Tests should reuse each other's work rather than repeating it, but a test must pass when run by itself, without another test having run first. A test of starting from nothing leaves out only what it checks.

## Failures point at the cause

Use assertions that show expected and actual values. When a test checks several steps, a failure says which step.

## Tests are deterministic

Use a clock the test controls instead of the wall clock. Enforce ordering with explicit synchronization, such as holding one task while checking another proceeds, instead of timing. Leave performance to [benchmarks](measurement/benchmarking.md). Asserting fixed quantities such as sizes, counts and configured limits is fine.

## Check that a new test can fail

Before relying on a regression test, see it fail against the bug, or with the guard it covers disabled.

## Known failures are marked ignored

A test for a bug that isn't fixed yet may be committed marked ignored only when the owner has explicitly decided to defer the issue. Record that decision and the reason for ignoring it. The owner also decides whether a reported issue is a non-issue; an agent must not make either decision by suppressing a failure. Don't commit a test that fails in a normal run, and don't write one that asserts the buggy behaviour.

## Build static compiler outputs in advance

Any static compiler outputs a test needs, such as packages, guest and runtime modules, the compiled runtime, installed tools and built fixture projects, are built by a Task step before the tests run. They are kept in `.build/` and rebuilt only when their sources change. Tests don't modify them; copy one to a temporary folder to change it. A test of a build starts from a fixture Task built and runs only the build step it checks.

**This, not that:** a test copies a fixture project Task built and runs the CLI Task installed, not one that generates a project, installs the CLI and restores packages before the step it checks. An engine test loads the runtime Task compiled, configuring its engine with the test configuration that compile was made for, not one that compiles the 66 MB runtime module itself.

**This, not that:** a test that needs different application code selects it by route or setting in a fixture Task built, not by rewriting the copied fixture's source, which makes every run compile it again. Edit the copy's source only when how a build or reload reacts to the edit is what the test checks.

## Test with fixtures, not shipped templates

Tests of the SDK and platform build fixture projects kept with the tests. Use a shipped template only in a test of that template.

## A missing input fails the test

When something a test needs hasn't been built, the test fails and names the command that builds it. Only tests marked ignored are skipped.

## Keep hostile guests

For every way a guest could try to escape or abuse the sandbox, keep a guest that tries it. That includes forbidden imports and features, exhausting a limit, and misusing host calls. Give limits that involve time short values in these tests, so they finish quickly. Run them on every change to the engine, validator or host interface.

## Pack fixture apps

One fixture app can cover several behaviours. Split it only when those behaviours would interfere with each other, or to stay within a size limit. Don't add test-only behaviour to templates or samples customers copy from.

## Don't test third-party services

When our code talks to a third-party service, test our side against a stand-in for it, not the service itself.

**This, not that:** a test that runs a library such as Npgsql inside the sandbox against a local PostgreSQL container tests Isoplane, so keep it; don't delete it as a test of third-party code.

## Clean up outside the work tree

Tests may write temporary files wherever keeps them fast, including local disk outside the work tree. Anything outside the work tree is removed as soon as nothing needs it, by the end of the test run at the latest. Output in `.build/` may stay when later runs are likely to use it.

## Ask the owner for acceptance criteria

Ask before inventing product behaviour or performance targets for a test to check.

## Run on a developer machine

The suite runs on a developer machine with the toolchain installed. It may start containers to test against, but must not need to run inside one.

## Put tests where they belong

Unit tests go in the module, integration tests in `crates/<crate>/tests/`, end-to-end scenarios in `scenario-tests/`, and shared setup in each crate's `tests/common`. Name a test as a sentence stating its behaviour.
