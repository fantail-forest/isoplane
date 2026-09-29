# Architectural Decomposition

## Share the execution implementation between production and local use

Build production execution and local development/testing on the same core: sandbox activation, dispatch and scheduling, resource ownership, cancellation, budgets, and supported capture/restore behavior. Keep the [four invariants](../../../AGENTS.md#the-four-invariants) intact in both environments; changing the surrounding services does not justify a second implementation of guest execution.

**Not this:** A native .NET simulator that becomes the only integration-test host while production scheduling, capability enforcement and suspension execute elsewhere.

**Exception:** Ordinary application unit tests can run natively, and doubles can isolate dependencies under the [testing strategy](../testing/strategy.md). Neither establishes coverage of the execution implementation it replaces.

## Keep the execution core small independently of what is public

Admit a responsibility to the core when it is necessary to execute a supplied application correctly in one local cell. Public SDKs, preparation tools and development conveniences may be distributed alongside it without becoming engine dependencies; retain distinct engine and cell responsibilities where their interfaces earn that separation.

**Not this:** A public edition of the daemon that still needs registry, deployment and fleet configuration because those components were safe to disclose.

**Exception:** Low-level platform support needed for real execution, such as memory mapping or CPU measurement, belongs with its execution consumer. Lightweight does not mean removing correctness mechanisms.

## Accept prepared applications instead of acquiring or building them

Keep project discovery, dependency acquisition, IL transformation, initialization and artifact retrieval outside the execution core; the caller supplies compatible prepared input. Keep compatibility and trust enforcement at the consuming boundary, including authenticated provenance before unsafe native-code loading, and let preparation and execution consume one versioned contract.

**Not this:** An engine that accepts a registry URL or an abstract registry client, downloads a template, and invokes the compiler on a cache miss.

**Exception:** Holding installed templates and their backing storage while instances reference them is execution bookkeeping. It does not require remote version discovery or a distribution policy.

## Keep service operation outside local execution

Put responsibilities that discover, place, distribute, replicate, coordinate, sell or operate workloads in callers, providers or supervisors. Use the following nonexhaustive inventory to review dependencies and public interfaces; it is neither an implementation backlog nor a requirement for a separate service per row.

| Responsibility outside the core | Examples |
| --- | --- |
| Fleet coordination | Membership, discovery, heartbeats, consensus, leader election and desired-state reconciliation |
| Placement and capacity | Region/node selection, affinity, reservations, autoscaling, rebalancing and evacuation |
| Global ingress | DNS/anycast, regional steering, load balancing, TLS certificate operation, edge authentication and DDoS controls |
| Application and deployment management | Accounts, versions, authorization, routes, promotion, canaries and rollback |
| Build operations and artifact supply | Job scheduling, build isolation infrastructure, dependency downloads, registries, signing, distribution, prefetch and retention |
| Distributed ownership | Instance directories, shard maps, lease issuance, ownership transfer and distributed routing |
| Storage operation | Database/object-store deployment, replication, quorums, backup, recovery, residency and deletion orchestration |
| Failure recovery | Failure detection, process supervision, replacement placement, regional failover and disaster recovery |
| Machine isolation and operation | Namespaces, cgroups, seccomp deployment, privileges, mounts, CPU placement and host patching |
| Egress infrastructure | Caching DNS services, NAT, routing rules, private tunnels, proxies and connection placement |
| Operational observability | Exporters, collectors, aggregation, retention, dashboards, alerting, SLO calculation and durable usage delivery |
| Identity and security operations | User/service authentication, IAM, vault/KMS, credential issuance, key rotation and audit retention |
| Management surfaces | Operator APIs, consoles, support tooling, remote diagnostics and configuration distribution |

**Not this:** Making every local test start a node supervisor, telemetry exporter and distributed directory, or implement no-op substitutes for them.

**Exception:** Their local execution consequences remain real: enforce supplied limits and ownership, bound queues, serialize dispatch, mediate capabilities, retain host resources, and emit accurate outcomes.

## Introduce only the infrastructure interfaces execution consumes

Supply infrastructure through [deep, consumed interfaces](interfaces.md#make-a-trait-a-boundary-between-concerns-not-a-mirror-of-one-implementation) that hide deployment machinery. A connection provider exists because the guest opens a connection; external operations can consume outcomes or supply existing inputs without acquiring corresponding engine traits.

**Not this:** A universal plugin system with unused registry, fleet and telemetry-storage interfaces, or a socket API that exposes network namespace names to the engine.

**Exception:** None. Exclusion from the core does not by itself create an extension point.

## Put simple local defaults in a separate host

Compose the shared core with session-scoped in-memory service implementations and straightforward local networking so an application can run without a production account, registry or infrastructure deployment. Keep service state outside guest memory, preserve the supported capability semantics, and exercise interchangeable implementations through shared conformance tests.

**Not this:** Asking a test author to assemble a vault, directory, spool and resolver, or calling retention of a live guest object a hibernation implementation.

**Exception:** Applications may need external services, including their own databases. The local host mediates those connections; it provides no guest persistence API.

## Share failure semantics without importing recovery orchestration

Keep capture/restore, resource preservation or loss, cancellation and structured terminal outcomes in the shared implementation, with the limits of each operation explicit. Let the service decide where to recover and whether retry is safe; a saved sandbox does not establish survival of its sockets across a machine failure, and a missing completion establishes neither success nor zero usage.

**Not this:** Claiming transparent cross-region failover because a template restores locally, or replaying an interrupted dispatch without considering already committed effects.

**Exception:** None. Stronger delivery or migration guarantees need their own supported contracts and validation.

## Prove the boundary before moving repositories

Decompose incrementally when an actual consumer needs independence, validating the same supported application through the local host and production integration before extracting their shared implementation. Keep production consuming that implementation after extraction, and make standalone build/test/package independence demonstrable; repository placement, licensing and release work follow the proven boundary.

**Not this:** Moving a large dependency graph into a public repository to discover what it contains, or waiting for every possible fleet feature before supporting local tests.

**Exception:** A small earlier extraction can be justified by the [interface criteria](interfaces.md#make-a-crate-only-where-there-is-a-real-interface). It still needs a real consumer and validation, not speculative contracts for future infrastructure.
