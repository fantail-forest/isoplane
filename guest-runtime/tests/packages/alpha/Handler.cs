using System;
using System.Linq;
using System.Reflection;
using System.Threading;
using System.Threading.Tasks;
using Isoplane;

namespace Alpha;

/// <summary>Answers after one host timer, so each request parks once.</summary>
public static class Handler
{
    private static bool libraryLoadedDuringInitialization;
    private static Timer? heartbeat;

    /// <summary>Warms the response path before template capture.</summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        libraryLoadedDuringInitialization = AppDomain.CurrentDomain.GetAssemblies()
            .Any(assembly => assembly.GetName().Name == "Beta.Library");
        GC.KeepAlive(Answer("/warmup"));
        return Task.CompletedTask;
    }

    /// <summary>Answers <c>alpha:</c> and the request path.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        if (req.Path == "/load-library")
        {
            Assembly library = Assembly.Load("Beta.Library");
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return Response.Text($"initialized={libraryLoadedDuringInitialization};loaded={library.GetName().Name}");
        }
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        return Answer(req.Path);
    }

    private static Response Answer(string path) => Response.Text("alpha:" + path);

    /// <summary>A public static method of the wrong shape to be recorded as the handler.</summary>
    public static string Describe() => "not a handler";

    /// <summary>
    /// A method of the right shape to be recorded as the initializer, which returns with a
    /// periodic timer and an awaited delay still running.
    /// </summary>
    public static Task LeaveTimersRunningAsync()
    {
        heartbeat = new Timer(Beat, null, TimeSpan.FromSeconds(30), TimeSpan.FromSeconds(30));
        _ = PollAsync();
        return Task.CompletedTask;
    }

    private static void Beat(object? state) => GC.KeepAlive(heartbeat);

    private static async Task PollAsync() => await Task.Delay(TimeSpan.FromSeconds(45));
}
