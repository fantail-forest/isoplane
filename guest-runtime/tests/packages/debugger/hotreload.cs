using System;
using System.Threading.Tasks;

namespace Debugger.HotReload;

/// <summary>Active invocations exercised across compiler updates.</summary>
public static class Application
{
    /// <summary>Retains a catch continuation while another method is updated.</summary>
    public static string HandleUnchanged()
    {
        int value = 1;
        try
        {
            try { throw new InvalidOperationException(); }
            catch (InvalidOperationException)
            {
                value += 10; // HOT_RELOAD_UNCHANGED
                System.Diagnostics.Debugger.Break();
                throw;
            }
        }
        catch (InvalidOperationException)
        {
            // Consuming the rethrow lets the response prove that the outer handler was reached.
        }
        return value.ToString();
    }

    /// <summary>Requires matching local storage before redirecting a live invocation.</summary>
    public static string HandleLayout()
    {
        int value = 1;
        value += 10; // HOT_RELOAD_LAYOUT
        System.Diagnostics.Debugger.Break();
        return value.ToString();
    }

    /// <summary>Preserves a local across an edited statement in the active invocation.</summary>
    public static string Handle()
    {
        int value = 1;
        value += 10; // HOT_RELOAD_BODY
        System.Diagnostics.Debugger.Break();
        return value.ToString();
    }

    /// <summary>Runs finally cleanup after adopting an edited try body.</summary>
    public static string HandleTry()
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

    /// <summary>Reenters an await after adopting edited async instructions.</summary>
    public static async Task<string> HandleAsync()
    {
        int value = 1;
        await Task.Delay(1); // HOT_RELOAD_AWAIT
        value += 10;
        System.Diagnostics.Debugger.Break();
        return value.ToString();
    }
}

/// <summary>Receives a field through a metadata update.</summary>
public static class Storage { }
