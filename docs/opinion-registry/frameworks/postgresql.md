# Relational Storage

## Use PostgreSQL for relational integrations

Use PostgreSQL for relational integrations. Connect through the normal PostgreSQL protocol with a maintained driver and certificate verification. Consume storage through a responsibility-focused interface so infrastructure choices do not spread through application logic.

**Not this:** Replacing database operations with vendor HTTP calls or introducing a competing schema owner.

## Use EF Core and EF migrations

Use EF Core for managed relational persistence and EF migrations for schema evolution. Let maintained providers manage their connection pools and scoped contexts; use asynchronous database operations and transactions for the invariants that need them. Do not impose a single mutex-protected connection or hand-written `(name, sql)` migration framework. Give each schema one migration owner; native consumers must not independently evolve the same tables.

**Not this:** Holding a global mutex around remote database access or maintaining a second competing migration history.

**Exception:** Provider-specific SQL inside an EF migration or query is appropriate when required. This does not justify a separate migration system. The project's pre-release authorization permits resetting undeployed schemas without inventing legacy-data migration support.

## Persist state whose loss would change correctness

Persist state whose loss would change correctness. Treat in-memory state as a cache or explicitly transient operation state. Missing records, corrupt data and unavailable storage are distinct outcomes.

**Not this:** Treating stale cached data as authoritative or using an uncoordinated second database.

**Exception:** Explicitly disposable local execution state and request-lifetime state need not become database records.

## Enforce concurrency in the database

Keep multi-record invariants atomic through transactions, constraints and appropriate concurrency checks. Test conflicting operations against PostgreSQL. EF does not remove the need to choose transaction boundaries.

**Not this:** Reading a value, performing an independent write later, and assuming another process cannot change it between those operations.

**Exception:** None.

## Test the database we use

Exercise queries, constraints, transactions and EF migrations against real PostgreSQL with isolated databases or schemas. Provision local containers when needed and remove owned resources afterward. Keep clocks controllable where expiry matters.

**Not this:** Using SQLite or an in-memory EF provider to claim PostgreSQL persistence or migration correctness.

**Exception:** Application unit tests may replace a storage boundary to isolate logic; they do not qualify the persistence implementation.
