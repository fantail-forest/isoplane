namespace Wisp.Abi;

/// <summary>
/// The packed <c>u64</c> that every guest entrypoint returns
/// (spec.md §5.2; mirrors <c>wisp-abi::status::PollStatus</c>).
///
/// <code>
///   bits  0..32   tag      0 = done, 1 = pending, 2 = trapped
///   bits 32..64   payload  done -> response handle (0 = none)
///                          trapped -> trap code
/// </code>
///
/// <see cref="Pending"/> is the whole of invariant 3. It is a <em>return
/// value</em>, not a yield: producing it unwinds the wasm stack all the way
/// back to the host, which is why a parked instance is at a serializable
/// point by construction rather than by convention.
/// </summary>
internal static class PollStatus
{
    private const uint TagDone = 0;
    private const uint TagPending = 1;
    private const uint TagTrapped = 2;

    internal static long Done(uint responseHandle) => Pack(TagDone, responseHandle);
    internal static long Pending() => Pack(TagPending, 0);
    internal static long Trapped(TrapCode code) => Pack(TagTrapped, (uint)code);

    private static long Pack(uint tag, uint payload) => (long)((ulong)tag | ((ulong)payload << 32));
}

/// <summary>Mirrors <c>wisp-abi::status::TrapCode</c>.</summary>
internal enum TrapCode : uint
{
    Unknown = 0,
    Panic = 1,
    OutOfMemory = 2,
    CapabilityViolation = 3,
    /// <summary>Malformed dispatch/result encoding.</summary>
    Protocol = 4,
    TooManyFutures = 5,
}
