using System;

namespace Wisp.Abi;

/// <summary>Host entropy for the adapted runtime's pinned-buffer RNG boundary.</summary>
public static unsafe class InstanceRandomness
{
    /// <summary>Fills the runtime-owned pinned buffer through the host entropy capability.</summary>
    public static void GetBytes(byte* data, int length) => Randomness.Fill(new Span<byte>(data, length));
}
