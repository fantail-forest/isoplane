using System;
using Isoplane.Abi;

namespace Isoplane.Runtime;

/// <summary>
/// Aims the garbage collector below the instance's memory limit. An instance
/// whose memory passes the limit takes no more requests and is replaced, so the
/// collector should work harder before the heap grows it there. Left to
/// itself, the collector collects after a set amount of allocation and grows
/// the heap whenever live data needs room, regardless of the limit.
/// </summary>
internal static class CollectorTarget
{
    /// <summary>
    /// Sets the heap size the collector aims to stay under: the memory limit,
    /// less what the instance holds outside the heap once the application has
    /// initialized, less a tenth of the limit for the heap's growth between
    /// collections. Called at the end of initialization, so the value is part
    /// of the captured template.
    /// </summary>
    internal static void Aim()
    {
        // Only a guest's runtime has the collector setting.
        if (!OperatingSystem.IsWasi()) return;
        GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
        GCMemoryInfo memory = GC.GetGCMemoryInfo();
        long limit = memory.TotalAvailableMemoryBytes;
        // The instance's linear memory: the limit less what remains available.
        long outsideHeap = Math.Max(memory.MemoryLoadBytes - memory.HeapSizeBytes, 0);
        long target = limit - outsideHeap - limit / 10;
        if (target > 0) RuntimeCollector.SetSoftHeapLimit((ulong)target);
    }
}
