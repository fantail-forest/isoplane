using System;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

internal static class MemoryProbe
{
    private static long _initialMemoryBytes;

    public static Task InitializeAsync()
    {
        GC.Collect();
        _initialMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Task.CompletedTask;
    }

    public static Task<Response> HandleAsync()
    {
        GC.Collect();
        long currentMemoryBytes = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Task.FromResult(Response.Text($"{_initialMemoryBytes}:{currentMemoryBytes}"));
    }
}
