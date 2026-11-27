# Tools and automation

## Use what exists

Use the standard library, maintained libraries and the tools we already have. Task runs build steps. Cargo, MSBuild and the .NET SDK compile and decide what is out of date. Git holds source history, and EF migrations hold the schema's. Configure or extend them before replacing them. Write custom code, including schedulers, freshness checks and caches, only for what is specific to Isoplane.

Managed source projects share `Isoplane.slnx`; solution filters select each build phase and MSBuild follows their project references. Task prepares external inputs before that graph and packages its outputs afterward without recompiling. Fixtures copied to isolated directories to exercise installed packages or source changes have their own small solutions.

## Don't repeat finished work

Reuse a build output or a passing check while its inputs are unchanged. Redo it when they change, when it failed, or when there's reason to doubt it.

## Let the tools run things in parallel

Declare the real dependencies and let Task, the build tools and the test runners schedule the rest. Serialize only work that really shares something it modifies.

## Regenerate generated files instead of testing them

A generated file may be committed. A directory of generated output holds only generated files, and its generator removes whatever it did not write. Commit it where reviewing a change to it is useful, or where something reads it without running the generator. Keep it current with automation that makes a stale copy hard to commit or publish. The pre-commit hook runs every generator and adds what they changed to the commit. The hook is Husky running lint-staged, configured in `lint-staged.config.mjs` at the repository root. The generators are the tasks in `taskfiles/generate.yml`, and `task generate` runs them all. Husky is the only thing that manages this repository's Git hooks. Running any Task target registers the hook, and fails while the tooling for it is missing. The hook is a convenience that makes the right thing the default, not a guarantee. It does not run for `git commit --no-verify`, a commit that only deletes files, a rebase, a cherry-pick, a merge made without conflicts or on the server, or in a worktree where no task has run yet. Nor does it cover a commit that names its paths. A workflow that publishes or releases generated material generates it immediately before publishing, and that is the backstop. Publishing a release stops when code generation has left the working tree different from its commit.

A drift test is a test in the suite that checks whether two things are in sync. It is a symptom of a missing single source or of missing automation, so supply whichever is missing. Do not write a test to decide whether generated output needs regenerating. Regenerate it.

The one exception is a copy that cannot be generated, which the [hand-mirrored tables rule](../../../AGENTS.md#absolute-prohibitions) covers.
