using System;
using System.Diagnostics;
using System.Runtime.CompilerServices;

namespace Isoplane.Abi;

/// <summary>
/// The packed <c>u64</c> that every guest entrypoint returns, as
/// <c>isoplane-abi::status::PollStatus</c> encodes it.
///
/// <code>
///   low bits    tag      a <see cref="PollTag"/>
///   high bits   payload  done -> response handle (0 = none)
///                        trapped -> a <see cref="TrapCode"/>
/// </code>
///
/// How a tag and its payload are packed is generated from the Rust definition.
///
/// <see cref="Pending"/> is a return value, not a yield: producing it unwinds
/// the wasm stack back to the host, which is what leaves a parked instance
/// with nothing on any stack.
/// </summary>
internal static partial class PollStatus
{
    internal static long Done(uint responseHandle) => Pack(PollTag.Done, responseHandle);
    internal static long Pending() => Pack(PollTag.Pending, 0);
    internal static long Cancelled() => Pack(PollTag.Cancelled, 0);
    internal static long Failed(Exception error)
    {
        NotifyUnhandled(error);
        return Pack(PollTag.Failed, 0);
    }
    internal static long Trapped(TrapCode code) => Pack(PollTag.Trapped, (uint)code);

    internal static long Unhandled(Exception error)
    {
        NotifyUnhandled(error);
        return Trapped(TrapCode.Panic);
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void NotifyUnhandled(Exception error)
    {
        // The debugger adapter recognizes this frame and reads error after the handler has unwound.
        if (Debugger.IsAttached) Debugger.Break();
        GC.KeepAlive(error);
    }
}
