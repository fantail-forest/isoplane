# Negative Controls

## Make every guard fire in a test

The [guard coverage rule](../../../AGENTS.md#principles) proves a rejection path is reachable. Without a test that trips it, a check might be compiled out, gated on an unused flag, or checking the wrong condition.

**Not this:** A `debug_assert!` on an engine invariant, compiled out in release, whose doc comment says the check happens at construction, where no such check exists.

**Exception:** None.

## Give every limit a test that exceeds it

A memory ceiling, a pending-future cap, a queue depth, a module size limit: give each a test that goes one past it and observes the refusal, and one at the boundary that observes acceptance.

**Not this:** Pool caps "sized several times past the richest guest in the tree" with no test that ever hits them.

**Exception:** None.

## Don't make the negative control the positive one with the assertion flipped

A control proves the detector works by presenting the condition it detects. Building a stub that would pass and asserting it passes tests the stub.

**Not this:** A "negative control" that constructs a silent stub itself and asserts the adapter accepts it.

**Exception:** None.

## Demonstrate that the test detects the defect

Use a meaningful negative control when adding a regression test or guard: reproduce the defect before the fix, temporarily mutate the guarded behavior, or supply invalid input that reaches the real rejection path. Check that the relevant assertion detects the wrong behavior. Choose the control for the claim being made; invalid input proves rejection, while a claim that an assertion catches a missing guard needs that guard disabled or equivalent faulty behavior.

**Not this:** Calling a compile failure after deleting a new API proof that its behavioral assertions work, or treating an unrelated setup failure as the expected red result.

**Exception:** Reverting a fix is unnecessary when another control demonstrates the same failure. Report which control was exercised and any limits on what it proves.
