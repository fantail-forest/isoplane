using System.Text;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>The compiled runtimes a cache holds, and the most it keeps before it removes the ones used least recently.</summary>
internal sealed record RuntimeCacheInventory(ulong BudgetBytes, CompiledRuntime[] Entries);

/// <summary>
/// What clearing a cache did: the bytes removed, the runtimes that remain and why each of those could not be
/// removed.
/// </summary>
internal sealed record RuntimeCacheCleared(RuntimeCacheInventory Remaining, ulong RemovedBytes, string[] Failures);

/// <summary>What a shared runtime is compiled for ahead of its first session.</summary>
/// <param name="RuntimeLibrary">The native library, whose identity the compiled runtime depends on.</param>
/// <param name="RuntimeCacheDirectory">The absolute directory of the cache to compile into.</param>
/// <param name="MachineRoots">The machine's trusted roots exported as PEM, or null to trust the shipped roots alone.</param>
/// <param name="RuntimeIdentity">The text of the runtime's <c>runtime.json</c>.</param>
internal sealed record RuntimePreparationOptions(ApplicationManifest Manifest, string RuntimeLibrary, string RuntimeCacheDirectory, string? MachineRoots, string RuntimeIdentity);

/// <summary>The per-user cache of compiled shared runtimes that local sessions fill, or that is prepared ahead of them.</summary>
internal static class RuntimeCache
{
    /// <summary>
    /// Reports the cache at the absolute <paramref name="directory"/>. A directory that does not exist is an
    /// empty cache and is not created.
    /// </summary>
    internal static unsafe RuntimeCacheInventory Inventory(string directory)
    {
        NativeRuntime.EnsureLoaded();
        var path = Encoding.UTF8.GetBytes(directory);
        NativeCacheInventory inventory;
        fixed (byte* data = path)
            NativeRuntime.Check(NativeMethods.isoplane_runtime_cache_inventory(data, (nuint)path.Length, &inventory));
        try { return Copy(inventory); }
        finally { NativeMethods.isoplane_runtime_cache_inventory_free(inventory); }
    }

    /// <summary>
    /// Compiles the shared runtime <paramref name="wasm"/> and its debug build <paramref name="debugWasm"/>
    /// into the cache, as the first session and the first debug session on them would, and reports whether
    /// each had to be compiled.
    /// </summary>
    internal static unsafe RuntimePreparation Prepare(byte[] wasm, byte[] debugWasm, RuntimePreparationOptions options)
    {
        NativeRuntime.EnsureLoaded();
        using var arguments = new NativeArguments();
        var settings = new NativeRuntimeOptions
        {
            manifest = options.Manifest.Lend(arguments),
            runtime_library = arguments.Text(options.RuntimeLibrary),
            runtime_cache_directory = arguments.Text(options.RuntimeCacheDirectory),
            machine_roots = arguments.Text(options.MachineRoots),
            runtime_identity = arguments.Text(options.RuntimeIdentity),
        };
        RuntimePreparation prepared;
        fixed (byte* module = wasm)
        fixed (byte* debug = debugWasm)
            NativeRuntime.Check(NativeMethods.isoplane_runtime_prepare(
                module, (nuint)wasm.Length, debug, (nuint)debugWasm.Length, &settings, &prepared));
        return prepared;
    }

    /// <summary>
    /// Removes every compiled runtime from the cache at the absolute <paramref name="directory"/>, including
    /// one a running session uses.
    /// </summary>
    internal static unsafe RuntimeCacheCleared Clear(string directory)
    {
        NativeRuntime.EnsureLoaded();
        var path = Encoding.UTF8.GetBytes(directory);
        NativeCacheCleared cleared;
        fixed (byte* data = path)
            NativeRuntime.Check(NativeMethods.isoplane_runtime_cache_clear(data, (nuint)path.Length, &cleared));
        try
        {
            var failures = new ReadOnlySpan<NativeText>(cleared.failures, checked((int)cleared.failures_length)).ToArray();
            return new(Copy(cleared.remaining), cleared.removed_bytes, Array.ConvertAll(failures, NativeRuntime.Text));
        }
        finally { NativeMethods.isoplane_runtime_cache_cleared_free(cleared); }
    }

    private static unsafe RuntimeCacheInventory Copy(NativeCacheInventory inventory) =>
        new(inventory.budget_bytes, new ReadOnlySpan<CompiledRuntime>(inventory.entries, checked((int)inventory.entries_length)).ToArray());
}
