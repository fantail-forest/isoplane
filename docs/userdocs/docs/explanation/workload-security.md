---
id: workload-security
title: Workload security and isolation
sidebar_position: 2
---

# Workload security and isolation

WISP runs customer-supplied WebAssembly modules as untrusted workloads. The
security boundary is the Wasmtime sandbox and the host's capability checks.
Using the C# SDK correctly makes an application work with WISP's scheduler;
it is not a prerequisite for containing a malicious guest.

This page describes the current execution model and its limits. The
[four invariants](invariants.md) describe the architectural constraints.

## What is trusted

Wasmtime, the compiler and runtime code in the host, the host bindings, and the
operating system are trusted infrastructure. Guest code and its input are not.
The .NET runtime compiled into a guest executes inside that guest's WASM
sandbox; it does not become a host CLR with unrestricted access to the machine.

A cell is a group of threads inside one `wispd` process. It is not a process,
container, or VM boundary. A guest trap and a caught host panic have contained
handling paths, but a process abort, host memory-safety defect, or engine escape
can affect other tenants in that process. The sandbox is not a guarantee
against microarchitectural side channels.

## Host enforcement does not depend on the SDK

The deploy validator checks the actual module's imports, export signatures,
memory shape, and permitted WASM features. The execution engine separately
configures its feature restrictions: threads, shared memory, atomics, and WASM
stack switching are unavailable. Cargo features describe compiled engine
support; the engine configuration determines what guest modules can use.

The guest reaches the outside world through the declared host interface. WISP's
PAL adapts an enumerated set of WASI calls; unsupported blocking calls, including
`pollable.block`, trap. A guest cannot gain arbitrary filesystem, network, or
thread access by bypassing the C# wrappers. The import surface says what can be
requested; host-side tenant context and authorization determine which resources
the request may access. A valid import is not permission to access another
tenant's data.

Memory bounds, resource limits, and execution budgets apply to guest modules
regardless of their source language. Bounds violations trap. A guest that keeps
executing instead of returning is subject to a budget kill. These controls do
not prove that an application is correct, that it will finish, or that every
host resource has a complete lifecycle bound.

## Why a parked guest has no live invocation stack

The host calls `wisp_handle` and `wisp_wake` synchronously. A guest parks by
**returning** `Pending` from that call. It does not suspend the call inside
Wasmtime's async execution API.

“Empty stack” means there are no live guest invocation frames left on the
worker's native stack. The worker still has its own host stack, and guest linear
memory still holds application state, including managed continuations. It does
not mean every byte of a guest's stack storage has been zeroed.

The engine tracks WASM invocation depth using Wasmtime call hooks. Parked state
requires a returned `Pending`, zero invocation depth, and no tombstone. Capture
eligibility also allows surviving completed instances; it requires zero depth
and is checked again after the pre-capture GC hint. A guest's claim that it is
waiting cannot authorize capture while its invocation is still executing.

A module can spin, return misleading results, or return `Pending` without
arranging useful work. It cannot turn that into permission for the host to
snapshot a live native stack.

## How the C# SDK cooperates

C# compiles `async` methods into state machines. At an incomplete `await`, a
method saves its continuation state and returns to its caller; it does not keep
that invocation's native frames suspended. In the WASM guest, the managed task
and state-machine objects live in linear memory.

The SDK connects this language mechanism to the host protocol:

1. An asynchronous SDK timer call invokes a host operation and
   receives a future ID without waiting for the operation to finish.
2. The guest future table associates that ID with a `TaskCompletionSource`.
   The SDK installs `WispSynchronizationContext`, whose `Post` queues
   continuations in guest memory.
3. The export driver starts the root handler task and drains runnable
   continuations. If the root task is incomplete, `Settle()` returns `Pending`,
   returning all the way through the synchronous WASM export to the host.
4. When the host has a result, it makes a new `wisp_wake` invocation. The SDK
   completes the registered task and drains runnable continuations again.

A worker can execute other guests between these invocations. Resumption uses
fresh invocation frames and the saved state machine, not copied native stack
frames. An already-completed await need not park at all; the boundary is the
export's return, not each occurrence of the `await` keyword.

The synchronization context is an SDK scheduling mechanism, not an unbypassable
C# restriction. Customer code can use custom awaiters, bypass context capture,
replace the context, or synchronously wait. The current SDK does not statically
validate every use of `Task`, `ConfigureAwait`, or third-party libraries.
Unsupported usage can fail or prevent progress, while the host still controls
whether the invocation has returned and is eligible for capture.

## Preemption is not a resumable checkpoint

An OS descheduling a worker preserves that thread's live execution context; it
does not make the guest parked or its state portable. WISP's epoch-based budget
checks can interrupt guest execution. Cooperative return during the cancellation
grace period and a hard budget trap have different outcomes: a hard kill
terminates execution and tombstones the instance. It does not preserve a
continuation for later resumption, and guest `finally` blocks are not guaranteed
to run on that path.

Freezing a thread and copying its stack bytes is not WISP's hibernation strategy.
Those bytes can contain native code addresses and pointers into other runtime
objects; registers and execution context also exist outside the stack bytes.
Copying them onto another thread or host does not reconstruct a valid execution.
Wasmtime's ability to suspend an async invocation on a separate native stack is
not a portable stack-serialization contract.

## Guest state and host resources

The engine can capture supported guest state at an eligible return boundary and
restore it against a compatible template. This does not serialize the host's
operating-system resources. A socket or pending operation remains owned by the
host, and copied guest bytes cannot recreate it after that host disappears.

Capturing guest state does not persist application data or guarantee that
external effects run exactly once.

## Safe suspension does not guarantee progress

Currently, `Settle()` returns `Pending` whenever the root task is incomplete.
It does not prove that the root depends on a host operation that can complete.
For example, awaiting a `TaskCompletionSource` that nobody completes can leave
a guest parked with no useful wake source. Checking that *some* future exists
would not prove that it can advance the root task.

CPU budgets bound executing work, not time spent parked. The engine's wall-clock
backstop applies during execution, and `dispatch_blocking`'s caller timeout stops
waiting for a result without itself terminating the dispatch. Neither establishes
an end-to-end deadline across parked or hibernated intervals. Ending a dispatch
also does not currently cancel all reactor work it spawned. These are lifecycle
and resource-retention gaps; successful SDK use and safe stack capture do not
close them.

## Implementation and evidence

The relevant source and regression suites in the repository are:

- `crates/wisp-control/src/validator/` and
  `crates/wisp-control/tests/validator_corpus.rs`: import, signature, memory,
  and feature-profile rejection, including threads and stack switching.
- `crates/wisp-engine/src/wtconfig.rs`, `activate.rs`, `call.rs`, and
  `instance.rs`; `tests/engine_park_capture.rs`, `engine_budget.rs`, and
  `engine_pal.rs`: synchronous entry, capture eligibility, traps, budget kills,
  and PAL behavior.
- `guest-sdks/dotnet/Abi/Exports.cs`, `Runtime/FutureTable.cs`, and
  `Runtime/WispSynchronizationContext.cs`: the C# scheduling path and the
  incomplete-root check described above. This inspection does not establish
  compatibility with arbitrary C# async patterns.
- `crates/wisp-host/tests/two_phase_kill.rs` exercises cooperative and
  uncooperative guests, including `finally` behavior.

For the underlying runtime mechanisms, see Microsoft's
[C# async state-machine explanation](https://devblogs.microsoft.com/dotnet/how-async-await-really-works/)
and [ConfigureAwait FAQ](https://devblogs.microsoft.com/dotnet/configureawait-faq/),
and Wasmtime's [async execution documentation](https://docs.wasmtime.dev/api/wasmtime/#asynchronous-wasm).
