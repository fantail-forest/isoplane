# AGENTS.md

> Read this file in full before writing any code.
>
> **Not all existing code follows these conventions yet.** That doesn't reduce
> their authority. New and modified code must conform. Bring directly affected existing code
> into compliance with small, relevant refactors; report larger debt separately
> rather than expanding the task into an unrelated migration.
>
> The rules below follow the **pit of success** philosophy: the correct path
> must be made to be the easiest path. Structure code so that developers and
> agents naturally fall into doing the right thing. When a mistake keeps
> happening, fix the default, not the developer. Apply this to every decision,
> including ones this document doesn't cover.

## Project Overview

**Isoplane** hosts customer-written .NET applications as WebAssembly guests. The
isolation boundary is the WASM sandbox, not the CLR and not a hypervisor.
Guests are pre-initialised at deploy time into a memory template, activate
copy-on-write from it, park with an empty stack while waiting on host I/O, and
can be hibernated as a delta against the template and woken later. The host is
Rust; the guest SDK is C#.

## Architecture

Two Cargo workspaces. The root workspace holds the host crates. `guests/` is a
separate workspace because guests target wasm32 under a size-tuned profile and
share nothing with the host build but the contract crate.

Keep host crates depending downward only: the contract crate depends on nothing
in the workspace, the engine and cell layers know nothing of HTTP, deploys, or
tenants, and only the daemon wires layers together.

Give every crate a `description` in its manifest; that is the crate inventory:

```sh
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | "\(.name): \(.description)"'
```

## Tooling Quick Reference

| Tool                     | Configured in                                               | Notes                                                                                                                                            |
| ------------------------ | ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Rust**                 | `Cargo.toml` (`rust-version`), `Dockerfile`             | `rust-version` declares the minimum supported version; the Dockerfile pins the build toolchain. Nightly is never required for build, test, lint, or docs.                                                                                |
| **wasmtime**             | `Cargo.toml`                                            | One version across the workspace, shared by preparation and execution.                                                |
| **Build orchestration**   | `Taskfile.yml`, `taskfiles/`                             | Use Task for repository build dependencies and preparation; native build tools own compilation and incrementality. |
| **.NET SDK**             | `global.json`, consumed by `Dockerfile`                         | Policy: pin one SDK version. The image installs the version declared in `global.json`.                                                                                                           |
| **wasi-sdk, wasm-tools** | `Dockerfile`                                            | The C# guest linker and the wasm inspection tool.                                                                                                |
| **Lint**                 | `cargo clippy --workspace --all-targets -- -D warnings` | Full Rust gate before merging or releasing code changes; lint affected packages during development. See the validation workflow. |
| **Format**               | `rustfmt.toml` (root and `guests/`)                     | Check affected workspaces during development; `cargo fmt --all --check` in both workspaces is the full format gate. Default rustfmt settings; never `#[rustfmt::skip]`. |
| **Tests**                | `cargo test`                                            | Prepare prerequisites through Task before running tests. Fixture helpers resolve and load prepared artifacts. |
| **Database** | PostgreSQL for relational integrations | Use the PostgreSQL protocol; local PostgreSQL containers are suitable for tests. See the relational-storage opinion. |
| **HTTP**                 | hyper 1, tokio                                          |                                                                                                                                                  |
| **Wire formats**         | postcard (guest boundary, deltas), JSON (operator API)  |                                                                                                                                                  |
| **Logging**              | `tracing` + `tracing-subscriber` (JSON)                 |                                                                                                                                                  |
| **Docs site**            | Docusaurus 3 (`docs/userdocs`)                          |                                                                                                                                                  |
| **Container**            | `scripts/dev --container`, `Dockerfile`                             | A convenience that carries the whole toolchain. Never make crates, tests, scripts, or docs require, detect, or gate on it.                    |

## Common Operations

```sh
scripts/setup                                     # Initialize submodules and install Task/native tools
python3 scripts/tasks.py --list                    # List build and release tasks
scripts/dev <command>                              # Run a native development command
scripts/dev                                       # A development shell
scripts/dev --container --build                    # Optional Linux toolchain image
scripts/dev --container --stop                     # Remove your container when done

cargo build --workspace --all-targets              # Build crates, tests, examples
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check && (cd guests && cargo fmt --all --check)   # The format gate

scripts/dev scripts/test --exclude-binary isoplane-native/release_journey # Rust suite, parallel binaries
cargo test --workspace                             # Everything, including release-installation journeys
cargo test -p isoplane-host                            # One crate
cargo test -p isoplane-host --test cell                # One integration test binary

scripts/dev guests/build.sh                        # Rust guests -> .build/wasm32/rust-guests/*.wasm
scripts/dev guests/build-dotnet.sh --if-stale      # C# guests -> .build/wasm32/dotnet/mono/*.wasm

cargo run -p isoplaned -- --config isoplaned.toml          # Run a node
cargo run -p isoplaned -- --config isoplaned.toml --check  # Validate a config without running
isoplane deploy --app <name> --project <directory> --team <name>
isoplane doctor <wasm>             # Validate a module locally

cd docs/userdocs && npm run start                  # Serve customer docs

scripts/clean                                      # Reclaim build intermediates
```

**IMPORTANT**: Redirect long-running command output (`cargo build`,
`cargo test`, the guest builds) to a file and read it after the command
finishes. Don't pipe into `head`, `grep`, or `tail` and re-run when the filter
misses.

**IMPORTANT**: Wait for long-running operations to complete; do not repeatedly
peek at log files, poll process or job status, or narrate incremental output
while they run. Use a completion notification or a wait that returns on exit,
and arrange a bounded timeout to wake you if the operation hangs or its
completion monitor fails to notify you. Choose that timeout from prior observed
runtime or a reasonable estimate for the operation, with margin; do not use
short, recurring checks as a substitute. Do independent useful work while it
runs, or wait. Inspect the exit status and saved output on completion. If the
timeout expires or a failure notification arrives, inspect enough to diagnose
the problem and choose a concrete recovery action or a revised, justified
wait; do not fall back to repeated progress polling.

**IMPORTANT**: Prepare guest fixtures, runtimes and packages through Task before starting the test runner. Shared fixture helpers resolve and load prepared artifacts; they do not schedule builds, decide freshness or coordinate build writers. Keep preparation dependencies and reuse policy in the build tools. Missing prerequisites fail the selected test with an actionable preparation command; never silently skip. See the [tooling opinion](docs/opinion-registry/workflow/tooling.md#prepare-test-prerequisites-in-the-build-graph).

Release-installation and package-artifact checks consume explicitly prepared artifacts; they do not build releases, create source worktrees or change versions as incidental setup. Tests of a build or installation workflow may invoke that workflow with isolated inputs. Check alternate host build configurations in explicit build tasks with reusable outputs, rather than recursively running Cargo test suites inside a test.

Reuse valid build outputs and completed checks, and let the build tools and test runners schedule independent work in parallel. Follow the [reuse and concurrency guidance](docs/opinion-registry/workflow/tooling.md#reuse-valid-work); keep invalidation and serialization scoped to the work that requires them.

**IMPORTANT**: `scripts/dev --container` runs you as your own uid and only ever removes
containers it created. Don't drive Docker directly to work around it, and don't
leave a container running when you are done.

## Dependency Management

- Declare shared dependencies once in `[workspace.dependencies]` and reference them with `workspace = true`. A version string in a crate manifest is a bug.
- Set dependency feature flags in one place and check the resolved dependency graph for unintended features. Cargo features determine compiled support; engine configuration and module validation determine guest capabilities. Explicitly disable forbidden capabilities regardless of feature unification, reject engine construction if capability probes validate, and test that rejection. Run hostile modules through the real validator and engine to prove guests cannot obtain forbidden capabilities.
- Use the language's standard library and runtime for operations they already cover well, and maintained libraries or tools for other established problems. This applies across product code, tests, builds, releases and repository automation, including date and URL parsing, serialization, command-line parsing and help. Use supported APIs, configuration and extension points before writing a substitute; keep custom code focused on product-specific needs or concrete gaps, following the [libraries and tooling opinion](docs/opinion-registry/workflow/tooling.md).
- Choose tools and libraries in this priority order: active maintenance, adoption/support, lightweight operation, then performance. Apply this to standalone tools and managed libraries as well as Rust crates; an existing integration is not a reason to skip evaluating its maintenance burden. Include the custom code, tests and operational state a tool would replace in the comparison.
- Run a tool that needs a different wasmtime, target, or profile in its own workspace or its own process; never link it into the host build.
- Share only the contract crate, by path, between the guests workspace and the host workspace.

## Code Quality Rules

These rules are **non-negotiable**. If you can't satisfy a requirement without breaking one, **stop and explain** instead of taking a shortcut.

### Absolute Prohibitions

| Rule                                                                                                                                            | Exceptions                                                                                                                                                         |
| ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **No unexplained or blanket lint suppressions.** | Fix meaningful warnings. When satisfying a lint would worsen the code, a narrowly scoped allowance with a reason is permitted. Configure a lint in `[workspace.lints]` only when the rationale applies to the whole workspace. |
| **No `unwrap()` / `expect()`** in non-test code                                                                                                 | `expect("reason")` where the invariant is established in the same function and the reason says so.                                                                 |
| **No `panic!` as control flow** in the host or daemon                                                                                           | Test code. An uncontained host panic can disrupt other tenants; contain unwinding at the execution boundary.                                                                                                        |
| **No assertions on wall-clock time** in tests | A deadline that fails a hung test rather than letting it hang. Benchmarks, which are not tests. Tests take a clock and advance it. |
| **No stubs in application code.** Leave unimplemented code absent, so a caller fails to compile, not at runtime. | A method a foreign trait forces, or a wire case required by a concrete supported consumer or explicit compatibility requirement, may exist, and must return an error naming what is unsupported. Never a fake value, a hard-coded zero, or an injected sleep. Test code may stub freely. |
| **No configuration without a consumer** (config field, CLI flag, cargo feature, trait method, enum variant with no non-test caller or producer) | An obsolete field retained for a concrete supported consumer or explicit compatibility requirement. Name that requirement where the field is declared, and make loading it explicit: accepted and ignored with a warning, or rejected with a migration message. Never silently read into nothing. |
| **No hand-mirrored tables** (an enum or constant table duplicated in another crate or language)                                                 | A generated copy, or a copy with a test that fails when the two differ.                                                                                            |
| **No `unsafe`** except for operations that are unsafe by nature (FFI, memory mapping, deserialising native code)                                | Give every such block a `// SAFETY:` comment naming the invariant and where the caller enforces it. Put `#![forbid(unsafe_code)]` in every crate with no such operation. |
| **No C# `DllImport` or `unsafe`** outside the SDK's one interop layer | None. |
| **No test that cannot fail** (prints a figure and asserts nothing; asserts a constant against itself; downgrades a miss to a pass by default)   | None.                                                                                                                                                              |
| **No test that is known to fail** committed without the fix that makes it pass | None. Ignoring a known failure or asserting the defect is prohibited. |

### Types

Use the type system so that misuse is a compile error.

- **Newtypes for identifiers and units.** `InstanceId`, `TenantId`, `CpuNanos`, `PageCount`. A bare `u64` parameter is a bug waiting for a caller to swap arguments.
- **Typed errors per crate.** Define each crate's error as an enum callers can match on; keep untyped, catch-all errors to the outermost layer. Match on variants, never on message text.
- **Prefer exhaustive matches.** A `_ =>` arm over a workspace-owned enum hides the variants a future reader should have to consider. Use one only where a new variant genuinely shouldn't revisit this site, and say so.
- **A variant needs a producer.** Add an enum variant, dispatch kind, or import in the commit that adds the code constructing it and the test observing it, not before.
- **Keep input types minimal and output types exhaustive.** Make config structs describe only what is read and result records the full known shape.

### Naming

| Construct                 | Convention                                          | Example                                       |
| ------------------------- | --------------------------------------------------- | --------------------------------------------- |
| Crates                    | `isoplane-<noun>`                                       | `isoplane-proxy`                                  |
| Files, modules, functions | snake_case                                          | `future_table.rs`                             |
| Types, traits             | PascalCase, no `I` prefix                           | `Activator` ✓, `IActivator` ✗                 |
| Numeric values            | unit in the name                                    | `budget_ms`, `delta_bytes` ✓, `budget` ✗      |
| Tests                     | a sentence stating the behaviour                    | `a_tombstoned_instance_refuses_the_next_call` |
| C#                        | .NET conventions; public API in `Isoplane.*` namespaces | `Isoplane.Sdk.Request`                            |

### Module Organization

- **Make a crate a boundary worth compiling separately.** A crate earns its place by an interface worth isolating: a toolchain or target of its own, a dependency set the rest of the tree shouldn't carry, or a responsibility whose consumers shouldn't see its internals. A crate that exists only to hold files is a directory with ceremony.
- **Group by purpose, not by kind.** Keep a feature's types, logic, and tests together. No `types.rs` every module reaches into.
- **Prefer deep modules.** A module, crate, or trait earns its place by hiding complexity behind a simple interface. If the interface is nearly as complex as what it hides, it is moving code around, not reducing what callers need to know.
- **Isolate infrastructure behind responsibility-focused interfaces.** A concrete module can provide that boundary. Add a trait when actual substitution, test isolation, or ownership needs justify it; do not design for hypothetical backend replacement. Keep the interface small and hide the mechanism from consumers. A trait nobody consumes is a specification, not code.
- **Split files by responsibility, not by size.** Separate queue, lifecycle, and socket responsibilities when they have distinct interfaces. Keep unit tests with the responsibility they exercise.
- **Watch for architectural momentum.** Adding to a module because "that's where similar things go", past the point where a split would fit better, ossifies the wrong shape.

### Code Structure

- Keep functions small and at one level of abstraction. Orchestration and byte-fiddling don't share a body.
- Extract shared code when the copies represent one responsibility that should evolve together. Similar syntax or a repetition count alone does not justify coupling independent behavior. Apply the same judgment to test harnesses; avoid configurable shared setup that is harder to understand than the duplication.
- Observability should not cost the hot path its shape. Prefer instrumentation that attaches at a boundary and nests: a span guard that records on construction and drop, context carried on the span rather than in parameters. Boundaries nest, so spans do too. Threading an observer through signatures is acceptable when nothing cleaner reaches what must be measured; leaving something important unmeasured because the clean way isn't there yet is not. Don't fork a method into instrumented and plain variants; make the one method observable.
- Multiple implementations of one interface are fine when each has a purpose: production, a feature behind a flag, branch-by-abstraction during a migration, or an experiment. Cover each with the same tests, exercised through the interface it implements, so none is the one that only production runs. When an experiment decides, or a migration completes, remove the losing implementation then; don't leave it selectable.

## Comments & Documentation

- **Comments cost something.** Every comment costs time to maintain and carries the risk of rotting into something misleading, and a wrong comment is worse than none. The default is no comment; write one only when what it adds clearly outweighs that cost. Code that needs many comments to be understood is usually structured poorly; make the code readable first.
- **Doc comments are required on public interfaces**, and doubly so on the SDK surfaces customers consume. Make a doc comment get to the point and give a developer who already knows the system exactly what they need about this item: what it does, what it returns, what it fails on, anything non-obvious. No more.
- **Comments explain _why_, not _what_.** The code says what it does.
- **Comments are not the place to accumulate a historical record.** Rationale for the current shape is welcome, including that an alternative was tried and lost. Narration of what changed is not: no "used to", no "this replaces", no commit hashes, no changelogs inside function bodies. Comments must never describe the edit history of a file (`// changed foo to str`). That's what git is for.
- **Prefer to take responsibility over delegating it via a normative statement.** A comment that says MUST, never, always, or invariant hands a guarantee to the reader that the code could hold itself. Restructure first: enforce the rule through ownership, the type system, or the interface. A normative comment survives only where something breaks if the rule is not followed and no restructuring can enforce it, and then it says what breaks in the same sentence.
- **Measurements are rationale, not properties.** Quote one where it explains a non-obvious decision, and prefer a comparison under a described load ("X was five times faster than Y under Z") to a bare figure, which reads as a guarantee once the machine that produced it is gone.
- **Documentation costs something.** Every page costs time to maintain and carries the risk of rotting into something misleading, and wrong documentation is worse than none. The default is no page; write one only when what it adds clearly outweighs that cost. A system that needs many pages to be understood is usually structured poorly; make the system understandable first.
- **Choose a home by maintenance intent.** Keep maintained architecture, user documentation, internal checklists and runbooks in `docs/`. Use repository Discussions for phase-specific research, experiment reports, provisional ideas and open questions whose usefulness has a foreseeable end. Issues track actionable work; their comments retain execution evidence. Follow the [documentation lifecycle opinion](docs/opinion-registry/workflow/documentation-lifecycle.md), including preserving links when moving content and feeding documentation friction back into product development.
- **Docs describe current behavior and its limits.** Support claims with the implementation and validation appropriate to the claim; an explanation does not need a new test for each statement. Customer-visible guarantees still need coverage through their real interfaces. No "in production this would", no roadmap prose in reference pages, no thesis restated across several files.
- **Generate reference docs from doc comments and definitions in code.** Updating an API reference, a config reference, or a capability table must never need a manual edit to a docs file. Generate pages as a documentation-build prerequisite, or check deliberately retained generated copies for drift on the stable toolchain. Pages generated for the build need no separate drift test. Guides and tutorials are written by hand and are expected to need manual edits to stay in sync.
- **The original project spec is history.** It is not authoritative. Decisions are visible in maintained opinions, code and the commits that made them.
- **Shell scripts and `--help` text are not essays.** Keep a header to what the script does and how to invoke it.

## Testing Standards

Behavior changes need coverage that detects their absence or regression. Extend or reuse existing tests when they cover the contract; add a separate test when it contributes distinct failure detection. Judge that contribution against build, execution, and maintenance cost.

Validate repository build, setup, cleanup and release scripts by running the real commands; do not maintain tests of build-script machinery.

### Principles

1. **A feature is proven at the level that proves it, in a suite that stays fast.** Validate critical integrations (the SDK against the host, wasmtime, pre-initialisation, the execution engine) with integration tests through their real interfaces. Validate behaviour a customer sees end to end through `isoplaned`'s listener or the `isoplane` command when that runs in reasonable time, with a small number of such tests rather than one per change. A fast suite is a feature of the codebase: replace a slow end-to-end test whenever an integration test could cover it. Code reachable only from fakes or benchmarks is still not a feature.
2. **Tests are deterministic.** No elapsed-time assertions, no ratios of timings, no "A finished before B". Assert order with explicit sequencing; make time a clock the test controls.
3. **Exercise critical rejection paths.** Every sandbox rejection rule and engine limit needs a test that trips it through the real boundary. Cover other guards according to the correctness, security, or contract failure they prevent. Existing tests may exercise several checks; a conditional does not automatically require its own test.
4. **Tests assert logic; benchmarks measure.** Tests may assert deterministic quantities and configured limits, including byte sizes, resource accounting, and deadlines advanced with a controlled clock. Machine-dependent performance thresholds belong in benchmarks outside `cargo test`. Performance regression gates follow the [benchmarking opinion](docs/opinion-registry/measurement/benchmarking.md), including owner-approved thresholds, comparable baselines and explicit noise handling.
5. **Doubles isolate; they don't cover.** Stubs, fakes, and mocks are welcome in unit tests, and in integration tests that would otherwise reach a third-party service, a slow service, or one that takes more than a simple container to run. A test that exercises a double adds no coverage to the component it replaces; give that component its own tests, especially over the behaviour its consumers depend on.
6. **Acceptance criteria come from the owner.** Ask before inventing product behaviour or performance targets. Derive boundary cases and regression tests from approved requirements, established contracts, and the invariants here without further approval. The original project spec and benchmark results do not establish requirements.
7. **The suite must not require a container to run within.** It runs on a developer machine with the required toolchain. It is fine for the suite to provision temporary infrastructure as containers to test against. Follow the shared fixture build rules under Common Operations.

### Structure

- Put unit tests in the module (`#[cfg(test)] mod tests`), integration tests in `crates/<crate>/tests/`, and scenario tests in a top-level directory named for what they are, `scenario-tests/`, driving a real `isoplaned`. A directory called `tests/` at the root says nothing about what kind of test it holds.
- Give each test a coherent behavior or journey and make failures identify the failed step. Related transitions and assertions can share a scenario; split independent cases when it improves diagnosis or isolation. Separate cases do not require separate binaries or guest builds: share immutable artifacts and setup while isolating mutable state.
- Prefer assertions that report useful actual and expected values. Add a message when the test name and assertion leave the failure unclear; routine comparisons need no extra prose. Do not replace an equality or collection assertion with a boolean comparison just to attach a message.
- Name tests like a spec: `a_websocket_survives_its_instance_hibernating`.
- Give each test that writes files its own uniquely created temporary directory, never a pid-based or shared path. Prepared fixture artifacts may be shared read-only under the rules in Common Operations.
- Keep shared harness code in one `tests/common/` per crate, not copied into each file.
- Make every scenario check an assertion. Prose explaining what a check would mean is not a check.

## Git & Commit Conventions

- Work in `main` unless you are asked to do otherwise.
- If you are asked to work in a worktree, create a topic branch for your work in that worktree.
- If you think that the work that you're doing belongs in a different branch or worktree, just ask.
- Write all commits in [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>)[!]: <description>
```

- **`type`**: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `ci`, `perf`, `build`.
- **`scope`**: the crate or area touched, by its short name (`engine`, `sdk`), comma-separated for several.
- Keep commits atomic. Never commit generated artifacts (`target/`, `.build/wasm32/rust-guests/`, `*.wasm`, `obj/`, `bin/`, `docs/userdocs/build/`).

## Workflow: develop, validate, replan

Work happens iteratively. Each iteration of the planning cycle goes develop, validate, replan.

The project is undeployed and pre-release. The owner has authorized breaking API, ABI, SDK and stored-format changes without legacy readers or data-migration support until that status changes. Preserve this fact in handoffs and future context checkpoints. This authorization does not change product scope or waive the four invariants; EF migrations still own the current database schema.

Retain obsolete APIs, fields, formats or compatibility adapters only for a concrete supported consumer or explicit compatibility requirement. Internal fixtures and test helpers change with the implementation; their old shape alone does not justify compatibility machinery. Remove obsolete support within the affected scope when its requirement ends.

- **Plan one iteration at a time.** Keep the plan for an iteration narrow: specify what this iteration needs and nothing about how later ones will be carried out, unless that detail is critical to the work at hand. Put longer-range goals in the plan only as rationale for its decisions ("we are doing X because we are working toward Y"), never as instructions. A goal many iterations away is a hope, and hopes are not planned against.
- **Develop** against the current iteration of the plan. Build the smallest change that can be validated through the interface it changes: a real entry point when the change is customer-visible, the integration it touches otherwise. Not a layer, not a module, not "the plumbing for" something.
- **Validate** is a retrospective. Review whether the work done during development achieved the iteration's goal: run the tests that prove it, on this machine, and read the result. Then reflect on the deviations from the plan that were necessary to reach that goal, why they were necessary, and what they say about the plan. If the goal can't be demonstrated with the tools available, the iteration is not done, and saying so is the deliverable.
- **Replan** with what the first two phases taught. Rework the plan only when a lesson warrants it: that may mean a different path for the steps ahead, or revising the goal itself. Don't carry forward a plan that validation contradicted; leave alone a plan it confirmed. Explain a decision that changed in the commit that changes it.
- **Stop and report** when an iteration needs a breaking change to a shared contract (the host interface, the PAL, a wire format, the config schema) outside existing owner authorization, when a rule here would have to be broken, or when the task as given conflicts with what validation showed.
- **Scope validation to the change.** During development and before committing code, build and test the affected behavior, lint affected Rust packages and targets, and check formatting in affected workspaces; use the relevant checks for other languages. Run the full workspace lint and format gates before merging or releasing code changes, and during development when shared changes make narrower checks insufficient. Reuse passing results while their inputs remain unchanged.
- **Done** means the applicable validation passed and the report says exactly what was verified and what was not. For prose-only changes, review the diff, check links and consistency, and run `git diff --check`; builds, lints and runtime tests are unnecessary unless executable examples or generated references changed.

## Workflow & Architecture Notes

### The four invariants

Derive every host mechanism from these. A change that breaks one is a product change, not an implementation change, and needs the owner's agreement before code.

1. **Keep all tenant state visible.** At a supported return boundary, represent resumable state as sandbox state plus explicit records of what the host holds on its behalf. Native invocation stacks are not checkpoints, and host-owned resources need explicit preservation, rebinding, or loss semantics.
2. **Keep every wake source host-owned.** Put sockets, timers, and futures in the host or the proxy. A sleeping guest retains handles; the host owns the resources and their lifecycle, including any cost while the guest sleeps.
3. **Suspend only on an empty stack.** A guest parks only by returning control to the host with no live guest invocation left on a native stack. Capture supported sandbox state at that boundary, with the resource semantics above. Provide no way for a guest to block inside a call. How that is achieved (today, no blocking import and synchronous execution on pool threads) may change; the property may not.
4. **Route every capability through the host interface.** Offer everything a guest can do to the world, including every form of I/O and anything a syscall would provide, through an interface the guest cannot bypass, so that what a module can reach is a static question, answerable at deploy time.

Local-development debugging may suspend a worker thread while a debugger holds a live guest invocation. This exception does not apply to production execution or permit capturing or migrating the paused invocation as a checkpoint.

### The host interface

Keep the host interface to one definition. Generate every other representation of it, in another crate or another language, from that definition, or drift-test it against that definition. Ship any change to it with those representations updated and the hostile-module corpus passing. A breaking change, one that an existing guest or template would no longer satisfy, needs the owner's agreement; the pre-release authorization above supplies that agreement within the approved task scope. Don't change it as a side effect of another task.

### Guest implementations in this repository

- `guest-sdks/rust` is internal guest-test support, with no customer SDK compatibility or feature-parity commitment. Retain helpers where they simplify current fixtures, including hostile guests, and evolve them with those tests.
- Keep a guest's purpose clear. Samples show what a customer would write and may also provide regression coverage without accumulating test-only complexity. Dedicated fixtures stay as small as the behavior under test allows; related probes may share a module or runtime when their state and outcomes remain distinguishable.
- Give every rejection rule in the validator and every limit in the engine a hostile module that trips it, and run that corpus on every change to either.
- Automate project setup for a C# guest in ways that feel native to the .NET ecosystem: a `dotnet new` template, MSBuild targets and code generation in the SDK package, source generators for the exports a guest must provide. A customer never copies boilerplate from a README into their project.

## Environment & Configuration

- Keep configuration layered: let a value come from more than one source (a file, the environment, a flag), give the sources a defined precedence, and let the daemon print the merged result it is actually running with. Which sources exist and how they rank is the daemon's decision, not a rule here.
- Never commit secrets. Make anything that holds a development-only key or credential refuse to run anywhere it could be mistaken for production.
- Treat every template and derived snapshot as potentially secret-bearing. Applications choose initialization-time or activation-time configuration/secret reads; changes to any guest-readable configuration trigger redeployment and regeneration of captured state, even with unchanged code. See the [isolation opinion](docs/opinion-registry/production/isolation.md).
- Use PostgreSQL and EF Core for managed relational integrations, with EF migrations owning their schemas. Keep persistence and application settings behind consumed interfaces. .NET User Secrets may supply local development configuration; they are not encrypted vault storage. See [relational storage](docs/opinion-registry/frameworks/postgresql.md).
- Expose a port, hostname, limit, or deadline as configuration when there is an operational need to vary it, and validate the supported range. Fixed policy and protocol facts may remain constants. Define a shared concept's value or default once in its owning crate; unrelated policies may have equal values without sharing a definition.
- Never default identity. Treat a tenant, org, or app that a request did not establish as an error, not as `"unknown"`.

## Error Handling

- **Never swallow errors silently.** No bare `let _ =` on a `Result`, no empty `catch` in C#. Handle, propagate, or log with context. Where discarding a failure is genuinely by design, discard it with a stated reason.
- **Preserve distinctions that recovery depends on.** Keep missing, corrupt and temporarily unreadable state distinguishable when they require different actions. A failed read is not evidence that data is absent or disposable; logging the failure does not justify deleting or overwriting it.
- **Give errors context**: which tenant, which instance, which operation, what limit. `"budget exceeded"` is not an error message.
- **Don't catch what you can't handle.** Let traps propagate to the layer that decides the response.
- **Match errors by variant.** Code that inspects an error's text to decide what happened has the wrong error type.
- **Fail fast at startup.** Parse and validate static configuration before acquiring service resources, including binding listeners or opening stores; report all independently detectable configuration problems together. Then acquire resources and validate dependencies and store schemas before accepting traffic. If either phase fails, report the failure and release acquired resources rather than serving with partial initialization.
- **Fail contained at runtime.** Let an error in one request, dispatch, or deploy end that operation, never the process.
- **Make every handled terminal error observable.** Record its context and outcome at the boundary that ends the operation or refuses startup. For operations with a completion event, include the error there rather than emitting a duplicate completion. The limits of this guarantee are defined under Observability.

## Observability

- Prefer wide events: one structured completion enriched with the context and outcome of a logical operation. Use nested spans for stage timing and standalone metrics or plain log lines where they fit better. Never log secrets, tokens, keys, or guest memory in any of them. See the [observability opinion](docs/opinion-registry/production/observability.md) for rationale and examples.
- A dispatch is one work item delivered to an instance (a request, a frame, an alarm), from delivery to its final outcome, across every hot interval it takes between parks. Record one wide completion event per dispatch, with attribution (who, what), outcome, CPU across its hot intervals, and wall time, for every outcome the host handles, including cancellation and caught unwinding. The execution boundary owns panic containment and finalization; parking does not complete a dispatch. Treat that record as the source of truth for load, error rates, and resource usage. This is an in-process emission requirement, not a guarantee of durable delivery: aborts, forced termination, process crashes, and exporter failures can leave missing records. Do not infer successful completion or zero usage from a missing event.
- Make where a dispatch's time went observable per request, as nested timed stages, so latency can be decomposed without reading logs. Keep one tracing mechanism in the host for that; don't build a second, bespoke one beside it. How tracing is done may change; that it is one mechanism does not.

## Opinion Registry

The opinion registry at `docs/opinion-registry/` captures how we build software here: architecture, testing, the frameworks we depend on, measurement, and production practice. Start with `docs/opinion-registry/philosophy.md`, then read the section for the area you're touching. `docs/opinion-registry/INDEX.md` maps topics to files.

Opinions define the direction, not the status quo. When existing code doesn't follow an opinion, new code follows it anyway.

**Before writing code**, read the registry section for the area you're about to touch.

If this file and the registry conflict, notify the owner with links and the
conflicting text, explain which decision is blocked, and wait for the owner to
decide. Neither source wins by default. Continue independent work only when it
does not depend on that decision. Record the resolution in the affected guidance
so the conflict does not recur.

## Agent Behavior Guidelines

1. **Correct consequential factual errors immediately.** If the owner states something materially incorrect, say so before planning or implementing anything that depends on it. Identify the false premise, explain the correction and why it matters, and support it with code, a test, or an authoritative source when available. Distinguish established facts from uncertainty; if unsure, flag the uncertainty immediately and verify before relying on the claim. Do not treat confidence or authority as evidence, silently accommodate a false premise, or defer the correction to the final report. Correct facts without recasting preferences or product decisions as errors. Continue independent work while resolving the premise.
2. **Understand before you act.** Read the code, the registry section for the area, and the recent history of the files before modifying anything. Check for an existing pattern before inventing one.
3. **Keep the change focused.** Apply the cleanup scope at the top of this file. Report what you refactored and why; do not add unasked-for features.
4. **Ask about consequential ambiguity.** Ask when requirements leave product behaviour undecided, a rule would need an exception, or a shared contract would break. Make routine implementation choices within established requirements and explain material assumptions. Existing authorization applies; do not ask again for an already approved change.
5. **Respect existing patterns within these standards.** Improve conflicting patterns in directly affected code within the cleanup scope above; report larger migrations separately.
6. **Report faithfully.** Follow the validation workflow above. Report failing checks with their output and skipped checks as skipped. Put edit history and change rationale in the commit or PR; durable design rationale may live beside code under Comments & Documentation.
7. **Prose is not a replacement for a working implementation.** When implementation is requested, deliver the change and the test that proves it. A README, design note, or issue does not substitute for them.

## Agent skills

### Issue tracker

Use the repository issue tracker. Apply the acceptance-criteria rule under Testing Standards. Ask in the current conversation unless posting to the issue is authorized.

Follow the [issue-tracking opinion](docs/opinion-registry/workflow/issue-tracking.md) when creating, restructuring, triaging or closing issues. It defines tracker checkboxes, functional grouping, milestone scope, evidence and where status belongs.

### Measurements

Measurements inform decisions; they are not static properties of the system. Keep a result with the decision it informed: the commit message or the comment on the constant it justified, usually as a comparison under a described load. Treat bulk benchmark run records as build outputs; retain maintained workloads, reproduction instructions and selected regression baselines with their provenance and comparison conditions.
