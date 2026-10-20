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
