using System.Runtime.InteropServices;

namespace Isoplane.Abi;

/// <summary>The garbage collector of the runtime the guest links, as module <c>isoplane_runtime</c>.</summary>
internal static class RuntimeCollector
{
    /// <summary>Sets the heap size the collector aims to stay under, in bytes.</summary>
    [DllImport("isoplane_runtime", EntryPoint = "mono_gc_set_soft_heap_limit")]
    internal static extern void SetSoftHeapLimit(ulong softLimit);
}
