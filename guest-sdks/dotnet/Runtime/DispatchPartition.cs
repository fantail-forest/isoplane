using System;
using System.Runtime.CompilerServices;
using System.Threading;

namespace Isoplane.Runtime;

/// <summary>
/// The runtime's thread-pool queue, partitioned by dispatch. Work queued under
/// a dispatch's execution context waits for a slice of that dispatch, so a
/// slice runs only its own dispatch's code and the host futures that code
/// starts belong to it.
/// </summary>
internal static class DispatchPartition
{
    /// <summary>Opens a partition for a dispatch that has begun.</summary>
    internal static void Open(ulong dispatch)
    {
        // Only a guest's runtime partitions its thread pool; tooling and tests
        // run the SDK on a desktop runtime, which has neither the queue nor the accessors.
        if (OperatingSystem.IsWasi()) Open(null, (long)dispatch);
    }

    /// <summary>Begins a slice of the dispatch: the work waiting for it becomes runnable.</summary>
    internal static void Enter(ulong dispatch)
    {
        if (OperatingSystem.IsWasi()) Enter(null, (long)dispatch);
    }

    /// <summary>Whether work queued in another slice is waiting for the dispatch.</summary>
    internal static bool HasWaiting(ulong dispatch) =>
        OperatingSystem.IsWasi() && HasWaiting(null, (long)dispatch);

    /// <summary>Closes the dispatch's partition: what still waits for it becomes the instance's.</summary>
    internal static void Close(ulong dispatch)
    {
        if (OperatingSystem.IsWasi()) Close(null, (long)dispatch);
    }

    /// <summary>
    /// Records <paramref name="dispatch"/> on the current execution context
    /// until disposed, so everything the dispatch's code starts carries it.
    /// </summary>
    internal static Marked Mark(ulong dispatch)
    {
        if (!OperatingSystem.IsWasi()) return default;
        var local = Local(null);
        local.Value = (long)dispatch;
        return new Marked(local);
    }

    internal readonly struct Marked : IDisposable
    {
        private readonly AsyncLocal<long>? _local;
        internal Marked(AsyncLocal<long> local) => _local = local;
        public void Dispose()
        {
            if (_local is not null) _local.Value = 0;
        }
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneDispatch")]
    private static extern AsyncLocal<long> Local(ExecutionContext? anchor);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneOpen")]
    private static extern void Open(ExecutionContext? anchor, long dispatch);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneEnter")]
    private static extern void Enter(ExecutionContext? anchor, long dispatch);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneHasWaiting")]
    private static extern bool HasWaiting(ExecutionContext? anchor, long dispatch);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneClose")]
    private static extern void Close(ExecutionContext? anchor, long dispatch);
}
