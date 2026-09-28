# Test the real sandbox

`Isoplane.Testing` starts an installed `Isoplane.Cli` tool, publishes the application through `isoplane dev`, and exposes its actual HTTP listener. The native process owner supports Linux and Windows; no separate shell or process-cleanup utilities are needed. macOS support is unfinished. It requires the same .NET SDK and guest build toolchain as local development; it does not emulate the runtime.

Keep ordinary native .NET unit tests for application logic. Use this fixture for platform compatibility tests that need the actual WebAssembly sandbox and host behavior. The installed-package consumer is validated with xUnit; the package itself has no test-framework dependency. This does not establish compatibility for arbitrary application frameworks.

Reference `Isoplane.Testing` from an ordinary .NET 10 test project:

```csharp
using Isoplane.Testing;

using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
await using var app = await IsoplaneApplication.StartAsync(
    "/path/to/app",
    new ApplicationOptions { Configuration = "Debug" },
    startup.Token);
var response = await app.Client.GetAsync("/health");
response.EnsureSuccessStatusCode();
```

Select `Release` to exercise optimized application IL from a generated project. `CliPath` defaults to `isoplane` on PATH and can name an explicit installed executable.

The application directory must contain its `.csproj` and `isoplane.toml`. Port zero selects an available loopback port. Use a separate project directory for each concurrently running session; the CLI owns its existing project lock and local state. Startup cancellation, startup failure, and async disposal release owned child processes. Disposal first requests normal CLI cleanup, then terminates remaining descendants if `ShutdownTimeout` expires. Nonzero CLI exit and forced cleanup are reported as failures rather than successful disposal. `DiagnosticOutput` retains the last `DiagnosticLimitCharacters` of process output.

Pack into a local feed with `dotnet pack tools/Isoplane.Testing -o /tmp/isoplane-feed`. Install the matching CLI from the local feed using the [CLI instructions](../Isoplane.Cli/README.md). This package has no test-framework dependency.
