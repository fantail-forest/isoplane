using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

internal static class ManagedWork
{
    /// <summary>
    /// Runs ready work until none remains: the runtime's queue, the
    /// continuations posted to the instance's context, which belong to no
    /// dispatch, and those posted to <paramref name="dispatch"/>'s.
    /// </summary>
    internal static void Drain(GuestDispatch? dispatch = null)
    {
        var instance = IsoplaneSynchronizationContext.Instance;
        var context = dispatch?.Context ?? instance;
        do
        {
            RunReady(null!);
            SynchronizationContext.SetSynchronizationContext(instance);
            instance.Drain();
            SynchronizationContext.SetSynchronizationContext(context);
            context.Drain();
        }
        while (ThreadPool.PendingWorkItemCount > 0 || instance.HasWork || context.HasWork);
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RunWasiEventLoopOnce")]
    private static extern void RunReady(Thread thread);
}
