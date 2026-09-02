# Metering

## Export meter output as events, not internal counters

Use per-dispatch completion events and exported aggregates to assess resource usage. An in-process counter alone does not provide durable accounting.

**Not this:** Treating a gauge that resets on restart as a durable usage record.

**Exception:** None.

## Enforce limits where they are metered

Enforce a CPU budget in the engine that meters it, a memory ceiling in the allocator that accounts it, and a pending-future cap in the table that holds them. A limit that is configured and never enforced is a lie in the config schema.

**Not this:** A concurrency cap documented as configured and never enforced.

**Exception:** None.

## One budget, from one table

Take the CPU budget a dispatch runs under from one place, and run tests under the same budget production does. A test-only default that differs from the production table by orders of magnitude means every test proves something production never runs.

**Not this:** A default budget constant used by every scenario while the plan table production reads carries a different value.

**Exception:** None.
