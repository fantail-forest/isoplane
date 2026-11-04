# Relational Storage

## Use the PostgreSQL protocol for relational integrations

Connect through the normal PostgreSQL protocol with a maintained driver and certificate verification. Consume storage through a responsibility-focused interface so infrastructure choices do not spread through application logic.

## Use EF Core and EF migrations

Use EF Core for managed relational persistence and EF migrations for schema evolution. Let maintained providers manage their connection pools and scoped contexts; use asynchronous database operations and transactions for the invariants that need them. Give each schema one migration owner; native consumers must not independently evolve the same tables.

Provider-specific SQL inside an EF migration or query is appropriate when required. It does not justify a separate migration system. The project's pre-release authorization permits resetting undeployed schemas without inventing legacy-data migration support.

## Preserve storage failure distinctions

Treat missing records, corrupt data and unavailable storage as distinct outcomes. In-memory state is a cache or explicitly transient operation state. A failed read is not evidence that state is absent or disposable.

## Enforce concurrency in the database

Keep multi-record invariants atomic through transactions, constraints and appropriate concurrency checks. Test conflicting operations against PostgreSQL. EF does not remove the need to choose transaction boundaries.

## Test the database we use

Exercise queries, constraints, transactions and EF migrations against real PostgreSQL with isolated databases or schemas. Provision local containers when needed and remove owned resources afterward. Keep clocks controllable where expiry matters.

Application unit tests may replace a storage boundary to isolate logic; they do not qualify the persistence implementation. SQLite and in-memory EF providers do not establish PostgreSQL persistence or migration correctness.
