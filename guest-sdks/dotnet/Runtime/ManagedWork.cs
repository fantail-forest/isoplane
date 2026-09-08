using System.Runtime.CompilerServices;
using System.Threading;

namespace Wisp.Runtime;

internal static class ManagedWork
{
    internal static void Drain()
    {
        do
        {
            RunReady(null!);
            SynchronizationContext.SetSynchronizationContext(WispSynchronizationContext.Instance);
            WispSynchronizationContext.Instance.Drain();
        }
        while (ThreadPool.PendingWorkItemCount > 0 || WispSynchronizationContext.Instance.HasWork);
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "WispRunReady")]
    private static extern void RunReady(Thread thread);
}
