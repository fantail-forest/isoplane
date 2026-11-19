using System;

namespace Debugger.HotReload;

public static partial class Application
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
}
