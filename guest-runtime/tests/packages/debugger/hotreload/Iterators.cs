using System.Collections.Generic;
using System.Threading.Tasks;

namespace Debugger.HotReload;

/// <summary>Iterator bodies edited while their current invocation is paused.</summary>
public static class Iterators
{
    private static bool syncDisposed;
    private static bool asyncDisposed;

    /// <summary>Collects yielded values and verifies disposal after applying edits.</summary>
    public static async Task<string> Handle()
    {
        var values = new List<int>();
        foreach (var item in Sequence()) values.Add(item);
        await foreach (var item in AsyncSequence()) values.Add(item);
        return $"{string.Join(',', values)}:{syncDisposed}:{asyncDisposed}";
    }

    private static IEnumerable<int> Sequence()
    {
        int syncItem = 1;
        try
        {
            syncItem += 10; // SYNC_EDIT
            yield return syncItem;
            syncItem += 100; // SYNC_RESUME
            yield return syncItem;
        }
        finally
        {
            syncDisposed = true;
        }
    }

    private static async IAsyncEnumerable<int> AsyncSequence()
    {
        int asyncItem = 1;
        await Task.Delay(1);
        try
        {
            asyncItem += 10; // ASYNC_EDIT
            yield return asyncItem;
            await Task.Delay(1);
            asyncItem += 100; // ASYNC_RESUME
            yield return asyncItem;
        }
        finally
        {
            asyncDisposed = true;
        }
    }
}
