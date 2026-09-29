# Metering

## Export meter output as events, not internal counters

Use per-dispatch completion events and exported aggregates to assess resource usage. An in-process counter alone does not provide durable accounting.

**Not this:** Treating a gauge that resets on restart as a durable usage record.

**Exception:** None.

## Reclaim cached instances before parked requests

Use available memory across the host or VM, respecting container memory limits,
rather than separate per-cell budgets. Retain instances while memory is available.
Under memory pressure, hibernate idle
completed instances before parked in-flight instances, using least-recently-used
order within each group and only capturing at an empty-stack boundary. Under
storage pressure, discard idle hibernated instances in least-recently-used order;
an unfinished request's continuation is protected from storage eviction.

Reserve the worst-case hibernation storage footprint before creating an instance,
including encoding, compression expansion and encryption overhead. Refuse admission
when that reservation cannot be secured. Keep host contexts and pending work alive
across in-process hibernation; request deadlines and cancellation still apply.

**Not this:** Discarding a parked request to recover disk space, or admitting a new
instance because its expected compressed delta fits when its maximum does not.

**Exception:** Explicit request termination releases its resources. Retention grants
neither post-response guest execution nor a guarantee of warm state to customers.

## Enforce limits where they are metered

Enforce a CPU budget in the engine that meters it, a memory ceiling in the allocator that accounts it, and a pending-future cap in the table that holds them. A limit that is configured and never enforced is a lie in the config schema.

**Not this:** A concurrency cap documented as configured and never enforced.

**Exception:** None.

## Share production defaults and choose test budgets deliberately

Define production CPU defaults once and test that production wiring applies them. Qualify budget enforcement with explicit boundary and rejection cases. Tests of unrelated behavior may supply a budget that lets that behavior complete; their result does not establish that the workload fits a production tier.

**Not this:** An implicit test-only default presented as production qualification, or an unrelated behavior test failing because it inherited a tight production budget.

**Exception:** A test specifically qualifying a production budget uses that budget and names the workload it qualifies.
