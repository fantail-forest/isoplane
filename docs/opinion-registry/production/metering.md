# Metering

## Account for occupied workers

Compute usage is monotonic elapsed time while an execution or reactor worker
services a guest and cannot service another. Include synchronous host work and
OS descheduling inside that interval. Exclude queueing and parked I/O while the
worker is available for other work; reactor accounting surrounds each active
poll, not the lifetime of the asynchronous operation.

Watchdog deadlines are best-effort thresholds. Scheduling delays may cause
overshoot; tests prove interruption, cleanup and accounting without asserting
realtime delivery. Use controlled clocks for exact boundary checks.

## Export meter output as events, not internal counters

Use per-dispatch completion events and exported aggregates to assess resource usage. An in-process counter alone does not provide durable accounting.

**Not this:** Treating a gauge that resets on restart as a durable usage record.

**Exception:** None.

## Reclaim cached instances before parked dispatches

Use available memory across the host or VM, respecting container memory limits,
rather than separate per-cell budgets. Retain instances while memory is available.
Under memory pressure, hibernate idle
completed instances before parked in-flight instances, using least-recently-used
order within each group and only capturing at an empty-stack boundary. Under
storage pressure, discard idle hibernated instances in least-recently-used order;
an unfinished dispatch's continuation is protected from storage eviction.

Reserve the worst-case hibernation storage footprint before creating an instance,
including encoding, compression expansion and encryption overhead. Refuse admission
when that reservation cannot be secured. Keep host contexts and pending work alive
across in-process hibernation; dispatch deadlines and cancellation still apply.

**Not this:** Discarding a parked dispatch to recover disk space, or admitting a new
instance because its expected compressed delta fits when its maximum does not.

**Exception:** Explicit dispatch termination releases its resources. Retention grants
no guarantee of warm state or completion of background work. Background execution
uses a separate bounded instance allowance and does not count as a request.

## Enforce limits where they are metered

Enforce a worker-occupancy budget in the engine that meters it, a memory ceiling in the allocator that accounts it, and a pending-future cap in the table that holds them. A limit that is configured and never enforced is a lie in the config schema.

**Not this:** A concurrency cap documented as configured and never enforced.

**Exception:** None.

## Share production defaults and choose test budgets deliberately

Define production worker-occupancy defaults once and test that production wiring applies them. Qualify budget enforcement with explicit boundary and rejection cases. Tests of unrelated behavior may supply a budget that lets that behavior complete; their result does not establish that the workload fits a production tier.

**Not this:** An implicit test-only default presented as production qualification, or an unrelated behavior test failing because it inherited a tight production budget.

**Exception:** A test specifically qualifying a production budget uses that budget and names the workload it qualifies.
