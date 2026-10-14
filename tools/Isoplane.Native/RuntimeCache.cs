using System.Text;
using System.Text.Json.Nodes;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>The per-user cache of compiled shared runtimes that local sessions fill, or that is prepared ahead of them.</summary>
internal static class RuntimeCache
{
    /// <summary>
    /// Reports the cache at the absolute <paramref name="directory"/>: <c>budget_bytes</c> and, for each
    /// compiled runtime in <c>entries</c>, its <c>bytes</c> and <c>last_used_unix_ms</c>. A directory that
    /// does not exist is an empty cache and is not created.
    /// </summary>
    internal static unsafe JsonObject Inventory(string directory)
    {
        NativeRuntime.EnsureLoaded();
        var path = Encoding.UTF8.GetBytes(directory);
        fixed (byte* data = path)
            return NativeRuntime.Read(NativeMethods.isoplane_runtime_cache_inventory(data, (nuint)path.Length));
    }

    /// <summary>
    /// Compiles the shared runtime <paramref name="wasm"/> and its debug build <paramref name="debugWasm"/>
    /// into the cache, as the first session and the first debug session on them would.
    /// <paramref name="options"/> is the JSON the native entry point documents. Reports
    /// <c>baseline_compiled</c> and <c>debug_compiled</c>: whether each had to be compiled.
    /// </summary>
    internal static unsafe JsonObject Prepare(byte[] wasm, byte[] debugWasm, byte[] options)
    {
        NativeRuntime.EnsureLoaded();
        fixed (byte* module = wasm)
        fixed (byte* debug = debugWasm)
        fixed (byte* settings = options)
            return NativeRuntime.Read(NativeMethods.isoplane_runtime_prepare(
                module, (nuint)wasm.Length, debug, (nuint)debugWasm.Length, settings, (nuint)options.Length));
    }

    /// <summary>
    /// Removes every compiled runtime from the cache at the absolute <paramref name="directory"/>, including
    /// one a running session uses. Reports <c>removed_bytes</c>, the <c>entries</c> that remain and, in
    /// <c>failures</c>, why each of those could not be removed.
    /// </summary>
    internal static unsafe JsonObject Clear(string directory)
    {
        NativeRuntime.EnsureLoaded();
        var path = Encoding.UTF8.GetBytes(directory);
        fixed (byte* data = path)
            return NativeRuntime.Read(NativeMethods.isoplane_runtime_cache_clear(data, (nuint)path.Length));
    }
}
