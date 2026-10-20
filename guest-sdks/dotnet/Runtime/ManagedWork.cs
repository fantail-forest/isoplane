using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

internal static class ManagedWork
{
    /// <summary>Runs a bounded batch from this execution scope's runtime and continuation queues.</summary>
    internal static void Drain(GuestDispatch? dispatch = null)
    {
        var context = dispatch?.Context ?? IsoplaneSynchronizationContext.Instance;
        RunReady(null!);
        SynchronizationContext.SetSynchronizationContext(context);
        context.Drain();
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RunWasiEventLoopOnce")]
    private static extern void RunReady(Thread thread);
}
