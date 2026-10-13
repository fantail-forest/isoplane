---
title: Application packages and the shared runtime
---

An Isoplane application is a package of managed IL, dependencies, symbols and
assets. The SDK records its runtime identity in that package. Local execution
uses the shared Mono runtime included in the SDK; a hosted node uses its installed
copy of the matching runtime. Both execute the application inside the WebAssembly
sandbox.

You select an SDK version in your project. You do not compile a runtime or supply
a WebAssembly module, baseline or native compilation profile when deploying an
application. A node refuses a package when its runtime identity does not match an
installed runtime.

On Linux, preparation initializes the runtime into a shared baseline, then
initializes each application package into a distinct layer over that baseline.
Compatible applications share the baseline while retaining their own application
state. Wasmtime still compiles the platform runtime for execution on the host.

Debug and Release are the ordinary .NET build configurations. Both produce an
IL package for the same runtime, with no trimming or ahead-of-time compilation
of your code, so they differ only in how the C# compiler optimizes it.

Application packages retain managed metadata for reflection and runtime type
loading. Sandbox capability and resource limits still apply to managed code and
its dependencies.

The shared debug runtime supports breakpoints, stepping, watches and editing
locals. See the [debugging workflow](debug-an-application.md).

Use [build and run](run-and-rebuild.md) for local development and
[sandbox integration tests](test-an-application.md) to exercise application
behavior through the runtime.
