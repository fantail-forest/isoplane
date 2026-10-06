using System.Text;
using System.Text.Json;
using Isoplane;

namespace SampleHandler;

/// <summary>
/// The application. Isoplane calls <see cref="HandleAsync"/> for every HTTP
/// request; the other attributed methods are optional lifecycle hooks.
/// </summary>
public static class Handler
{
    private static Greeter greeter = new("world");
    private static Guid instance;

    /// <summary>
    /// Runs while Isoplane prepares the template that every instance starts
    /// from. Whatever it leaves in memory is copied into each instance.
    /// </summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        // Settings come from the platform. In local development they are the
        // project's User Secrets and its launch profile's environment variables.
        var settings = RuntimeConfiguration.ReadValues();
        greeter = new Greeter(settings.GetValueOrDefault("Greeting:Default") ?? "world");

        // Run the code a request will run, so no instance pays for its first use.
        _ = JsonSerializer.Serialize(greeter.Greet(null), HandlerJson.Default.Greeting);
        return Task.CompletedTask;
    }

    /// <summary>
    /// Runs in each instance, before its first request. State that must differ
    /// between instances is set here, because initialization state is shared.
    /// </summary>
    [Activated]
    public static Task ActivatedAsync()
    {
        instance = Guid.NewGuid();
        return Task.CompletedTask;
    }

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => (request.Method, request.Path) switch
    {
        ("GET", "/") => Task.FromResult(Json(new ServiceInfo("SampleHandler", instance), HandlerJson.Default.ServiceInfo)),
        ("GET", "/hello") => Task.FromResult(Hello(request)),
        ("POST", "/echo") => Task.FromResult(Echo(request)),
        ("GET", "/countdown") => CountdownAsync(request),
        (_, "/" or "/hello" or "/echo" or "/countdown") => Task.FromResult(Response.Text("Method not allowed", 405)),
        _ => Task.FromResult(Response.Text("Not found", 404))
    };

    private static Response Hello(Request request)
    {
        var name = request.Query.FirstOrDefault(pair => pair.Key == "name").Value;
        Console.WriteLine($"Greeting {name ?? "the default"}");
        return Json(greeter.Greet(name), HandlerJson.Default.Greeting);
    }

    private static Response Echo(Request request)
    {
        Greeting? greeting;
        try { greeting = JsonSerializer.Deserialize(request.Body.Span, HandlerJson.Default.Greeting); }
        catch (JsonException) { greeting = null; }
        return greeting is null ? Response.Text("Send a JSON greeting", 400) : Json(greeting, HandlerJson.Default.Greeting);
    }

    /// <summary>Streams its response: each line is sent as it is written, not buffered until the end.</summary>
    private static async Task<Response> CountdownAsync(Request request)
    {
        var response = Response.Stream(200, [new("content-type", "text/plain; charset=utf-8")]);
        for (var remaining = 3; remaining > 0; remaining--)
        {
            // Aborted is cancelled when the client goes away, so the handler stops instead of writing to nobody.
            await response.WriteAsync(Encoding.UTF8.GetBytes($"{remaining}\n"), request.Aborted);
        }
        await response.WriteAsync("Liftoff!\n"u8.ToArray(), request.Aborted);
        return response.Complete();
    }

    private static Response Json<T>(T value, System.Text.Json.Serialization.Metadata.JsonTypeInfo<T> type) =>
        Response.Json(JsonSerializer.Serialize(value, type));
}
