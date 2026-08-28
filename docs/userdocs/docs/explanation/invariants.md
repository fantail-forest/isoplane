---
id: invariants
title: The four invariants
sidebar_position: 1
---

# The four invariants

These constraints guide the host design and its execution boundary.

## Invariant 1: all tenant state is visible

At a supported return boundary, resumable state consists of guest memory and
supported mutable WebAssembly state, together with explicit records of resources
the host holds on the guest's behalf. A live native invocation stack is not a
checkpoint. Saving guest memory alone does not preserve sockets or pending I/O.

## Invariant 2: all wake sources are host-owned

The host owns timers, sockets and futures. The guest receives handles and results
through the host interface. These resources can remain live while the guest is
parked; parking does not make their memory or operating-system cost disappear.
A wake is a new host call into the guest.

## Invariant 3: suspension implies an empty stack

The guest parks by returning `Pending` from its synchronous export. Its async
state remains in guest memory, while the invocation frames have returned. When a
result arrives, a fresh invocation resumes the saved task. A budget kill ends
execution; it does not save a native stack for later resumption.

For example, a Rust fixture's `Clock::delay(50)` creates a timer through
`clock_deadline_in`. If its future is incomplete, polling returns `Pending` and
the guest returns to the host. Delivery of the timer result starts a new wake
invocation. Returning `Pending` without arranging a useful wake does not
establish that the application will make progress.

## Invariant 4: capability-only imports

Guest access to the outside world passes through the host interface. Validation
checks the compiled module's imports and permitted features; the engine and host
bindings enforce those restrictions during execution. A supported import does
not by itself authorize access to every resource the host can reach.
