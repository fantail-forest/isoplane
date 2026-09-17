using System.Runtime.CompilerServices;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using System.Text.Json.Nodes;
using Wisp.Native.Interop;

namespace Wisp.Native;

internal sealed class LocalSession : IAsyncDisposable
{
    private readonly nint handle;
    private readonly Action<JsonObject> events;
    private readonly object access = new();
    private ExceptionDispatchInfo? callbackFailure;
    private bool disposed;

    internal Task Completion { get; }

    internal LocalSession(byte[] module, byte[] options, Action<JsonObject> events)
    {
        NativeRuntime.EnsureLoaded();
        this.events = events;
        handle = Create();
        Completion = Task.Run(() => Run(module, options));
    }

    private static unsafe nint Create() => (nint)NativeMethods.isoplane_session_create();

    internal unsafe void Reload(byte[] module, byte[] options)
    {
        lock (access)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* wasm = module)
            fixed (byte* settings = options)
                NativeRuntime.Read(NativeMethods.isoplane_session_reload((void*)handle, wasm, (nuint)module.Length, settings, (nuint)options.Length));
        }
    }

    internal unsafe void Stop()
    {
        lock (access)
        {
            if (!disposed) NativeRuntime.Read(NativeMethods.isoplane_session_stop((void*)handle));
        }
    }

    private unsafe void Run(byte[] module, byte[] options)
    {
        var context = GCHandle.Alloc(this);
        try
        {
            fixed (byte* wasm = module)
            fixed (byte* settings = options)
                NativeRuntime.Read(NativeMethods.isoplane_session_run((void*)handle, wasm, (nuint)module.Length,
                    settings, (nuint)options.Length, &OnEvent, (void*)GCHandle.ToIntPtr(context)));
            callbackFailure?.Throw();
        }
        finally { context.Free(); }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static unsafe void OnEvent(void* context, byte* data, nuint length)
    {
        var session = (LocalSession)GCHandle.FromIntPtr((nint)context).Target!;
        try
        {
            var message = JsonNode.Parse(new ReadOnlySpan<byte>(data, checked((int)length)))!.AsObject();
            session.events(message);
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
