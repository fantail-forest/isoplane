using System.Threading.Tasks;

namespace Debugger.HotReload;

/// <summary>An async body edited in source while its invocation is paused after an await.</summary>
public static class AsyncRegion
{
    public static async Task<string> Handle()
    {
        int value = 1;
        await Task.Delay(1); // HOT_RELOAD_AWAIT
        value += 10;
        System.Diagnostics.Debugger.Break();
        return value.ToString();
    }
}
