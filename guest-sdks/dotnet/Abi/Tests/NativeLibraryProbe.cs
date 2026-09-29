using System;
using System.Diagnostics.CodeAnalysis;
using System.Runtime.InteropServices;

namespace Isoplane.Abi;

internal static class NativeLibraryProbe
{
    [DllImport("isoplane_missing_native_library", EntryPoint = "isoplane_missing_native_symbol")]
    private static extern int MissingLibrary();

    [DllImport("libSystem.Native", EntryPoint = "isoplane_missing_native_symbol")]
    private static extern int MissingSymbol();

    [DllImport("__Internal", EntryPoint = "isoplane_missing_internal_symbol")]
    private static extern int MissingInternalSymbol();

    [DllImport("*", EntryPoint = "isoplane_missing_star_symbol")]
    private static extern int MissingStarSymbol();

    [DllImport("libSystem.Native", EntryPoint = "SystemNative_GetTimestamp")]
    private static extern long Timestamp();

    [DllImport("libSystem.Native", EntryPoint = "isoplane_unused_native_symbol")]
    private static extern int UnusedMissingSymbol();

    internal static string Invoke()
    {
        try { _ = MissingLibrary(); }
        catch (DllNotFoundException) { return "DllNotFoundException"; }
        catch (PlatformNotSupportedException) { return "PlatformNotSupportedException"; }
        throw new InvalidOperationException("unsupported native invocation unexpectedly returned");
    }

    [DynamicDependency(nameof(UnusedMissingSymbol), typeof(NativeLibraryProbe))]
    internal static string Invoke(string operation)
    {
        try
        {
            if (operation == "symbol") _ = MissingSymbol();
            else if (operation == "internal") _ = MissingInternalSymbol();
            else if (operation == "star") _ = MissingStarSymbol();
            else if (operation == "timestamp") { _ = Timestamp(); return "prelinked-call-returned"; }
            else throw new ArgumentException("unknown native probe");
        }
        catch (DllNotFoundException) { return "DllNotFoundException"; }
        catch (EntryPointNotFoundException) { return "EntryPointNotFoundException"; }
        catch (PlatformNotSupportedException) { return "PlatformNotSupportedException"; }
        throw new InvalidOperationException("unsupported native invocation unexpectedly returned");
    }
}
