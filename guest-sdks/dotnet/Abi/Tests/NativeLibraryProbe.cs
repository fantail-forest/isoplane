using System;
using System.Runtime.InteropServices;

namespace Wisp.Abi;

internal static class NativeLibraryProbe
{
    [DllImport("wisp_missing_native_library", EntryPoint = "wisp_missing_native_symbol")]
    private static extern int MissingLibrary();

    internal static string Invoke()
    {
        try { _ = MissingLibrary(); }
        catch (DllNotFoundException) { return "DllNotFoundException"; }
        catch (PlatformNotSupportedException) { return "PlatformNotSupportedException"; }
        throw new InvalidOperationException("unsupported native invocation unexpectedly returned");
    }
}
