#nullable enable
using System;
using System.Reflection.Metadata;
using System.Text.Json;

[assembly: MetadataUpdateHandler(typeof(Debugger.HotReload.ReloadNotifications))]

namespace Debugger.HotReload;

/// <summary>Edits callers while their callee owns the current pause.</summary>
public static class Callers
{
    private static int calls;
    private static int constructions;

    /// <summary>Retains a partially evaluated expression and locals while adopting caller edits.</summary>
    public static string Handle()
    {
        int preserved = 7;
        int result = 100 + Outer();
        result += 10; // OUTER_EDIT
        return $"{result}:{preserved}:{calls}";
    }

    /// <summary>Adopts edits after class and value-type constructors finish.</summary>
    public static string Construct()
    {
        int preserved = 7;
        int result = 100 + new Constructed(20).Value;
        result += 10; // CLASS_CONSTRUCTION_EDIT
        result += new ConstructedValue(3).Value;
        result += 10; // VALUE_CONSTRUCTION_EDIT
        return $"{result}:{preserved}:{constructions}";
    }

    /// <summary>Observes a source edit without requiring a debugger pause.</summary>
    public static string Running()
    {
        ReloadNotifications.Enabled = true;
        var sample = new NotificationSample();
        if (JsonSerializer.Serialize(sample) != "{\"Value\":1}") throw new InvalidOperationException("The initial JSON contract was not cached.");
        System.Diagnostics.Debugger.Break();
        int value;
        do { value = Revision(); } while (value == 0);
        return $"{value}:{ReloadNotifications.Clears}:{ReloadNotifications.Updates}:{ReloadNotifications.TypesMatched}:{JsonSerializer.Serialize(sample)}"; // RUNNING_RESULT
    }

    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    private static int Revision() => 0; // RUNNING_EDIT

    private sealed class NotificationSample
    {
        public int Value => 1; // NOTIFICATION_PROPERTY
    }

    private sealed class Constructed
    {
        public int Value { get; }

        public Constructed(int value)
        {
            constructions++;
            Value = value;
            System.Diagnostics.Debugger.Break();
        }
    }

    private readonly struct ConstructedValue
    {
        public int Value { get; }

        public ConstructedValue(int value)
        {
            constructions++;
            Value = value;
            System.Diagnostics.Debugger.Break();
        }
    }

    private static int Outer()
    {
        int preserved = 3;
        Span<int> alias = new(ref preserved);
        Func<Number> operation = Inner;
        int result = Combine(new Box(20), operation());
        result += 10; // INNER_EDIT
        alias[0] += 2;
        return result + preserved;
    }

    private sealed record Box(int Value);
    private readonly record struct Number(int Value);

    private static int Combine(Box left, Number right) => left.Value + right.Value;

    private static Number Inner()
    {
        calls++;
        System.Diagnostics.Debugger.Break(); // CALLER_PAUSE
        GC.Collect();
        return new Number(1);
    }
}

internal static class ReloadNotifications
{
    internal static bool Enabled;
    internal static int Clears;
    internal static int Updates;
    internal static bool TypesMatched;

    public static void ClearCache(Type[]? updatedTypes)
    {
        if (!Enabled) return;
        Clears++;
        TypesMatched = updatedTypes is not null
            && Array.Exists(updatedTypes, type => type == typeof(Callers))
            && Array.Exists(updatedTypes, type => type.Name == "NotificationSample");
    }

    public static void UpdateApplication(Type[]? updatedTypes)
    {
        if (!Enabled) return;
        if (Clears != Updates + 1) throw new InvalidOperationException("Cache clearing must precede application updates.");
        Updates++;
        throw new InvalidOperationException("notification-fixture-warning");
    }
}
