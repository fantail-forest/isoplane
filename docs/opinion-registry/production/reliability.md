# Reliability

## Refuse to start on invalid configuration, and say everything that is wrong

Parse and validate configuration in full before a port is bound or a store is opened, and don't start a node with invalid configuration. List every problem found in the report, so the operator fixes the file once. A node that starts and discovers a bad value on the first request has already accepted traffic it cannot serve.

**Not this:** A config key checked when the code path that reads it first runs.

**Exception:** None.

## End the operation on error, not the process, and never silently

On a failure while handling a request, a dispatch, a wake, or a deploy, end that operation with a structured event carrying its context and let the process continue. When a failure must end the process (a store that cannot open, a listener that cannot bind), emit the same kind of event first. Either way an operator can see what ended and why without a debugger.

**Not this:** A panic in host bookkeeping taking the cell down; an error swallowed into a `let _ =` so the request simply never answers.

**Exception:** None.
