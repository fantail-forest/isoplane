---
title: Interpreted and NativeAOT execution
---

Both execution modes run your application inside the WebAssembly sandbox.
Interpreted execution runs the application's managed code through an interpreter
in the guest. NativeAOT compiles the application and its reachable dependencies
ahead of time into WebAssembly. It does not run the application as unrestricted
native code on your development machine.

## Why the project selects the mode

The template gives each mode a named configuration: `IsoplaneInterpretedDebug` and
`IsoplaneNativeAotRelease`. Its `PublishAot` setting selects the mode; the target is
`wasi-wasm` in both cases. Ordinary MSBuild configuration therefore determines
what `dotnet build`, `dotnet run` and the integration test fixture execute.

## Compatibility differs

Interpreted builds retain application metadata by default so libraries can
discover types and members at runtime. Retaining those assemblies uses more
memory. You can opt into trimming with `PublishTrimmed=true` when your
application and its dependencies support it; test reflection-dependent paths
with that setting enabled.

Ahead-of-time compilation needs to determine reachable code before the request
runs. Reflection and dynamic-code assumptions in an application or its libraries
can therefore expose trimming warnings or compilation failures that an
interpreted build does not. Build and test your application in the mode you
intend to deploy.

The PostgreSQL sample generates its EF model and queries at build time so they
can run under NativeAOT.

## Debugging

Breakpoint debugging is currently unavailable in both execution modes.

For the commands, use [build and run](run-and-rebuild.md). For validation in each
mode, use [sandbox integration tests](test-an-application.md).
