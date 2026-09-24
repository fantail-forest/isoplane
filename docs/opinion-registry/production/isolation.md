# Isolation

## Treat every template as potentially secret-bearing

Applications choose whether to read configuration and secrets during initialization
or activation. User code and external services can supply sensitive data, so the
absence of platform secret reads does not establish that a template is public.
Protect templates, snapshots and derived artifacts as potentially secret-bearing.

Assume every guest-readable configuration value may have been captured during
initialization. Changing or deleting configuration or secrets creates a redeployment
even when code is unchanged: regenerate captured state before promoting the new
deployment. Reuse unaffected compilation outputs. A future layered template can
regenerate from the affected layer forward; layering is not a prerequisite.

Initialization-time access still goes through bounded host capabilities. This policy
does not permit bypassing the sandbox or capturing a live invocation stack.

**Not this:** Refusing initialization-time secrets to claim templates cannot contain
secrets, or changing settings beneath an instance that retains their previous values.

**Exception:** None.

## Type tenant identity and carry it from the edge

Establish the tenant by authentication at the proxy or the operator API and carry it as a typed value through routing, dispatch, storage, and metering. Never recover it by scanning a table, default it to a placeholder, or accept it from a request body.

**Not this:** A free-text tenant field on a deploy request defaulting to a placeholder string, and an instance identifier that omits the tenant so the daemon scans apps to find it.

**Exception:** None.

## Make a development-only credential refuse to run where it could be mistaken for production

Take secrets and keys through an interface with a production implementation. Keep a development-only implementation for local runs, and make it refuse to start anywhere it could be mistaken for production.

**Not this:** A constant key array in the production wiring.

**Exception:** None.

## Verify before you map

Establish an artifact's controlled compiler origin and compatibility before the engine maps it, and have the code path that maps it state those preconditions in its `SAFETY` comment and enforce them in its caller. Reject truncated or tampered artifacts at admission. Keep published backing files
under operator control and unchanged for the lifetime of their mappings. Map
templates; do not copy them into private memory. Contain a later mapped-storage
fault to the guest that hits it: turn the fault signal into a trap for that
guest's dispatch, and have host code read mapped pages only through operations
that report the error. Serve every application's guests from the node's shared
process and thread pool. Filesystem-specific integrity features are not a
prerequisite for artifact storage.

**Not this:** An unsafe load whose caller cannot establish compiler origin or preserve its backing files. A process per application or version to contain storage faults: it gives up the density the shared runtime exists for.

**Exception:** None.

## Contain a panic to its dispatch

Have the execution loop contain panics from host code around a dispatch, record them, tombstone the instance, and continue. Keep unwinding on in the release profile for this reason.

**Not this:** Abort-on-panic plus a `panic!` on any host bookkeeping error.

**Exception:** None.

## No fake latency or fake tiers in production wiring

A "regional" store that is a local directory with an injected sleep, a stage report with invented millisecond figures, a constant chosen to look plausible: each teaches operators and tests that a measurement exists. Give production wiring the real thing, or have it report the absence.

**Not this:** A sleep in the hibernation spool's upload path so that two wake tiers are "distinguishable rather than nominal".

**Exception:** None.
