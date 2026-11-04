# Architectural Decomposition

## Share the execution implementation between production and local use

Build production execution and local development/testing on the same core: sandbox activation, dispatch and scheduling, resource ownership, cancellation, budgets, and supported capture/restore behavior. Keep the [four invariants](../../../AGENTS.md#the-four-invariants) intact in both environments; changing the surrounding services does not justify a second implementation of guest execution.

**Not this:** A native .NET simulator that becomes the only integration-test host while production scheduling, capability enforcement and suspension execute elsewhere.

**Exception:** Ordinary application unit tests can run natively, and doubles can isolate dependencies under the [testing opinions](../testing.md). Neither establishes coverage of the execution implementation it replaces.

## Keep the execution core small independently of what is public

Admit a responsibility to the core when it is necessary to execute a supplied application correctly in one local cell. Public SDKs, preparation tools and development conveniences may be distributed alongside it without becoming engine dependencies; retain distinct engine and cell responsibilities where their interfaces earn that separation.

**Not this:** A public edition of the daemon that still needs registry, deployment and fleet configuration because those components were safe to disclose.

**Exception:** Low-level platform support needed for real execution, such as memory mapping or CPU measurement, belongs with its execution consumer. Lightweight does not mean removing correctness mechanisms.

## Accept prepared applications instead of acquiring or building them

Keep project discovery, dependency acquisition and compilation outside the execution core. The caller supplies compatible prepared input; enforce the unsafe loading preconditions at the consuming boundary, including controlled local compiler origin and immutable backing.

## Keep service operation outside local execution

Keep execution independent of deployment and operational machinery it does not consume. Use [deep, consumed interfaces](interfaces.md#make-a-trait-a-boundary-between-concerns-not-a-mirror-of-one-implementation) for actual dependencies rather than speculative extension points.

## Put simple local defaults in a separate host

Compose the shared execution core with local implementations so applications can run without unnecessary infrastructure. Exercise interchangeable implementations through shared conformance tests.

## Share failure semantics without importing recovery orchestration

Keep capture/restore, resource preservation or loss, cancellation and structured terminal outcomes in the shared implementation, with the limits of each operation explicit. Let the service decide where to recover and whether retry is safe; a saved sandbox does not establish survival of its sockets across a machine failure, and a missing completion establishes neither success nor zero usage.

**Not this:** Claiming transparent cross-region failover because a template restores locally, or replaying an interrupted dispatch without considering already committed effects.

**Exception:** None. Stronger delivery or migration guarantees need their own supported contracts and validation.

## Prove the boundary before moving repositories

Decompose when an actual consumer needs independence. Validate the supported application through the resulting boundary and demonstrate independent build and test before relying on the split.
