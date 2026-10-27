---
title: Find and fix blocking calls
---

A synchronous wait that would block throws `PlatformNotSupportedException` in
the sandbox, because an instance has one thread and nothing else on it could
ever satisfy the wait.
[Use async, not blocking waits](../concepts/dotnet-runtime-differences.md#blocking-waits)
explains the rule. This guide finds the blocking calls in an existing
application and replaces them.

## Build with the analyzer

The build reports a synchronous wait in your own code as
[`ISOPLANE1003`](../reference/error-codes.md#isoplane1003). To have it inspect
the NuGet packages you reference as well, set
[`IsoplaneAnalyzeDependencies`](../reference/project-settings.md#isoplaneanalyzedependencies)
in the project file and build:

```xml
<PropertyGroup>
  <IsoplaneAnalyzeDependencies>true</IsoplaneAnalyzeDependencies>
</PropertyGroup>
```

```bash
dotnet build
```

Each warning names the call and where it is. In the editor the warning sits on
the call itself, and where the call has an async counterpart a quick fix
replaces it.

## Search for what the analyzer can't see

The analyzer doesn't flag `.Result` or `.GetAwaiter().GetResult()`, which
block in the same way. Search your code for them:

```bash
grep -rn --include=*.cs "\.Result\b\|GetAwaiter()\.GetResult()" .
```

Some blocking calls don't look like waits at all, such as reading
`Request.Form` before the form has been read, or disposing a data reader that
still has unread rows.
[Use async, not blocking waits](../concepts/dotnet-runtime-differences.md#blocking-waits)
lists the ones we know of, with their async forms.

## Replace each call

Use the async form of each call. The most common replacements:

| Blocking call | Use instead |
| --- | --- |
| `task.Wait()`, `task.Result`, `task.GetAwaiter().GetResult()` | `await task` |
| `Task.WaitAll(...)` | `await Task.WhenAll(...)` |
| `Thread.Sleep(...)` | `await Task.Delay(...)` |
| `semaphore.Wait()` | `await semaphore.WaitAsync()` |
| `SaveChanges()`, `ToList()` on an EF Core context | `SaveChangesAsync()`, `ToListAsync()` |

Making one method async usually means making its callers async too. Work
outward from each call until you reach the request handler, which is already
async.

## Check the libraries you use

A library can block inside a call that looks asynchronous, or offer no async
API at all. [Library compatibility](../concepts/library-compatibility.md)
lists the libraries known to do so, with what to use in their place.

## Run the application

Anything left throws when it is reached, not before. Run the application and
its tests as [Run and rebuild](run-and-rebuild.md) and
[Test an application](test-an-application.md) describe. A remaining blocking
wait fails its request, and the session log shows
`request failed: System.PlatformNotSupportedException` without a stack trace.
To find the call, run under the debugger and break on the exception, as
[Debug an application](debug-an-application.md) describes, or wrap the
suspect code in a `try` block that logs the exception's `StackTrace`.
