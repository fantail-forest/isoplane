using System;

namespace Debugger.HotReload;

/// <summary>Edits callers while their callee owns the current pause.</summary>
public static class Callers
{
    private static int calls;

    /// <summary>Retains a partially evaluated expression and locals while adopting caller edits.</summary>
    public static string Handle()
    {
        int preserved = 7;
        int result = 100 + Outer();
        result += 10; // OUTER_EDIT
        return $"{result}:{preserved}:{calls}";
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
