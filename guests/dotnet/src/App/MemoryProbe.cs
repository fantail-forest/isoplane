#if ISOPLANE_MEMORY_PROBE
using System;
using System.Threading.Tasks;
using Isoplane.Durable;

namespace Isoplane.App;

internal sealed class MemoryProbe : IIsoplaneHandler
{
    private readonly long _initialMemoryBytes;

    public MemoryProbe()
    {
        GC.Collect();
        _initialMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
    }

    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        GC.Collect();
        long currentMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Task.FromResult(Response.Text($"{_initialMemoryBytes}:{currentMemoryBytes}"));
    }
}
#endif
