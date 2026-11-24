# Tools and automation

## Use what exists

Use the standard library, maintained libraries and the tools we already have. Task runs build steps. Cargo, MSBuild and the .NET SDK compile and decide what is out of date. Git holds source history, and EF migrations hold the schema's. Configure or extend them before replacing them. Write custom code, including schedulers, freshness checks and caches, only for what is specific to Isoplane.

Managed source projects share `Isoplane.slnx`; solution filters select each build phase and MSBuild follows their project references. Task prepares external inputs before that graph and packages its outputs afterward without recompiling. Fixtures copied to isolated directories to exercise installed packages or source changes have their own small solutions.

## Don't repeat finished work

Reuse a build output or a passing check while its inputs are unchanged. Redo it when they change, when it failed, or when there's reason to doubt it.

## Let the tools run things in parallel

Declare the real dependencies and let Task, the build tools and the test runners schedule the rest. Serialize only work that really shares something it modifies.

## Regenerate generated files instead of testing them

A generated file may be committed. Commit it where reviewing a change to it is useful, or where something reads it without running the generator. Keep it current with automation that makes a stale copy hard to commit or publish. The pre-commit hook runs every generator and adds what they changed to the commit. A workflow that publishes or releases generated material generates it immediately before publishing.

A drift test is a test in the suite that checks whether two things are in sync. It is a symptom of a missing single source or of missing automation, so supply whichever is missing. Do not write a test to decide whether generated output needs regenerating. Regenerate it.

The one exception is a copy that cannot be generated, which the [hand-mirrored tables rule](../../../AGENTS.md#absolute-prohibitions) covers.
