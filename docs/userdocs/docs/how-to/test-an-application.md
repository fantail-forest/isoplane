---
title: Test an application
---

Use ordinary .NET unit tests for application logic. `Isoplane.Testing` adds
integration tests that start your application in the real sandbox and make HTTP
requests to it. The tests themselves run outside the sandbox.

The [`isoplane-webapi`](../reference/project-templates.md#isoplane-webapi),
[`isoplane-handler`](../reference/project-templates.md#isoplane-handler),
[`isoplane-webapp`](../reference/project-templates.md#isoplane-webapp) and
[`isoplane-mcpserver`](../reference/project-templates.md#isoplane-mcpserver)
templates include a test project. Add one to any other application with the
[`isoplane-tests`](../reference/project-templates.md#isoplane-tests) template. From
the directory containing `MyApi`:

```bash
dotnet new isoplane-tests --name MyApi.Tests
```

This creates the xUnit project `MyApi.Tests` beside the application. It
references `Isoplane.Testing` at the version of your installed release, and
the application's project, which it takes to be `../MyApi/MyApi.csproj`: the
sibling named like the test project without `.Tests`. For an application
somewhere else, give the path from the test project to its project file:

```bash
dotnet new isoplane-tests --name MyApi.Tests --application ../src/MyApi/MyApi.csproj
```

If you keep a solution, add the project to it with
`dotnet sln add MyApi.Tests`.

The project has two files:

- `SandboxFixture.cs` starts the application in the sandbox once and shares it
  between the tests. Its comments show where settings and a build configuration
  go.
- `ApplicationTests.cs` has one test, which requests `/` and expects a success
  status. Replace it with what your application should return.

```csharp
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class ApplicationTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task The_application_answers()
    {
        using var response = await sandbox.Client.GetAsync("/");
        Assert.True(response.IsSuccessStatusCode, sandbox.Diagnostics);
    }
}
```

From the directory where you created the test project, enter it and run:

```bash
cd MyApi.Tests
dotnet test
```

The fixture builds the application, starts it on an available local port, and
stops it when the tests have finished. A time limit on starting bounds a start
that hangs. The diagnostic output in the assertion's message helps distinguish
a build or runtime failure from an incorrect HTTP response. Ordinary unit tests
belong in the same project, and
`dotnet test --filter "Category!=Sandbox"` runs them without starting the
sandbox.

Without the fixture, a test starts a session of its own:

```csharp
using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
await using var app = await IsoplaneApplication.StartAsync(
    Path.GetFullPath("../../../../MyApi", AppContext.BaseDirectory),
    new ApplicationOptions { Configuration = "Release" },
    startup.Token);
using var response = await app.Client.GetAsync("/");
```

To give the application a setting for the test, such as the connection string of
a database the test started, pass it in the options:

```csharp
new ApplicationOptions
{
    Settings = new Dictionary<string, string> { ["ConnectionStrings:Todos"] = connectionString }
}
```

Settings override the project's launch profile and User Secrets for that session.

Use a separate application directory for each concurrent session. The fixture
owns a session, not a shared global application. These tests exercise an
application as a client; they do not inject mocks into a running guest or run
arbitrary test methods inside the guest.
