# Docusaurus

## Organize documentation around Diátaxis needs

Use [Diátaxis](https://diataxis.fr/) for both customer and internal documentation.
Tutorials guide a learner through one concrete outcome; how-to guides solve a
specific task for a reader who knows the goal; reference describes the available
interfaces; explanation develops understanding. Classify the content by the
reader's need, not just by its subject, and link between these forms rather than
mixing them into a single page. Keep the customer and operator audiences separate.

**Not this:** A flat sequence of “guides” mixing a first-run exercise, API tables,
architectural rationale and troubleshooting; adding four sidebar labels without
separating those different purposes.

**Exception:** Navigation pages can connect all four forms without duplicating
their content. Add documentation when it serves a real need, not to fill a category.

## Generate reference docs from doc comments in code

Generate API references, config references, and capability tables from the doc comments and definitions in code, so that updating them never needs a manual edit to a docs file, and keep a test on the stable toolchain that fails when a generated page drifts from the code. Write guides and tutorials by hand; expect them to need manual edits to stay in sync, and treat that as part of the change that made them stale.

**Not this:** A drift test that needs nightly rustc and the .NET SDK to run, so that it runs nowhere.

**Exception:** None.

## Describe only what is built and tested

Add a page for a feature when a test exercises that feature through the interface a customer would use. Document configuration keys, entrypoint kinds, and SDK methods when they have a producer and a test, not when they have a struct field.

**Not this:** A configuration reference documenting entrypoint kinds that nothing in the tree can dispatch.

**Exception:** None.

## One home per topic

Explain a topic once and link to it from everywhere else. Give container hygiene, the sandbox convention, the four invariants, and the benchmark method one page each.

**Not this:** The same Docker rules in four pages and a script header.

**Exception:** A one-sentence summary with a link is not a duplicate.

## Run a tutorial before publishing it

Execute a tutorial's commands from a clean checkout on the toolchain the page names, and show the output that run produced. A tutorial that describes a step the tooling refuses to perform is fiction.

**Not this:** A tutorial claiming a build script fetches a toolchain the script explicitly declines to install.

**Exception:** None.
