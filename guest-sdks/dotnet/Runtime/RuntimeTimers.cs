using System;
using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

/// <summary>
/// The timers initialization left running. The snapshot keeps their deadlines
/// on the clock of the moment it was taken, so an instance activated later
/// would find every one of them due at once. They are moved by the time in
/// between, so each counts its interval from activation. The move has to come
/// before any timer runs in the instance: one that fired first would be
/// rescheduled from its stale deadline and then moved again.
/// </summary>
internal static class RuntimeTimers
{
    private static long s_initializedAtMs;

    internal static void MarkInitialized() => s_initializedAtMs = Environment.TickCount64;

    /// <summary>Moves the timers once per instance; later calls do nothing.</summary>
    internal static void CountFromActivation()
    {
        // Only a guest starts from a snapshot; tooling and tests run the SDK on
        // a desktop runtime, which has neither the snapshot nor the runtime's hook.
        if (!OperatingSystem.IsWasi() || s_initializedAtMs == 0) return;
        long elapsedMs = Environment.TickCount64 - s_initializedAtMs;
        s_initializedAtMs = 0;
        ShiftForActivation(null!, elapsedMs);
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "ShiftForIsoplaneActivation")]
    private static extern void ShiftForActivation(Timer timer, long elapsedMs);
}
