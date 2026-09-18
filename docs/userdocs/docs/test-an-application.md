---
title: Test an application
---

Use ordinary .NET unit tests for application logic. `Isoplane.Testing` adds
integration tests that start your application in the real sandbox and make HTTP
requests to it. The tests themselves run outside the sandbox.

From the directory containing `MyApi`, create a sibling test project and add
the testing package at the version of your installed release, which
`isoplane --version` prints before the `+`:

```bash
dotnet new xunit --name MyApi.Tests
cd MyApi.Tests
dotnet add package Isoplane.Testing --version "$(isoplane --version | sed 's/isoplane //; s/+.*//')"
```

Replace the generated test with the example below. It expects the original
template response; if you followed the first-application tutorial, change the
expected body to `Hello from my API!`. For another application, assert the
response its route should return.

```csharp
using Isoplane.Testing;
using Xunit;

public class ApplicationTests
{
    [Theory]
    [InlineData("IsoplaneInterpretedDebug")]
    [InlineData("IsoplaneNativeAotRelease")]
    public async Task The_home_page_answers(string configuration)
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        var project = Path.GetFullPath("../../../../MyApi", AppContext.BaseDirectory);
        await using var app = await IsoplaneApplication.StartAsync(
            project,
            new ApplicationOptions { Configuration = configuration },
            startup.Token);

        using var response = await app.Client.GetAsync("/");
        Assert.True(response.IsSuccessStatusCode, app.DiagnosticOutput);
        var body = await response.Content.ReadAsStringAsync();
        Assert.True(body == "Hello from Isoplane!", $"The home page returned unexpected content: {body}");
    }
}
```

Run it with:

```bash
dotnet test
```

Adjust the expected text if you changed the route. The fixture builds the chosen
configuration, starts the application on an available local port, and stops it
when disposed. Startup cancellation bounds a hung startup. The diagnostic output
helps distinguish build or runtime failures from an incorrect HTTP response.

Use a separate application directory for each concurrent session. The fixture
owns a session, not a shared global application. These tests exercise an
application as a client; they do not inject mocks into a running guest or run
arbitrary test methods inside the guest.
