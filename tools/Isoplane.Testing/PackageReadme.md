# Isoplane.Testing

Start an Isoplane application in the real sandbox from a test, and send it HTTP
requests. The test itself runs outside the sandbox, with any test framework.

```csharp
using Isoplane.Testing;

using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
await using var app = await IsoplaneApplication.StartAsync(
    "/path/to/application/project",
    new ApplicationOptions { Configuration = "Debug" },
    startup.Token);
var response = await app.Client.GetAsync("/health");
response.EnsureSuccessStatusCode();
```

- The directory holds the application's one `.csproj`. The session builds it,
  starts it on a free local port, and stops it when disposed.
- `ApplicationOptions.Settings` supplies configuration for the session, such as
  the connection string of a database the test started. It overrides the
  project's launch profile and User Secrets.
- `DiagnosticOutput` holds the session's recent output, for the message of a
  failed assertion.
- Use a separate project directory for each session that runs at the same time.

It needs the [Isoplane.Cli](https://www.nuget.org/packages/Isoplane.Cli) tool at
the same version, found as `isoplane` on `PATH` or named by `CliPath`. Keep
ordinary unit tests for application logic; use this for behavior that depends on
the sandbox.

Learn more at [isoplane.dev](https://isoplane.dev/).
