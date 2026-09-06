---
id: invariants
title: The four invariants
sidebar_position: 1
---

# The four invariants

These constraints guide WISP's host design. The implementation, enforcement
boundaries, and current limitations are explained in
[Workload security and isolation](workload-security.md).

## Invariant 1: all tenant state is visible

Represent resumable instance state as guest memory and supported mutable WASM
state, together with explicit records of resources the host holds for it.
Capture requires a safe return boundary and compatible state. Saving guest
memory alone does not preserve sockets or pending I/O.

## Invariant 2: all wake sources are host-owned

Keep external I/O and wake sources in the host. Guest code receives handles and
results through the capability interface. Host resources can remain live while
the guest is parked. This ownership rule does not prove that arbitrary guest
application logic will arrange a useful wake.

## Invariant 3: suspension implies an empty stack

A parked guest has returned control through its synchronous export. See
[the host checks](workload-security.md#why-a-parked-guest-has-no-live-invocation-stack)
and [the C# scheduling path](workload-security.md#how-the-c-sdk-cooperates).
A budget kill ends execution; it does not produce a resumable native stack.

## Invariant 4: capability-only imports

Route guest access to the outside world through the host interface and validate
the compiled module's imports. The
[enforcement boundary](workload-security.md#host-enforcement-does-not-depend-on-the-sdk)
is the sandbox and host authorization, even when a guest bypasses its SDK.
