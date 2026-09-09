using System;

namespace Wisp.Runtime;

/// <summary>The static cryptographic randomness bridge for the adapted NativeAOT runtime.</summary>
public static class Randomness
{
    /// <summary>Fills a span with fresh host entropy in bounded chunks; empty spans require no draw.</summary>
    public static void Fill(Span<byte> data) => Abi.Randomness.Fill(data);

    /// <summary>Returns the requested number of fresh random bytes.</summary>
    /// <exception cref="ArgumentOutOfRangeException"><paramref name="count"/> is negative.</exception>
    public static byte[] GetBytes(int count)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(count);
        byte[] result = new byte[count];
        Fill(result);
        return result;
    }
}
