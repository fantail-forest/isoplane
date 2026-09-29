using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Threading.Tasks;

namespace Acceptance;

internal sealed partial class Reflection
{
    private static string MissingNativeLibrary(string libraryName)
    {
        try
        {
            IntPtr library = NativeLibrary.Load(libraryName);
            NativeLibrary.Free(library);
        }
        catch (DllNotFoundException) { return "DllNotFoundException"; }
        catch (PlatformNotSupportedException) { return "PlatformNotSupportedException"; }
        throw new InvalidOperationException("missing native library unexpectedly loaded");
    }

    private static async Task<string> NativeLibraryAfterPark(string operation)
    {
        string name = operation == "host" ? "libc.so.6" : "isoplane_missing_native_library";
        string before = MissingNativeLibrary(name);
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + MissingNativeLibrary(name);
    }

    private static async Task<string> NativeDeclarationAfterPark()
    {
        string before = Isoplane.Abi.NativeLibraryProbe.Invoke();
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + Isoplane.Abi.NativeLibraryProbe.Invoke();
    }

    private static async Task<string> NativeSymbolAfterPark(string operation)
    {
        string before = Isoplane.Abi.NativeLibraryProbe.Invoke(operation);
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + Isoplane.Abi.NativeLibraryProbe.Invoke(operation);
    }

    private static async Task<string> NativePayloadAfterPark(string operation)
    {
        using var stream = typeof(Reflection).Assembly.GetManifestResourceStream("dynamic-payload")
            ?? throw new InvalidOperationException("embedded dynamic payload is missing");
        byte[] bytes = new byte[checked((int)stream.Length)];
        stream.ReadExactly(bytes);
        Assembly assembly = Assembly.Load(bytes);
        MethodInfo method = assembly.GetType("DynamicPayload.Operations", throwOnError: true)!
            .GetMethod("Native") ?? throw new MissingMethodException("payload Native");
        string before = (string)method.Invoke(null, new object[] { operation })!;
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + method.Invoke(null, new object[] { operation });
    }
}
