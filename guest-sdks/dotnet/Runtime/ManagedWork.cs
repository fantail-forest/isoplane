using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

internal static class ManagedWork
{
    internal static void Drain()
    {
        do
        {
            RunReady(null!);
            SynchronizationContext.SetSynchronizationContext(IsoplaneSynchronizationContext.Instance);
            IsoplaneSynchronizationContext.Instance.Drain();
        }
        while (ThreadPool.PendingWorkItemCount > 0 || IsoplaneSynchronizationContext.Instance.HasWork);
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RunWasiEventLoopOnce")]
    private static extern void RunReady(Thread thread);
}
