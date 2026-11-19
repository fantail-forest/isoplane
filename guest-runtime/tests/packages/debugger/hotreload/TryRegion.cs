namespace Debugger.HotReload;

/// <summary>A protected body edited in source while its invocation is paused inside it.</summary>
public static class TryRegion
{
    public static string Handle()
    {
        int value = 1;
        try
        {
            value += 10; // HOT_RELOAD_TRY
            System.Diagnostics.Debugger.Break();
        }
        finally { value += 1000; }
        return value.ToString();
    }
}
