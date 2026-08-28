---
id: deploy-pipeline
title: The deploy pipeline
sidebar_position: 3
---

# The deploy pipeline

Preparation turns a validated WebAssembly module into an artifact the local
engine can activate. The preparation code validates the module, runs its
optional initialization export, and compiles native code for the host engine.

## Validation before execution

Validation checks the module's actual imports, export signatures, memory shape
and permitted WebAssembly features before pre-initialization executes guest code.
The host's engine and import bindings enforce the execution boundary as well.

## Templates and activation

Pre-initialization can capture work performed by the guest's initialization
export. Activation restores that supported memory and global state instead of
repeating the captured work. A template does not guarantee that every runtime
startup step or application initializer has already run.

## Initialization policy

The initialization bindings expose the init-unit handshake, configuration input
and a string-hash seed. Application entropy is supplied separately; it is not
reconstructed from the string-hash seed. Configuration and initialized memory
can contain secrets, so the operator must protect templates and derived files.

A guest that participates in the init-unit protocol identifies each unit before
running it. When a unit reaches a refused capability, the preparation loop can
record that unit for deferral and replay initialization with that unit skipped.
This relies on stable unit identifiers and guest cooperation. Replaying guest
memory does not undo external effects, and the protocol does not imply that
arbitrary .NET static constructors automatically participate.

The runtime-bringup policy allows additional runtime imports when the host records
no active initialization unit. That policy is separate from the application entropy source; selecting
it does not grant arbitrary I/O during initialization.

## Compiled artifacts

Compiled native code depends on the engine and target configuration. Loading a
serialized native artifact requires trusted provenance; module validation alone
does not make arbitrary serialized native bytes safe to load.
