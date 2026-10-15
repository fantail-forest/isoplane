using System;
using System.Threading.Tasks;

namespace Isoplane;

/// <summary>
/// Lets an application prepare captured state ahead of requests, that every instance then starts with.
/// </summary>
/// <remarks>
/// <para>
/// Isoplane initializes the application ahead of requests and saves its memory as a template. It then
/// runs as many copies of the application as its traffic needs; each copy is an instance, and each
/// starts from the template. Captured startup work is reused rather than repeated in every
/// instance. Subscribe to <see cref="Initializing"/> to do that work: load reference data, build
/// lookup tables, and run the code a request will run so that no request pays for its first use.
/// </para>
/// <para>
/// An application written without ASP.NET has no start-up code to subscribe from, and marks one
/// method with <see cref="InitializerAttribute"/> instead. For state that must differ between
/// instances, use <see cref="Activation"/>.
/// <see href="https://isoplane.dev/docs/application-lifecycle">The application lifecycle</see>
/// explains templates and instances in full.
/// </para>
/// </remarks>
public static class Initialization
{
    private static readonly LifecycleHook Hook = new();

    /// <summary>Raised after the application has started and before its template is saved.</summary>
    /// <remarks>
    /// <para>
    /// Subscribe in <c>Program.cs</c> before <c>await app.RunAsync()</c>. The callbacks run once
    /// <c>RunAsync</c> has started the application, so its services and routes are available to them.
    /// They are awaited one at a time, in the order they subscribed.
    /// </para>
    /// <para>
    /// A callback may make network calls, for example to load data or apply database migrations.
    /// Await all I/O, and dispose connections, streams and timers before the callback returns: an open
    /// connection cannot be saved in the template. Do not start background work or keep
    /// request-scoped services. An exception stops the application from being prepared, so it does
    /// not start.
    /// </para>
    /// <para>
    /// Everything the callbacks leave in memory is identical in every instance. Create what must
    /// differ between instances, such as a random identifier, in an <see cref="Activation.Activated"/>
    /// callback.
    /// </para>
    /// </remarks>
    /// <example>
    /// An ASP.NET application serves itself one request, so that the route, its serializers and its
    /// queries are ready in every instance.
    /// <see href="https://isoplane.dev/docs/reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/IsoplaneHosting/WarmupAsync">WarmupAsync</see>
    /// is in the <c>Isoplane.AspNetCore</c> package.
    /// <code>
    /// var app = builder.Build();
    /// app.MapGet("/todos", (TodoStore store) => store.All());
    ///
    /// Initialization.Initializing += async () =>
    /// {
    ///     await app.WarmupAsync("/todos");
    /// };
    ///
    /// await app.RunAsync();
    /// </code>
    /// </example>
    public static event Func<Task>? Initializing;

    private const long MinimumTimerIntervalMs = 1000;

    /// <summary>
    /// Wakes the application's idle instances to run timers that have come due, at most once per
    /// <paramref name="minimumInterval"/>.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Without this call, a timer that comes due while an instance has no request to handle runs at
    /// the start of the instance's next request. With it, an idle instance is also woken to run its
    /// due timers, no sooner than <paramref name="minimumInterval"/> after its last request or wake
    /// ended; a timer due earlier waits until then. Timers that come due during a request run on time
    /// either way.
    /// </para>
    /// <para>
    /// Wakes are best effort: an instance that has had no request for five
    /// minutes is stopped instead of woken, and an instance Isoplane has set aside to save memory runs
    /// its timers only when its next request arrives. Use a timer between requests for upkeep such as flushing telemetry, not for work that
    /// has to happen on schedule.
    /// </para>
    /// <para>
    /// The setting applies to every instance, so call this during initialization: from an
    /// <see cref="Initializing"/> callback, or from the method marked with
    /// <see cref="InitializerAttribute"/>. A later call replaces the interval an earlier one set.
    /// </para>
    /// </remarks>
    /// <param name="minimumInterval">
    /// The least time between the end of one request or wake and the next wake: at least one second.
    /// </param>
    /// <exception cref="ArgumentOutOfRangeException"><paramref name="minimumInterval"/> is less than one second.</exception>
    /// <exception cref="InvalidOperationException">Initialization has finished.</exception>
    /// <example>
    /// <code>
    /// Initialization.RunTimersBetweenRequests(minimumInterval: TimeSpan.FromMinutes(1));
    /// </code>
    /// </example>
    public static void RunTimersBetweenRequests(TimeSpan minimumInterval)
    {
        ArgumentOutOfRangeException.ThrowIfLessThan(minimumInterval, TimeSpan.FromMilliseconds(MinimumTimerIntervalMs));
        // Only a guest has a host to ask; tooling and tests run the SDK on a desktop runtime.
        if (!OperatingSystem.IsWasi()) return;
        var minimumIntervalMs = (long)Math.Ceiling(minimumInterval.TotalMilliseconds);
        if (Isoplane.Abi.H.TimersBetweenRequests(minimumIntervalMs) != 0)
            throw new InvalidOperationException("Call RunTimersBetweenRequests during initialization: the setting is saved with the application's template.");
    }

    internal static event Func<Task>? Starting;

    /// <summary>Raised after every <see cref="Initializing"/> subscriber has finished, before the template is captured.</summary>
    internal static event Func<Task>? Completing;

    internal static Task EnterAsync() => Hook.EnterAsync(RunStagesAsync);

    // Each stage reads its subscribers when it begins: an entry point that
    // awaits before subscribing is still resumed by Starting first.
    private static async Task RunStagesAsync()
    {
        await LifecycleHook.InvokeCallbacksAsync(Starting);
        await LifecycleHook.InvokeCallbacksAsync(Initializing);
        await LifecycleHook.InvokeCallbacksAsync(Completing);
    }
}
