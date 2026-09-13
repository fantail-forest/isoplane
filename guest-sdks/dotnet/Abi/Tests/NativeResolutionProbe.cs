using System;
using System.Runtime.InteropServices;

namespace Wisp.Abi;

internal static class NativeResolutionProbe
{
    [DllImport("wisp_missing_native_library", EntryPoint = "wisp_missing_native_symbol")]
    private static extern int MissingLibrary();

    [DllImport("libSystem.Native", EntryPoint = "wisp_missing_native_symbol")]
    private static extern int MissingSymbol();

    [DllImport("libSystem.Native", EntryPoint = "SystemNative_GetTimestamp")]
    private static extern long Timestamp();

    internal static string Invoke(string operation)
    {
        try
        {
            if (operation == "library") _ = MissingLibrary();
            else if (operation == "symbol") _ = MissingSymbol();
            else if (operation == "timestamp") { _ = Timestamp(); return "prelinked-call-returned"; }
            else throw new ArgumentException("unknown native probe");
        }
        catch (DllNotFoundException) { return "DllNotFoundException"; }
        catch (EntryPointNotFoundException) { return "EntryPointNotFoundException"; }
        catch (PlatformNotSupportedException) { return "PlatformNotSupportedException"; }
        throw new InvalidOperationException("unsupported native invocation unexpectedly returned");
    }
}
