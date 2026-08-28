---
id: wisp-toml
title: wisp.toml
sidebar_position: 3
---

# `wisp.toml`

The application manifest records handler lifecycle, numeric resource limits,
initialization variables and outbound HTTP bindings. The runtime contract is
`wisp_abi::config::AppManifest`; local project settings are read through
`wisp_cli::app::CliManifest`.

## Local project keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | none | Application name |
| `tenant` | string | none | Owning tenant |
| `module` | path | none | Path to the built WebAssembly module |
| `hosts` | string[] | `[]` | Application hostnames |
| `path_prefix` | string | none | Routing path prefix |
| `limits` | table | none | Numeric resource budgets |

## `[handlers]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `lifecycle` | `reuse` \| `fresh` | `reuse` | Handler freshness. `fresh` resets the instance after every dispatch. |

`reuse` permits guest-state reuse between dispatches. The host may reset or
discard an instance, so its memory is temporary application state.

`fresh` requests initialized guest state for each dispatch.

## `[bindings]`

| Key | Type | Meaning |
|---|---|---|
| `fetch_allowlist` | optional string[] | Optional outbound HTTP host allowlist |

## `init_visible_vars`

Variables supplied to application initialization as JSON. The guest can parse
these values during preparation and retain the result in initialized state.
Use values that may be copied into each activation of the prepared template.

```toml
init_visible_vars = [
  ["FX_URL", "https://rates.internal/nzd"],
  ["REGION", "nz-1"],
]
```
