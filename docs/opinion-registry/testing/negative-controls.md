# Negative Controls

## Exercise critical rejection paths

The [rejection coverage rule](../../../AGENTS.md#principles) requires real-boundary tests for sandbox rejections and engine limits. Test other guards according to the correctness, security or contract failure they prevent. Existing tests can exercise several checks; do not add a separate test for each conditional merely to satisfy a count.

**Not this:** A `debug_assert!` on an engine invariant, compiled out in release, whose doc comment says the check happens at construction, where no such check exists.

**Exception:** None.

## Exercise enforced resource limits at their boundaries

A memory ceiling, a pending-future cap, a queue depth, a module size limit: cover acceptance at the supported boundary and refusal beyond it. Related limits can use table-driven cases and shared immutable fixtures. A test of one layer's accounting does not replace coverage of enforcement at the real boundary.

**Not this:** Pool caps "sized several times past the richest guest in the tree" with no test that ever hits them.

**Exception:** None.

## Don't make the negative control the positive one with the assertion flipped

A control proves the detector works by presenting the condition it detects. Building a stub that would pass and asserting it passes tests the stub.

**Not this:** A "negative control" that constructs a silent stub itself and asserts the adapter accepts it.

**Exception:** None.

## Demonstrate that the test detects the defect

Use a meaningful negative control when adding regression coverage or qualifying a critical guard: reproduce the defect before the fix, temporarily mutate the guarded behavior, or supply invalid input that reaches the real rejection path. Existing tests can supply the control. Check that the relevant assertion detects the wrong behavior. Choose the control for the claim being made; invalid input proves rejection, while a claim that an assertion catches a missing guard needs that guard disabled or equivalent faulty behavior.

**Not this:** Calling a compile failure after deleting a new API proof that its behavioral assertions work, or treating an unrelated setup failure as the expected red result.

**Exception:** Reverting a fix is unnecessary when another control demonstrates the same failure. Report which control was exercised and any limits on what it proves.
