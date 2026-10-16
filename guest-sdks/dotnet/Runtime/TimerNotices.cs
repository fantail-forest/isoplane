using System;
using System.Collections.Generic;
using System.Globalization;
using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

/// <summary>
/// Tells a developer, in local development and tests, what each periodic timer
/// costs once the application runs timers between requests. Every timer is
/// recorded from start-up, so one created before the application opts in is
/// reported too. Nothing is reported before the instance's first dispatch: the
/// host says whether notices are shown only to a running instance.
/// </summary>
internal static class TimerNotices
{
    /// <summary>The average month: 8,760 hours a year over twelve months.</summary>
    private const long HoursPerMonth = 730;

    private const long MillisecondsPerHour = 3_600_000;

    /// <summary>Each periodic timer by callback and period, and whether it has been reported.</summary>
    private static readonly Dictionary<(string Callback, uint PeriodMs), bool> s_timers = new();

    private static bool s_activated;

    /// <summary>Starts recording periodic timers as the runtime schedules them.</summary>
    internal static void Install()
    {
        // Only a guest has the runtime's hook; tooling and tests run the SDK on a desktop runtime.
        if (!OperatingSystem.IsWasi()) return;
        PeriodicTimerScheduled(null) = Record;
    }

    /// <summary>The instance is serving its first dispatch, so the host can be asked whether to report.</summary>
    internal static void Activate()
    {
        s_activated = true;
        Report();
    }

    /// <summary>The application opted in to running timers between requests.</summary>
    internal static void OptedIn() => Report();

    private static void Record(Delegate callback, uint periodMs)
    {
        var key = (Name(callback), periodMs);
        if (s_timers.ContainsKey(key)) return;
        s_timers[key] = false;
        Report();
    }

    private static void Report()
    {
        if (!s_activated || Initialization.TimersBetweenRequestsMs is not { } minimumIntervalMs) return;
        if (!s_timers.ContainsValue(false) || Abi.H.DevelopmentNotices() == 0) return;
        var periods = new List<uint>();
        foreach (var (_, periodMs) in s_timers.Keys) periods.Add(periodMs);
        foreach (var (key, reported) in new List<KeyValuePair<(string, uint), bool>>(s_timers))
        {
            if (reported) continue;
            s_timers[key] = true;
            Abi.GuestLog.Emit(2, Notice(key.Item1, key.Item2, periods, minimumIntervalMs));
        }
    }

    /// <summary>
    /// What one timer costs on its own, and what all of the instance's timers cost
    /// together, in wakes an hour. A wake runs every timer then due, so the timers'
    /// own rates add up only while their ticks fall apart: the sum is a ceiling, and
    /// so is one wake per minimum interval.
    /// </summary>
    internal static (long Timer, long Instance) WakesPerHour(
        uint periodMs, IReadOnlyCollection<uint> periodsMs, long minimumIntervalMs)
    {
        long Rate(uint ms) => MillisecondsPerHour / Math.Max(ms, minimumIntervalMs);
        long sum = 0;
        foreach (uint ms in periodsMs) sum += Rate(ms);
        return (Rate(periodMs), Math.Min(sum, MillisecondsPerHour / minimumIntervalMs));
    }

    internal static string Notice(
        string callback, uint periodMs, IReadOnlyCollection<uint> periodsMs, long minimumIntervalMs)
    {
        var (timer, instance) = WakesPerHour(periodMs, periodsMs, minimumIntervalMs);
        var invariant = CultureInfo.InvariantCulture;
        return string.Format(invariant,
            "Timer {0} fires every {1}. Between requests it wakes this instance up to {2:N0} times an hour on its own; "
            + "with this instance's {3} periodic {4}, it is woken up to {5:N0} per hour ({6:N0} per month) while the app "
            + "is receiving at least one request per 5 minutes. This notice appears "
            + "only in local development and tests.",
            callback, Duration(periodMs), timer, periodsMs.Count, periodsMs.Count == 1 ? "timer" : "timers",
            instance, instance * HoursPerMonth);
    }

    internal static string Duration(uint ms) => ms switch
    {
        < 1_000 => string.Create(CultureInfo.InvariantCulture, $"{ms} ms"),
        < 60_000 => string.Create(CultureInfo.InvariantCulture, $"{ms / 1_000.0:0.###} s"),
        _ => string.Create(CultureInfo.InvariantCulture, $"{ms / 60_000.0:0.###} min"),
    };

    private static string Name(Delegate callback) =>
        callback.Method.DeclaringType is { } type ? type.FullName + "." + callback.Method.Name : callback.Method.Name;

    [UnsafeAccessor(UnsafeAccessorKind.StaticField, Name = "s_isoplanePeriodicTimerScheduled")]
    private static extern ref Action<Delegate, uint>? PeriodicTimerScheduled(Timer? timer);
}
