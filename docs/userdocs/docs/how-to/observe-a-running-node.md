---
id: observe-a-running-node
title: Observe a running node
sidebar_position: 8
---

# Observe a running node

The operator listener exposes health checks and Prometheus metrics. Its default
address is `127.0.0.1:8788`, configured by `admin.listen`.

## Health

```bash
curl -s http://127.0.0.1:8788/healthz
```

## Metrics

```bash
curl -s http://127.0.0.1:8788/metrics
```

The response uses Prometheus text exposition format. Read the `# HELP` and
`# TYPE` lines for the series this node emits.

## Response headers the proxy sets

| Header | Meaning |
|---|---|
| `x-wisp-version` | The immutable version this request was routed to |
| `x-wisp-tier` | `fast` or `optimized` code tier |

`x-wisp-*` is the platform's namespace: guest-set headers in it are stripped,
because a guest that could set `x-wisp-version` could lie to your dashboard.
