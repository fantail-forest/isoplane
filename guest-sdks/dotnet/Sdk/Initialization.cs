using System;
using System.Threading.Tasks;

namespace Isoplane;

/// <summary>Initializes reusable application state before its deployment template is captured.</summary>
public static class Initialization
{
    private static readonly LifecycleHook Hook = new();

    /// <summary>Raised after application construction, before template capture.</summary>
    /// <remarks>
    /// Subscribe during application construction. Callbacks are awaited in registration order before
    /// template capture. The ASP.NET adapter then ensures the application host has started.
    /// Asynchronous network I/O uses the application's normal outbound policy.
    /// Await all I/O and dispose its connections, streams, and timers before the callback finishes;
    /// live host resources cannot be captured. Do not create request-scoped state or start
    /// background work. State retained here
    /// is copied into each instance; use <see cref="Activation.Activated"/> for per-instance
    /// initialization such as reseeding a random generator. Exceptions fail preparation.
    /// </remarks>
    public static event Func<Task>? Initializing;

    internal static event Func<Task>? Completing;

    internal static Task EnterAsync() => Hook.EnterAsync(Initializing, Completing);
}
