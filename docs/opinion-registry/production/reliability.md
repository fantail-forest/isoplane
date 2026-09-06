# Reliability

## Refuse to start on invalid configuration, and say everything that is wrong

Follow the two startup phases in [Error Handling](../../../AGENTS.md#error-handling). Static checks can report malformed values and inconsistent limits together without opening a database; checking schema compatibility or dependency availability requires resource acquisition. Completing both phases before accepting traffic prevents the first customer request from becoming the startup check.

**Not this:** A config key checked when the code path that reads it first runs.

**Exception:** None.

## End the operation on error, not the process, and never silently

The [error-handling rules](../../../AGENTS.md#error-handling) place responsibility at the boundary that decides the outcome. A wide completion event can carry the failure context without duplicate error records at every propagating layer; startup failures need their own terminal event. The [observability guarantee](../../../AGENTS.md#observability) covers handled failures, not abrupt process loss.

**Not this:** A panic in host bookkeeping taking the cell down; an error swallowed into a `let _ =` so the request simply never answers.

**Exception:** None.
