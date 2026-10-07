using System.Text;
using System.Text.Json;
using Isoplane;

namespace Sample_Handler;

/// <summary>
/// The application. Isoplane calls <see cref="HandleAsync"/> for every HTTP
/// request; the other attributed methods are optional lifecycle hooks. Isoplane
/// finds the methods by their attributes, so the class and the methods can have
/// any names.
/// </summary>
public static class Handler
{
    // Static fields are the application's state. What the initializer assigns is
    // captured in the template and copied into every instance; what the activation
    // hook assigns belongs to one instance. Isoplane may discard an instance
    // between requests, so anything that must last belongs in a database.
    private static Greeter greeter = new("world");
    private static Guid instance;

    /// <summary>
    /// Runs while Isoplane prepares the template that every instance starts
    /// from. Whatever it leaves in memory is copied into each instance.
    /// </summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        // Settings are name/value pairs, named as in ASP.NET configuration
        // ("Section:Key"). In local development they come from three places, and
        // a later one overrides an earlier one:
        //
        //   1. User Secrets, the place for credentials. They are stored outside
        //      the project, in ~/.microsoft/usersecrets/<UserSecretsId>/secrets.json
        //      (%APPDATA%\Microsoft\UserSecrets\ on Windows). The project file
        //      holds the UserSecretsId. Assign and list them with:
        //
        //          dotnet user-secrets set "Greeting:Default" "Ada" --project src/Sample-Handler
        //          dotnet user-secrets list --project src/Sample-Handler
        //
        //   2. The "environmentVariables" of the launch profile in
        //      Properties/launchSettings.json, where two underscores stand for the
        //      colon: "Greeting__Default": "world".
        //
        //   3. Variables named ISOPLANE_APP_<setting> in the shell that starts the
        //      session, to override a setting for one run:
        //
        //          ISOPLANE_APP_Greeting__Default=Grace dotnet run --project src/Sample-Handler
        //
        // The application sees none of your machine's other environment variables.
        // Restart the session after changing a setting.
        var settings = RuntimeConfiguration.ReadValues();
        greeter = new Greeter(settings.GetValueOrDefault("Greeting:Default") ?? "world");

        // Do here what every instance would otherwise repeat: load reference data,
        // build lookup tables, and run the code a request will run, so that no
        // instance pays for its first use. Await any I/O and dispose its
        // connections before returning: an open connection cannot be captured.
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
        // Anything random or unique belongs here or in a request. An identifier
        // generated in the initializer would be the same in every instance.
        instance = Guid.NewGuid();
        return Task.CompletedTask;
    }

    /// <summary>
    /// Receives every HTTP request. To add a route, add a line to the switch.
    /// </summary>
    /// <remarks>
    /// The method may take other parameters:
    /// <list type="bullet">
    /// <item><c>HandleAsync(StreamingRequest request)</c> reads the body from <c>request.Body</c>, a
    /// stream, as it arrives. Use it for uploads too large to hold in memory.</item>
    /// <item><c>HandleAsync(Request request, Ctx ctx)</c> also receives the request's start time and its time budget.</item>
    /// </list>
    /// </remarks>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => (request.Method, request.Path) switch
    {
        ("GET", "/") => Task.FromResult(Json(new ServiceInfo("Sample-Handler", instance), HandlerJson.Default.ServiceInfo)),
        ("GET", "/hello") => Task.FromResult(Hello(request)),
        ("POST", "/echo") => Task.FromResult(Echo(request)),
        ("GET", "/countdown") => CountdownAsync(request),
        (_, "/" or "/hello" or "/echo" or "/countdown") => Task.FromResult(Response.Text("Method not allowed", 405)),
        _ => Task.FromResult(Response.Text("Not found", 404))
    };

    private static Response Hello(Request request)
    {
        // Query and Headers are lists of pairs, because a name may repeat. Compare
        // header names ignoring case.
        var name = request.Query.FirstOrDefault(pair => pair.Key == "name").Value;

        // Console output is the application's log: it appears in the terminal
        // running the session and, once deployed, in `isoplane logs`.
        Console.WriteLine($"Greeting {name ?? "the default"}");
        return Json(greeter.Greet(name), HandlerJson.Default.Greeting);
    }

    private static Response Echo(Request request)
    {
        // Body holds the whole request body as bytes.
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

    // Response.Text, Response.Json, Response.Bytes and Response.Http build a
    // complete response; Response.Stream, above, writes one as it is produced.
    private static Response Json<T>(T value, System.Text.Json.Serialization.Metadata.JsonTypeInfo<T> type) =>
        Response.Json(JsonSerializer.Serialize(value, type));
}
