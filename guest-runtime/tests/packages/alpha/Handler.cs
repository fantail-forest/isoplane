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
    private static int beats;
    private static readonly SemaphoreSlim gate = new(0);

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
        // One request waits on a gate that another opens, with no host future
        // between them.
        if (req.Path == "/gate/wait")
        {
            await gate.WaitAsync().ConfigureAwait(false);
            Console.WriteLine("resumed after the gate opened");
            return Response.Text("resumed");
        }
        if (req.Path == "/gate/open")
        {
            gate.Release();
            return Response.Text("opened");
        }
        if (req.Path == "/load-library")
        {
            Assembly library = Assembly.Load("Beta.Library");
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return Response.Text($"initialized={libraryLoadedDuringInitialization};loaded={library.GetName().Name}");
        }
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        if (req.Path == "/beats") return Response.Text($"beats={beats}");
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

    /// <summary>
    /// A method of the right shape to be recorded as the initializer, which opts in to running
    /// timers between requests and leaves a frequent periodic timer running.
    /// </summary>
    public static Task RunTimersBetweenRequestsAsync()
    {
        Initialization.RunTimersBetweenRequests(minimumInterval: TimeSpan.FromMilliseconds(100));
        heartbeat = new Timer(LoggedBeat, null, TimeSpan.FromMilliseconds(20), TimeSpan.FromMilliseconds(20));
        return Task.CompletedTask;
    }

    private static void Beat(object? state) => beats++;

    private static void LoggedBeat(object? state)
    {
        beats++;
        Console.WriteLine("heartbeat");
    }

    private static async Task PollAsync() => await Task.Delay(TimeSpan.FromSeconds(45));
}
