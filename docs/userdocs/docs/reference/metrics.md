---
id: metrics
title: Metrics
sidebar_position: 14
---

# Metrics

Node and proxy measurements exported by the admin listener.

`GET /metrics` on the admin listener, in Prometheus text exposition format. Every series carries a `node="<name>"` label. There are **18** series: 8 counters, 5 gauges, 4 summaries and 1 histogram. The tables below, including the help text, are generated from what the exporter emits.

## Dispatches and CPU

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `wisp_dispatches_total` | counter | `tier` = `"fast"` \| `"optimized"` | Dispatches handled, by code tier |
| `wisp_cell_dispatches_total` | counter |  | Dispatches observed inside the cell |
| `wisp_cpu_ms_total` | counter |  | CPU milliseconds reported by the node |

## Lifecycle and memory

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `wisp_instances` | gauge | `tier` = `"hot"` \| `"parked"` \| `"hibernated"` | Instances by lifecycle state |
| `wisp_dirty_set_bytes` | histogram |  | Reported per-instance dirty-set bytes |
| `wisp_delta_storage_bytes` | gauge |  | Local snapshot bytes held |

## Latency

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `wisp_activation_us` | summary |  | Cold activation from a local template |
| `wisp_wake_us` | summary |  | Reported local wake latency |
| `wisp_proxy_request_latency_us` | summary |  | End-to-end latency observed at the proxy |
| `wisp_proxy_admission_wait_us` | summary |  | Time spent queued at admission before a slot freed |

## Faults

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `wisp_traps_total` | counter |  | Guest faults. Traps are never retried on the same memory. |
| `wisp_trap_rate` | gauge |  | Traps per dispatch |
| `wisp_hang_breakers_tripped` | gauge |  | Routes currently failing fast |

## Proxy

| Series | Type | Labels | Meaning |
|---|---|---|---|
| `wisp_proxy_requests_total` | counter |  | Requests accepted by the front proxy |
| `wisp_proxy_requests_unrouted_total` | counter |  | Requests with no matching route |
| `wisp_proxy_requests_shed_total` | counter |  | Requests refused at the edge before cells saturated |
| `wisp_proxy_requests_failed_total` | counter |  | Dispatches that returned an error |
| `wisp_proxy_route_generation` | gauge |  | Route-table generation |

Latency summaries publish the `0.5` and `0.99` quantiles and a sample count.
Use the `_count` series when interpreting those quantiles.
