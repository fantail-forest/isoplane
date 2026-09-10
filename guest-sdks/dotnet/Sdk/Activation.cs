using System;

namespace Wisp;

/// <summary>
/// The moment an instance first runs after activation from its template.
/// Every activation is a copy of the memory pre-initialisation left, so a
/// generator that seeded itself before the snapshot would replay the same
/// output in each copy; subscribe here to reseed or recreate it. The event
/// is raised once per activation, on the first dispatch or wake, and not
/// again after a wake from hibernation, whose state is the instance's own.
/// </summary>
public static class Activation
{
    private static bool _activated;

    /// <summary>Raised once, before the first dispatch of an activation runs.</summary>
    public static event Action? Activated;

    /// <summary>True from the first dispatch of this activation on.</summary>
    public static bool IsActivated => _activated;

    internal static void Enter()
    {
        if (_activated) return;
        _activated = true;
        Activated?.Invoke();
    }
}
