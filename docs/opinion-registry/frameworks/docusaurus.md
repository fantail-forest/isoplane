# Docusaurus

Choose what belongs in a maintained site using the [documentation lifecycle opinion](../workflow/documentation-lifecycle.md). Temporary research and reports belong in repository Discussions. Difficulty documenting a workflow should feed product improvement, not just more explanation.

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

Follow the [reference-documentation guidance](../../../AGENTS.md#comments--documentation): generate API references, config references and capability tables from code as a prerequisite of the documentation build, or drift-check deliberately retained generated copies on the stable toolchain. Generation at build time removes the need to synchronize a stored copy; generator behavior can still need tests. Write guides and tutorials by hand and update them when the behavior they explain changes.

**Not this:** Keeping a generated copy solely to give a drift test something to compare, or making a test runner build the documentation prerequisites.

**Exception:** Repository readers or consumers that cannot run the generator may benefit from retained generated pages. Check those copies for drift rather than requiring every consumer to generate them.

## Snapshot customer docs per release line

Publish one snapshot of the customer docs per release line, where a line begins with a breaking release, and re-snapshot the line from the working docs at every release on it. Keep the working docs unpublished, as the docs of the next release. Mark an item added after a line's first release with a release marker that is resolved and pruned at snapshot time, and derive the reference pages' markers from a record the snapshot keeps, not from prose. `docs/userdocs/README.md` has the mechanics.

**Not this:** A snapshot per patch release; editing a published snapshot by hand; a "since" note written into every list and table as prose.

**Exception:** None.

## Describe supported behavior and its limits

Describe behavior the implementation provides, using evidence appropriate to the claim. Customer-visible guarantees need validation through the relevant interface; structural explanations and limitations can be established from the implementation. Reuse existing evidence and coverage rather than adding a test for each page or sentence. A declared configuration field or SDK method alone does not establish a working feature.

**Not this:** A configuration reference advertising entrypoint kinds that nothing can dispatch, or a test that checks incidental source text solely to justify a documentation sentence.

**Exception:** None.

## Write for the developer using the product

Explain concrete behavior, requirements and actions. Keep internal acceptance
assessments and statements about what a test result proves in issue comments or
review reports. When a limitation matters to the reader, name it and explain
what they can do about it.

**Not this:** “Passing this sample does not establish compatibility.” Say which
build configuration to test or which feature requires a change.

**Exception:** None.

## One home per topic

Explain a topic once and link to it from everywhere else. Give container hygiene, the sandbox convention, the four invariants, and the benchmark method one page each.

**Not this:** The same Docker rules in four pages and a script header.

**Exception:** A one-sentence summary with a link is not a duplicate.

## Run a tutorial before publishing it

Execute a tutorial's commands from a clean checkout on the toolchain the page names, and show the output that run produced. A tutorial that describes a step the tooling refuses to perform is fiction.

**Not this:** A tutorial claiming a build script fetches a toolchain the script explicitly declines to install.

**Exception:** None.
