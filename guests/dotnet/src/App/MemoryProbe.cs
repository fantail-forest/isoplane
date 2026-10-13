#if ISOPLANE_MEMORY_PROBE
using System;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

internal static class MemoryProbe
{
    private static long _initialMemoryBytes;

    [Initializer]
    public static Task InitializeAsync()
    {
        GC.Collect();
        _initialMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Task.CompletedTask;
    }

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {
        GC.Collect();
        long currentMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Task.FromResult(Response.Text($"{_initialMemoryBytes}:{currentMemoryBytes}"));
    }
}
#endif
