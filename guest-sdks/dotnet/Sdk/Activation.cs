using System;
using System.Threading.Tasks;

namespace Isoplane;

/// <summary>
/// The moment an instance first runs after activation from its template.
/// Every activation is a copy of the memory pre-initialisation left, so a
/// generator that seeded itself before the snapshot would replay the same
/// output in each copy; subscribe here to reseed or recreate it. The event
/// is raised once per activation, before the first dispatch, and not
/// again after a wake from hibernation, whose state is the instance's own.
/// </summary>
public static class Activation
{
    private static readonly LifecycleHook Hook = new();

    /// <summary>Awaited once, in registration order, before the first dispatch of an activation runs.</summary>
    public static event Func<Task>? Activated;

    /// <summary>True after every activation callback has completed successfully.</summary>
    public static bool IsActivated => Hook.IsCompleted;

    internal static Task EnterAsync() => Hook.EnterAsync(Activated);
}
