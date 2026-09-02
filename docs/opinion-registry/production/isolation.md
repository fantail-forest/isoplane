# Isolation

## Type tenant identity and carry it from the edge

Establish the tenant by authentication at the proxy or the operator API and carry it as a typed value through routing, dispatch, storage, and metering. Never recover it by scanning a table, default it to a placeholder, or accept it from a request body.

**Not this:** A free-text tenant field on a deploy request defaulting to a placeholder string, and an instance identifier that omits the tenant so the daemon scans apps to find it.

**Exception:** None.

## Make a development-only credential refuse to run where it could be mistaken for production

Take secrets and keys through an interface with a production implementation. Keep a development-only implementation for local runs, and make it refuse to start anywhere it could be mistaken for production.

**Not this:** A constant key array in the production wiring.

**Exception:** None.

## Verify before you map

Establish an artifact's controlled compiler origin and compatibility before the engine maps it, and have the code path that maps it state those preconditions in its `SAFETY` comment and enforce them in its caller. Keep mapped backing files unchanged while their templates use them.

**Not this:** An unsafe load whose caller cannot establish compiler origin or preserve its backing files.

**Exception:** None.

## Contain a panic to its dispatch

Have the execution loop contain panics from host code around a dispatch, record them, tombstone the instance, and continue. Keep unwinding on in the release profile for this reason.

**Not this:** Abort-on-panic plus a `panic!` on any host bookkeeping error.

**Exception:** None.

## No fake latency or fake tiers in production wiring

A "regional" store that is a local directory with an injected sleep, a stage report with invented millisecond figures, a constant chosen to look plausible: each teaches operators and tests that a measurement exists. Give production wiring the real thing, or have it report the absence.

**Not this:** A sleep in the hibernation spool's upload path so that two wake tiers are "distinguishable rather than nominal".

**Exception:** None.
