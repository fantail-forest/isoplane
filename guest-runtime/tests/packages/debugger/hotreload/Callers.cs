using System;

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
        int result = Combine(new Box(20), Inner());
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
