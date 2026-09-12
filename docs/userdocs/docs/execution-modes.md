---
title: Interpreted and NativeAOT execution
---

Both execution modes run your application inside the WebAssembly sandbox.
Interpreted execution runs the application's managed code through an interpreter
in the guest. NativeAOT compiles the application and its reachable dependencies
ahead of time into WebAssembly. It does not run the application as unrestricted
native code on your development machine.

## Why the project selects the mode

The template gives each mode a named configuration: `WispInterpretedDebug` and
`WispNativeAotRelease`. Its `PublishAot` setting selects the mode; the target is
`wasi-wasm` in both cases. Ordinary MSBuild configuration therefore determines
what `dotnet build`, `dotnet run` and the integration test fixture execute.

## Compatibility differs

Ahead-of-time compilation needs to determine reachable code before the request
runs. Reflection and dynamic-code assumptions in an application or its libraries
can therefore expose trimming warnings or compilation failures that an
interpreted build does not. An interpreted test result does not qualify that
application's NativeAOT build.

The PostgreSQL sample includes EF model and query generation for its particular
workload. That supplies code needed by its NativeAOT path; it does not establish
that every EF application works without adaptation.

## A debug configuration is not a debugger

The current release does not ship a supported debugger adapter. Selecting a
configuration whose name includes `Debug` does not add breakpoint support.
The execution mode and the availability of a debugger are separate concerns.

For the commands, use [build and run](run-and-rebuild.md). For validation in each
mode, use [sandbox integration tests](test-an-application.md).
