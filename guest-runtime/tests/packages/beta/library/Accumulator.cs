using System.Numerics;

namespace Beta.Library;

/// <summary>A value-type pair, instantiated by the package over its own type arguments.</summary>
public readonly record struct Pair<T>(T First, T Second) where T : INumber<T>;

/// <summary>Sums pairs through generic math on value types.</summary>
public sealed class Accumulator<T> where T : INumber<T>
{
    /// <summary>The sum of every element of every pair.</summary>
    public T Sum(params Pair<T>[] pairs)
    {
        T total = T.Zero;
        foreach (Pair<T> pair in pairs) total += pair.First + pair.Second;
        return total;
    }
}
