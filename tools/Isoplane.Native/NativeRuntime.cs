using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json.Nodes;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

internal static class NativeRuntime
{
    private static readonly Lazy<bool> Loaded = new(Load);
    internal static void EnsureLoaded() => _ = Loaded.Value;
    internal static string LibraryFilename => OperatingSystem.IsWindows() ? "isoplane_native.dll" : OperatingSystem.IsMacOS() ? "libisoplane_native.dylib" : "libisoplane_native.so";
    internal static string LibraryPath
    {
        get
        {
            var directory = Path.GetDirectoryName(typeof(NativeRuntime).Assembly.Location)!;
            var bundled = Path.Combine(directory, "runtime", LibraryFilename);
            if (File.Exists(bundled)) return bundled;
            var platform = OperatingSystem.IsLinux() ? "linux" : OperatingSystem.IsMacOS() ? "osx" : "win";
            var rid = platform + "-" + RuntimeInformation.ProcessArchitecture.ToString().ToLowerInvariant();
            var asset = Path.Combine(directory, "runtimes", rid, "native", LibraryFilename);
            return File.Exists(asset) ? asset : Path.Combine(directory, LibraryFilename);
        }
    }

    internal static unsafe JsonObject Read(NativeResult result)
    {
        try
        {
            var payload = Encoding.UTF8.GetString(new ReadOnlySpan<byte>(result.data, checked((int)result.length)));
            return result.status switch
            {
                NativeStatus.Success => JsonNode.Parse(payload)?.AsObject()
                    ?? throw new InvalidOperationException("Native operation returned an empty report."),
                NativeStatus.InvalidInput => throw new ArgumentException(payload),
                NativeStatus.ProcessError => throw new IOException(payload),
                NativeStatus.InternalError => throw new InvalidOperationException(payload),
                _ => throw new InvalidOperationException("Unknown native result; reinstall a matching Isoplane package.")
            };
        }
        finally { NativeMethods.isoplane_result_free(result); }
    }

    private static bool Load()
    {
        NativeLibrary.SetDllImportResolver(typeof(NativeRuntime).Assembly, Resolve);
        try
        {
            var version = NativeMethods.isoplane_abi_version();
            if (version != NativeMethods.ABI_VERSION)
                throw new InvalidOperationException($"Isoplane native library ABI {version} does not match managed ABI {NativeMethods.ABI_VERSION}; reinstall a complete matching package.");
            return true;
        }
        catch (Exception error) when (error is DllNotFoundException or BadImageFormatException or EntryPointNotFoundException)
        {
            throw new InvalidOperationException($"Cannot load the Isoplane native library for {RuntimeInformation.RuntimeIdentifier}; reinstall a complete matching package. {error.Message}", error);
        }
    }

    private static nint Resolve(string name, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (name != "isoplane_native") return 0;
        return NativeLibrary.Load(LibraryPath);
    }
}
