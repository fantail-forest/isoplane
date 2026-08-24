# WISP — proof of concept

A WASM host prototype where the isolation boundary is the WASM sandbox.
Guests return with an empty stack while waiting on host I/O. The host owns
the resources that can make a parked guest ready to run.

This is an experimental implementation. Tests exercise specific runtime
behaviors; they do not establish a general security or snapshot guarantee.

## Running it

These commands use the repository toolchain image through Docker and the
Sysbox runtime. The WebAssembly runtime remains the guest isolation boundary.

```bash
docker build -t wisp-dev:latest .        # rust + .NET 10 + wasm tooling
```

`scripts/dev <cmd>` runs any command in that sandbox:

```bash
./scripts/dev cargo test --workspace     # the full suite
./scripts/dev bash                       # a shell inside it
```

Build and measurement commands need their declared toolchain and inputs.
Record the machine and execution environment when comparing measurements.

## Layout

| Path | What it is |
|---|---|
| `crates/wisp-abi`  | Host/guest records, poll statuses and allowed capability definitions. |
| `crates/wisp-engine` | The sandbox boundary: wasmtime, templates, slots, CPU budgets, metering. |
| `crates/wisp-control`  | Module validation and application preparation. |
| `crates/wisp-host` | The cell: reactors, execution pool, future table, import vtable, lifecycle tiers. |
| `crates/wisp-hibernate`  | Snapshot identity primitives. |
| `crates/wisp-proxy` | HTTP termination and routing. |
| `guests/` | The guest SDK and demo guests, including deliberately hostile ones. |

## The four invariants

The runtime is organized around four constraints:

1. **Represent resumable state explicitly.** At supported return boundaries,
   sandbox state and records of host-owned resources describe what can resume.
   Local snapshots require defined preservation, rebinding or loss behavior for
   each resource type.
2. **Keep wake sources in the host.** The host owns sockets, timers and futures,
   including their lifetimes and costs while a guest is parked.
3. **Suspend on an empty stack.** A guest parks by returning control to the host.
   A live native execution stack is not a checkpoint.
4. **Route capabilities through the host interface.** Validation and linking
   enforce the allowed imports. Other language representations require
   generation or drift checks against the contract.

## Execution boundaries

A guest parks by returning control to the host. The host retains explicit
ownership of pending I/O and its wake sources.
