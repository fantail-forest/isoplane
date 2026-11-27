using System.Collections.Generic;
using System.Threading.Tasks;

namespace Debugger.HotReload;

/// <summary>Iterator bodies edited while their current invocation is paused.</summary>
public static partial class Iterators
{
    /// <summary>Collects yielded values and verifies disposal after applying edits.</summary>
    public static async Task<string> Handle()
    {
        var values = new List<int>();
        foreach (var item in Sequence()) values.Add(item);
        await foreach (var item in AsyncSequence()) values.Add(item);
        return $"{string.Join(',', values)}:{syncDisposed}:{asyncDisposed}";
    }
}
