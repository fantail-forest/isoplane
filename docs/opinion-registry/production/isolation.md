# Isolation

## Treat every template as potentially secret-bearing

Applications may read configuration and secrets during initialization or activation. User code and external services can supply sensitive data, so the absence of platform secret reads does not establish that a template is public. Protect templates, snapshots and derived artifacts as potentially secret-bearing.

## Type identity at its boundary

Represent established identities with typed values and avoid placeholder defaults. Establish trust at the appropriate boundary for the deployment; the local host relies on its trusted operator.

## Make a development-only credential refuse to run where it could be mistaken for production

Keep development-only credentials from being used where they could be mistaken for production credentials.

## Verify before you map

Enforce unsafe loading and mapping preconditions at the interface that owns them. Preserve the controlled local compiler origin, compatibility and integrity checks, and operator control of immutable backing files while they are mapped. A claimed protection needs evidence at the boundary that provides it.

## Contain a panic to its dispatch

Contain failures at the execution boundary so one operation cannot take down unrelated work. See [error handling](../../../AGENTS.md#error-handling) and [observability](../../../AGENTS.md#observability).

## No fake latency or fake tiers in production wiring

Report real measurements and explicitly indicate when data is unavailable. An invented delay or plausible-looking constant teaches operators and tests that a measurement exists.
