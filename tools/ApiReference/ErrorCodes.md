---
mdx:
  format: md
---

# Error codes

The Isoplane SDK reports problems it finds in an application under codes that
start with `ISOPLANE`. Most appear in the editor as you type, on the line that
causes them, and all of them appear when the application builds. The codes,
severities and titles on this page are generated from the SDK, so they match the
version you have installed.

Codes marked **error** are requirements: the application cannot run until they
are fixed. Codes marked **warning** point at code that fails only if it is
reached at run time. You can silence a warning or promote it to an error; see
[Control compatibility warnings](../run-and-rebuild.md#control-compatibility-warnings).
[Project settings](project-settings.md) defines the properties that
decide what the build inspects.

{codes}

An *entry method* below is a method marked `[HttpHandler]`, `[Initializer]` or
`[Activated]` in an application
[without ASP.NET](../handle-requests-without-aspnet.md). A *lifecycle method* is one
marked `[Initializer]` or `[Activated]`.

## Assemblies and compatibility

### ISOPLANE1000

The build inspects every assembly that goes
into the application. This one is not a valid .NET assembly, has an invalid
assembly name, or could not be opened.

The message names the file. If your own project produced it, clean and rebuild.
If it comes from a package or a file reference, that file is damaged or is not a
.NET assembly: restore the package again, or remove the reference.

### ISOPLANE1001

A method is declared with `DllImport`
or `LibraryImport`. Declaring it is allowed. Calling it fails at run time unless
the runtime includes that native function, because an application cannot load
native libraries of its own.

This warning is off by default, since a declaration alone does not show that the
call will happen. To turn it on, set
[`IsoplaneWarnOnNativeImports`](project-settings.md#isoplanewarnonnativeimports)
to `true` in the application's project file.

Check whether your application reaches the call. If it does, use a managed .NET
API for the same job, or a different package. If it never does, there is nothing
to fix. See
[the machine underneath](../dotnet-runtime-differences.md#the-machine-underneath).

### ISOPLANE1002

The code calls `Thread.Start`. An instance has one
thread and cannot start another, so the call throws
`PlatformNotSupportedException` when it runs.

Use asynchronous APIs in place of a thread: start the operations and
`await Task.WhenAll(...)`. `Task.Run` compiles and runs, but it does not add a
thread either. See [one thread](../dotnet-runtime-differences.md#one-thread).

If the call is in a NuGet package, look for an option that turns the threaded
behaviour off, or an asynchronous alternative in the same package.

### ISOPLANE1003

The code calls one of `Thread.Sleep`,
`Thread.Join`, `Monitor.Wait`, `Task.Wait`, `Task.WaitAll`, `Task.WaitAny` or
`SemaphoreSlim.Wait`. When it runs, the call throws
`PlatformNotSupportedException`, in most cases on every call and not only when
it would have to wait.

Await instead. The editor offers a quick fix for the first four rows:

| Instead of | Use |
| --- | --- |
| `Thread.Sleep(...)` | `await Task.Delay(...)` |
| `task.Wait()` | `await task` |
| `Task.WaitAll(...)` | `await Task.WhenAll(...)` |
| `semaphore.Wait()` | `await semaphore.WaitAsync()` |
| `Task.WaitAny(...)` | `await Task.WhenAny(...)` |

`task.Wait()` on a task that has already completed successfully does not throw.
If you know that is the case at a particular call, suppress the warning there.
[Waiting is asynchronous](../dotnet-runtime-differences.md#waiting-is-asynchronous)
lists what each call does.

### Where these warnings are reported

`ISOPLANE1002` and `ISOPLANE1003` are reported on the call itself in your own C#
code, and from the compiled assembly for projects you reference. NuGet packages
are not inspected unless you set
[`IsoplaneAnalyzeDependencies`](project-settings.md#isoplaneanalyzedependencies)
to `true` in the application's project file.

## The entry point

### ISOPLANE1005

After compiling, the build checks that
Isoplane will have something to call. This error means it found nothing it can
use. The message says which of these applies:

- **The project has no entry at all.** Mark one static method `[HttpHandler]`.
  See [Handle requests without ASP.NET](../handle-requests-without-aspnet.md).
- **The entry point is synchronous.** End `Program.cs` with
  `await app.RunAsync();` in place of `app.Run();`. In C# the editor reports this
  earlier, as [`ISOPLANE1020`](#isoplane1020).
- **An entry method breaks one of the rules below.** In C# the editor reports
  these earlier, as `ISOPLANE1010` to `ISOPLANE1018`. You see `ISOPLANE1005` in
  their place when the project is not C#, or when analyzers are switched off.

### ISOPLANE1020

An Isoplane application has to hand control back while it waits for requests,
so it cannot call `app.Run()`.

Replace it with `await app.RunAsync();`. The quick fix does this, and makes an
explicit `void Main` asynchronous when there is one. See
[Adapt `Program.cs`](../onboard-an-application.md#adapt-programcs).

### ISOPLANE1021

The task was discarded, as in
`_ = app.RunAsync();`, or blocked on, as in `app.RunAsync().Wait();`. Either way
the entry point returns or blocks while the application should be serving
requests.

Await it: `await app.RunAsync();`. The quick fix does this.

## Entry methods

These apply to an application [without ASP.NET](../handle-requests-without-aspnet.md).
Each error is reported on the method, and the message names it.

### ISOPLANE1010

Isoplane calls the method without creating an
instance of its class.

Make the method `static`. The quick fix does this. If it used instance fields,
make them static too, or create the object inside the method.

### ISOPLANE1011

Change the return type. A method that returns `Response` directly can return
`Task.FromResult(...)` or become `async`. The quick fix makes the change where
one edit is enough.

### ISOPLANE1012

It takes exactly one parameter: a `Request`, or a `StreamingRequest` to read the
body as it arrives.

Give it that one parameter. The quick fix adds a `Request` parameter to a method
that has none. Get anything else the method needs from the request or from static
state.

### ISOPLANE1013

This applies to `[Initializer]` and `[Activated]` methods.

Change the return type to `Task`. A method with nothing to await can return
`Task.CompletedTask`. The quick fix makes the change where one edit is enough.

### ISOPLANE1014

Isoplane has nothing to pass to an
`[Initializer]` or `[Activated]` method.

Remove the parameters. The quick fix does this when the method does not use
them.

### ISOPLANE1015

The method, or the type that contains it, has
type parameters, and Isoplane has no type arguments to call it with.

Move the method to a non-generic type and remove its own type parameters. It can
call generic code from there.

### ISOPLANE1016

Another method in the same type has the same
name, and Isoplane finds the entry method by name.

Rename the other methods.

### ISOPLANE1017

That is one `[HttpHandler]` method, and at most one `[Initializer]` method and
one `[Activated]` method.

Keep the attribute on one method and call the others from it. The quick fix
removes the attribute from the method it is reported on.

### ISOPLANE1018

The project has an entry
point, so Isoplane runs that and never calls the attributed method.

In an ASP.NET application, remove the attribute and subscribe to
`Isoplane.Initialization.Initializing` or `Isoplane.Activation.Activated` before
awaiting `RunAsync`; see
[startup](../application-lifecycle.md#startup) and
[activation](../application-lifecycle.md#activation).
The quick fix removes the attribute.

<!-- Generated by tools/ApiReference. -->
