# Negative Controls

## Make every guard fire in a test

Ship a check that rejects, refuses, limits, or kills with a test that trips it. A check that has never failed might not exist: it might be compiled out, gated on a flag nobody sets, or checking the wrong thing.

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

## Prove the guard by breaking the code

Before trusting a new check, revert the fix it guards and watch the test fail, then restore. A guard added alongside its fix and never seen red is a guess about what the test observes.

**Not this:** Adding an assertion and a fix in one commit and reporting both as verified.

**Exception:** None.
