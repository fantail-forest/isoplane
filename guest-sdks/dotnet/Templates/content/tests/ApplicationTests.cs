namespace Sample_App.Tests;

/// <summary>
/// Sandbox tests: HTTP requests to the application running in the real Isoplane
/// sandbox, which is where platform behavior such as initialization, activation
/// and outbound connections can be observed. The tests themselves run on your
/// machine, as a client of the application.
/// </summary>
/// <remarks>
/// The trait lets a quick run leave them out:
/// <c>dotnet test --filter "Category!=Sandbox"</c>. Ordinary unit tests of the
/// application's classes belong in this project too, in classes without the
/// collection and the trait: they need no sandbox.
/// </remarks>
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class ApplicationTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task The_application_answers()
    {
        // Replace this with what your application should return: the status, the
        // headers, and the body read with ReadFromJsonAsync or ReadAsStringAsync.
        using var response = await sandbox.Client.GetAsync("/");

        // The development tool's output as the message shows a build or startup
        // failure beside the assertion that noticed it.
        Assert.True(response.IsSuccessStatusCode, sandbox.Diagnostics);
    }
}
