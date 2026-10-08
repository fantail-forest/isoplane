namespace Debugger.HotReload;

public static class Application
{
    /// <summary>Preserves a local across an edited statement in the active invocation.</summary>
    public static string Handle()
    {
        int value = 1;
        value += 10;
        System.Diagnostics.Debugger.Break();
        return value.ToString();
    }
}

/// <summary>Receives a field through a metadata update.</summary>
public static class Storage { }
