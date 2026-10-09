using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.StatementIterators;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/statement-iterators", Handle);

    private static bool syncDisposed;
    private static bool asyncDisposed;

    private static async Task<string> Handle()
    {
        var values = new List<int>();
        using (var iterator = Sequence().GetEnumerator())
            for (int index = 0; index < 3; index++)
            {
                if (!iterator.MoveNext()) throw new InvalidOperationException("The synchronous iterator ended early.");
                values.Add(iterator.Current);
            }
        await using (var iterator = AsyncSequence().GetAsyncEnumerator())
            for (int index = 0; index < 3; index++)
            {
                if (!await iterator.MoveNextAsync()) throw new InvalidOperationException("The asynchronous iterator ended early.");
                values.Add(iterator.Current);
            }
        return $"{string.Join(",", values)}:{syncDisposed}:{asyncDisposed}";
    }

    private static IEnumerable<int> Sequence()
    {
        int syncItem = 1;
        try
        {
            syncItem += 10;
            yield return syncItem; // SYNC_FIRST_YIELD
            syncItem += 100;
            yield return syncItem; // SYNC_SECOND_YIELD
            syncItem += 1000;
        }
        finally { syncDisposed = true; }
    }

    private static async IAsyncEnumerable<int> AsyncSequence()
    {
        int asyncItem = 1;
        await Task.Delay(1);
        try
        {
            asyncItem += 10;
            yield return asyncItem; // ASYNC_FIRST_YIELD
            await Task.Delay(1);
            asyncItem += 100;
            yield return asyncItem; // ASYNC_SECOND_YIELD
            asyncItem += 1000;
        }
        finally { asyncDisposed = true; }
    }
}
