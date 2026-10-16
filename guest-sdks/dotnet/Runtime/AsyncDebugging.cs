using System.Runtime.CompilerServices;
using System.Threading.Tasks;

namespace Isoplane.Runtime;

/// <summary>Enables guest-owned async state tracking for generated local debugger entrypoints.</summary>
public static class AsyncDebugging
{
    /// <summary>Enables await tracking and exposes its guest-owned state during one slice of a local debug build.</summary>
    public static Scope Enter() => new(GetState(null!));

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "EnableIsoplaneAsyncDebugging")]
    private static extern object GetState(Task task);

    /// <summary>Keeps the debugger's async-state reference visible throughout a slice.</summary>
    public readonly struct Scope : System.IDisposable
    {
        /// <summary>The guest-owned await graph retained throughout the slice.</summary>
        public readonly object State;

        internal Scope(object state) => State = state;

        /// <summary>Preserves the state reference until the slice has returned to the host.</summary>
        public void Dispose() => System.GC.KeepAlive(State);
    }
}
