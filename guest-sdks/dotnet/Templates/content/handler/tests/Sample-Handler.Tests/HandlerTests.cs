using System.Net;
using System.Net.Http.Json;

namespace Sample_Handler.Tests;

/// <summary>
/// Sandbox tests: HTTP requests to the application running in the real Isoplane
/// sandbox, which is where the handler and its lifecycle hooks actually run.
/// </summary>
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class HandlerTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task A_named_caller_is_greeted()
    {
        var greeting = await sandbox.Client.GetFromJsonAsync<Greeting>("/hello?name=Ada");

        Assert.Equal(new Greeting("Hello, Ada!", "Ada"), greeting);
    }

    [Fact]
    public async Task The_default_name_comes_from_the_setting_read_during_initialization()
    {
        var greeting = await sandbox.Client.GetFromJsonAsync<Greeting>("/hello");

        Assert.True(greeting == new Greeting("Hello, tester!", "tester"), sandbox.Diagnostics);
    }

    [Fact]
    public async Task A_posted_greeting_is_echoed()
    {
        using var response = await sandbox.Client.PostAsJsonAsync("/echo", new Greeting("Hi", "Grace"));

        Assert.Equal(new Greeting("Hi", "Grace"), await response.Content.ReadFromJsonAsync<Greeting>());
    }

    [Fact]
    public async Task A_body_that_is_not_a_greeting_is_refused()
    {
        using var response = await sandbox.Client.PostAsync("/echo", new StringContent("not json"));

        Assert.Equal(HttpStatusCode.BadRequest, response.StatusCode);
    }

    [Fact]
    public async Task The_countdown_streams_every_line()
    {
        Assert.Equal("3\n2\n1\nLiftoff!\n", await sandbox.Client.GetStringAsync("/countdown"));
    }

    [Fact]
    public async Task An_activated_instance_reports_its_own_identity()
    {
        var service = await sandbox.Client.GetFromJsonAsync<ServiceInfo>("/");

        // The identity is assigned on activation; the template's own value is empty.
        Assert.NotEqual(Guid.Empty, service!.Instance);
    }

    [Theory]
    [InlineData("GET", "/missing", HttpStatusCode.NotFound)]
    [InlineData("DELETE", "/hello", HttpStatusCode.MethodNotAllowed)]
    public async Task Unknown_routes_and_methods_are_refused(string method, string path, HttpStatusCode expected)
    {
        using var response = await sandbox.Client.SendAsync(new HttpRequestMessage(new HttpMethod(method), path));

        Assert.Equal(expected, response.StatusCode);
    }
}
