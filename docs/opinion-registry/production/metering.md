# Metering

## Account for occupied workers

Make resource measurements match the decision they support. Distinguish time a worker is occupied from time a dispatch is queued or parked, and use controlled clocks to test accounting boundaries. Watchdog deadlines are best-effort thresholds; scheduling can delay interruption.

## Export meter output as events, not internal counters

Use structured completion events for per-operation analysis. In-process counters reset with the process; do not present them as durable usage records.

## Reserve capacity for in-flight work

Bound resource commitments before admitting work, and protect in-flight work when reclaiming cached state. Capacity estimates must account for the supported representation rather than assuming favorable compression.

## Enforce limits where they are metered

Enforce a limit at the boundary that accounts for the resource. A configured limit that is never enforced misleads operators.

## Share production defaults and choose test budgets deliberately

Define shared defaults once and test their production wiring. Tests of unrelated behavior may use a budget that lets that behavior complete; that does not establish that the workload fits production limits.
