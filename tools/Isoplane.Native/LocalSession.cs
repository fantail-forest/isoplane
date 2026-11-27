using System.Runtime.CompilerServices;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

internal sealed class LocalSession : IAsyncDisposable
{
    private readonly nint handle;
    private readonly Action<SessionEvent> events;
    private readonly object access = new();
    private ExceptionDispatchInfo? callbackFailure;
    private bool disposed;
    private TaskCompletionSource? detaching;

    internal Task Completion { get; }

    /// <summary>
    /// Starts <paramref name="module"/> under <paramref name="options"/>. <paramref name="events"/> receives each
    /// event the host reports, possibly concurrently; one it throws stops the session and fails <see cref="Completion"/>.
    /// </summary>
    internal LocalSession(byte[] module, LocalSessionOptions options, Action<SessionEvent> events)
    {
        NativeRuntime.EnsureLoaded();
        this.events = events;
        handle = Create();
        Completion = Task.Run(() => Run(module, options));
    }

    private static unsafe nint Create() => (nint)NativeMethods.isoplane_session_create();

    internal unsafe void Reload(byte[] module, LocalSessionOptions options)
    {
        using var arguments = new NativeArguments();
        var settings = options.Lend(arguments);
        lock (access)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* wasm = module)
                NativeRuntime.Check(NativeMethods.isoplane_session_reload((void*)handle, wasm, (nuint)module.Length, &settings));
        }
    }

    internal async Task PrepareDetachAsync(byte[] module, LocalSessionOptions options)
    {
        var prepared = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        if (Interlocked.CompareExchange(ref detaching, prepared, null) is not null)
            throw new InvalidOperationException("Debugger detachment is already being prepared.");
        try
        {
            Reload(module, options);
            if (await Task.WhenAny(prepared.Task, Completion) == Completion)
            {
                await Completion;
                throw new IOException("The local host ended before preparing debugger detachment.");
            }
            await prepared.Task;
        }
        finally { Interlocked.CompareExchange(ref detaching, null, prepared); }
    }

    internal unsafe void Stop()
    {
        lock (access)
        {
            if (!disposed) NativeRuntime.Check(NativeMethods.isoplane_session_stop((void*)handle));
        }
    }

    private unsafe void Run(byte[] module, LocalSessionOptions options)
    {
        var context = GCHandle.Alloc(this);
        try
        {
            using var arguments = new NativeArguments();
            var settings = options.Lend(arguments);
            fixed (byte* wasm = module)
                NativeRuntime.Check(NativeMethods.isoplane_session_run((void*)handle, wasm, (nuint)module.Length,
                    &settings, &OnEvent, (void*)GCHandle.ToIntPtr(context)));
            callbackFailure?.Throw();
        }
        finally { context.Free(); }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static unsafe void OnEvent(void* context, NativeEvent* reported)
    {
        var session = (LocalSession)GCHandle.FromIntPtr((nint)context).Target!;
        try
        {
            var message = SessionEvent.Read(reported);
            switch (message)
            {
                case SessionEvent.DebuggerDetachPrepared:
                    Volatile.Read(ref session.detaching)?.TrySetResult();
                    return;
                case SessionEvent.DebuggerDetachFailed failed:
                    Volatile.Read(ref session.detaching)?.TrySetException(new InvalidOperationException(failed.Detail));
                    return;
                default:
                    session.events(message);
                    break;
            }
        }
        catch (Exception error)
        {
            Interlocked.CompareExchange(ref session.callbackFailure, ExceptionDispatchInfo.Capture(error), null);
            try { session.Stop(); }
            catch (Exception stopError)
            {
                // Both errors are retained for the managed operation's completion.
                session.callbackFailure = ExceptionDispatchInfo.Capture(new AggregateException(error, stopError));
            }
        }
    }

    public async ValueTask DisposeAsync()
    {
        Stop();
        try { await Completion; }
        finally { Release(); }
    }

    private unsafe void Release()
    {
        lock (access)
        {
            if (disposed) return;
            disposed = true;
            NativeMethods.isoplane_session_free((void*)handle);
        }
    }
}
