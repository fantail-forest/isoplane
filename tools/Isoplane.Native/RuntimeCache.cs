using System.Text;
using System.Text.Json.Nodes;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>The per-user cache of compiled shared runtimes that local sessions fill.</summary>
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
