---
id: lifecycle
title: Handler lifecycle
sidebar_position: 2
---

# Handler lifecycle

An activation starts from its prepared template. While waiting for host I/O,
a guest returns control with no live invocation stack. The host owns the
pending operations and their wake sources.

## Parked state

A parked instance retains guest memory without occupying an execution worker.
Host-owned resources can still consume memory, sockets and reactor work; those
costs do not disappear when the guest returns.

## Memory between dispatches

The `reuse` lifecycle may retain guest memory between dispatches. Treat that
memory as temporary: the host can reset or discard it.

The `fresh` lifecycle starts each dispatch from template state.

Discarded guest state does not resume. A later activation starts from its
template.
