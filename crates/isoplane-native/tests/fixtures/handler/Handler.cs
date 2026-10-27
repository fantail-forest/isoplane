using System.Diagnostics;
using System.Globalization;
using Isoplane;

public static class Handler
{
    /// <summary>Refuses to prepare without a debugger while the RequireDebugger setting is true.</summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        if (RuntimeConfiguration.ReadValues().TryGetValue("RequireDebugger", out var required)
            && required == "true" && !Debugger.IsAttached)
            throw new InvalidOperationException("ordinary runtime preparation rejected");
        return Task.CompletedTask;
    }

    [HttpHandler]
    public static async Task<Response> HandleAsync(StreamingRequest request) => request.Path switch
    {
        "/async" => await Delayed(),
        "/upload" => await Upload(request),
        "/fan-out" => await FanOut(request),
        "/debug" => Response.Text(await DebugFixture.Application.Handle(detached: false)),
        "/detach" => Response.Text(await DebugFixture.Application.Handle(detached: true)),
        "/library" => Response.Text(DebugFixture.Application.Library()),
        "/greeting" => Greeting(),
        _ => Response.Text("Hello from Isoplane!"),
    };

    private static async Task<Response> Delayed()
    {
        await Task.Delay(1);
        return Response.Text("async-ready");
    }

    /// <summary>
    /// Starts <c>count</c> outbound requests to the loopback <c>port</c> at once,
    /// more than the launch profile lets one dispatch have pending, and answers
    /// with every result in the order the requests were started.
    /// </summary>
    private static async Task<Response> FanOut(StreamingRequest request)
    {
        int Query(string name) => int.Parse(request.Query.First(pair => pair.Key == name).Value, CultureInfo.InvariantCulture);
        int count = Query("count");
        var target = new Uri($"http://127.0.0.1:{Query("port")}/");
        using var client = new HttpClient();
        var replies = await Task.WhenAll(Enumerable.Range(0, count)
            .Select(index => client.GetStringAsync(new Uri(target, index.ToString(CultureInfo.InvariantCulture)))));
        return Response.Text(string.Join(",", replies));
    }

    private static async Task<Response> Upload(StreamingRequest request)
    {
        long total = 0;
        var buffer = new byte[4096];
        int read;
        while ((read = await request.Body.ReadAsync(buffer, request.Aborted)) != 0) total += read;
        return Response.Text($"received:{total}");
    }

    private static Response Greeting()
    {
        Console.WriteLine("guest-log-marker");
        return Response.Text(FixtureLibrary.Greeting.Message);
    }
}

namespace DebugFixture
{
    /// <summary>The code the debugger tests pause in. Their driver finds lines by their text.</summary>
    internal static class Application
    {
        internal static async Task<string> Handle(bool detached)
        {
            int answer = 42;
            Task delay = Task.Delay(100);
            if (delay.IsCompleted) throw new InvalidOperationException("host timer must be incomplete before awaiting");
            await delay;
            bool resumed = true;
            return resumed && !(detached && Debugger.IsAttached) ? answer.ToString() : throw new InvalidOperationException("continuation did not run");
        }

        internal static string Library() => Array.ConvertAll(new[] { 41 }, FixtureLibrary.Calculator.Work)[0].ToString();
    }
}
