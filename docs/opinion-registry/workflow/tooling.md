# Tools and Custom Automation

## Prefer maintained tools and libraries for established responsibilities

Apply the [dependency guidance](../../../AGENTS.md#dependency-management) across product code, tests, builds, releases and repository automation. Evaluate the tools already adopted and their supported features before writing a substitute for an established responsibility, including dependency resolution, scheduling, caching, parsing, migrations, authentication and test execution. Count the custom implementation, tests, state formats and recovery procedures in the maintenance cost; avoiding a declared dependency does not make those costs disappear.

**Not this:** Writing a general-purpose subsystem in a script because the first version looks quick, or treating development tooling as exempt from the library choices we apply to production code.

**Exception:** A small product-specific operation or a concrete requirement that suitable maintained tools cannot meet can justify custom code. State the gap briefly and keep the implementation scoped to it; choosing a tool still requires weighing its integration and operational costs.

## Let each tool own its state and workflow

Configure or extend a tool through its supported interfaces before adding duplicate machinery. Use Task for repository build orchestration, leaving compilation and incremental builds to Cargo, MSBuild and the relevant native build systems; use Git commits and pinned revisions for modified dependency sources, preserving upstream history. The same principle applies to package-manager resolution and lockfiles, migration histories, test discovery and execution: keep each responsibility with its owner.

**Not this:** A Task wrapper that implements its own dependency graph and freshness database, a patch-replay service that reconstructs Git history and tracks generated heads in JSON, or a second migration ledger beside EF migrations.

**Exception:** Artifact digests, signatures and provenance required by a consumer remain valid domain data. They verify what the consumer receives; they do not replace source control or build freshness decisions.

## Keep custom automation specific to the product

Scripts can invoke tools, translate formats, assemble product artifacts and supply environment-specific arguments. Review what the wrapper owns: when it starts resolving dependencies, reconciling state, scheduling work or implementing recovery that the underlying tool already supports, move that responsibility into the tool's configuration or extension points. Keep custom logic for the product requirement the tool does not supply.

**Not this:** Putting a new orchestration engine behind a familiar command name, or replacing a short format conversion with a framework that costs more to maintain than the operation.

**Exception:** A documented tool limitation may need a narrow adapter. Keep the workaround local and remove it when the limitation no longer applies.

## Prepare test prerequisites in the build graph

Declare fixture, runtime and package preparation in Task and run the required tasks before the test runner. Fixture helpers resolve and load those artifacts; they do not schedule builds, decide freshness, coordinate build writers or maintain a second build cache. A convenient entry command can invoke preparation and testing, with the dependencies and reuse policy remaining in the build tools.

**Not this:** A test body launching Task or a compiler on a cache miss, or a shared fixture helper growing locks and build receipts to make that safe.

**Exception:** Tests whose subject is a build, installation, invalidation or recovery workflow may invoke that real workflow with isolated inputs. Ordinary behavior tests use prepared artifacts; missing prerequisites fail with the preparation command rather than being skipped.

## Reuse valid work

Preserve reusable build outputs and let the owning tools determine freshness from the relevant inputs, toolchain, target and configuration. Build a shared prerequisite once and let its consumers reuse the immutable result. Reuse passing validation when its relevant inputs and environment are unchanged; repeat it when changes, failures or unresolved concerns justify another run.

**Not this:** Cleaning caches as routine setup, choosing a fresh build directory for every test, rebuilding a runtime for each consumer, or repeating a completed check just to obtain a new copy of the same evidence.

**Exception:** Clean builds and isolated outputs are appropriate when testing that workflow, diagnosing stale state or separating incompatible configurations. Keep invalidation scoped to the affected work.

## Run independent work in parallel

Declare actual dependencies and let Task, native build tools and test runners schedule independent work concurrently. Keep mutable outputs isolated and share prepared artifacts read-only. Use the tools' normal concurrency defaults.

**Not this:** A sequential command chain for unrelated builds, serializing the entire suite around one shared prerequisite, speculative worker caps, or adding a custom scheduler beside the tools that already own execution.

**Exception:** Real dependencies or shared mutable state may require serialization. Add resource limits only to address observed contention or failures, using the existing tools and scoping the adjustment to the affected workflow.
