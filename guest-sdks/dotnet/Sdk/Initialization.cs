using System;

namespace Wisp;

/// <summary>Initializes reusable application state before its deployment template is captured.</summary>
public static class Initialization
{
    private static bool _initialized;

    /// <summary>Raised after application construction, before template capture.</summary>
    /// <remarks>
    /// Subscribe during application construction. Callbacks run synchronously under the
    /// preparation capability policy and must finish before returning. Do not create
    /// request-scoped state or start background work. State retained here
    /// is copied into each instance; use <see cref="Activation.Activated"/> for per-instance
    /// initialization such as reseeding a random generator. Exceptions fail preparation.
    /// </remarks>
    public static event Action? Initializing;

    internal static void Enter()
    {
        if (_initialized) return;
        Initializing?.Invoke();
        _initialized = true;
        Initializing = null;
    }
}
