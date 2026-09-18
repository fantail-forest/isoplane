using System;

namespace Isoplane.Abi;

internal static unsafe class Randomness
{
    internal const int MaximumDrawBytes = 1048576;

    internal static void Fill(Span<byte> data)
    {
        while (!data.IsEmpty)
        {
            int length = Math.Min(data.Length, MaximumDrawBytes);
            fixed (byte* pointer = data)
                H.EntropyFill(pointer, length);
            data = data.Slice(length);
        }
    }
}
