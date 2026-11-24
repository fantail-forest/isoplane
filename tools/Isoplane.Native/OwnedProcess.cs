using System.Diagnostics;
using Microsoft.Win32.SafeHandles;
using Isoplane.Native;
using Isoplane.Native.Interop;

namespace Isoplane.Processes;

internal sealed class OwnedProcess : IDisposable
{
    private readonly nint handle;
    private readonly Task<int> completion;
    private readonly TimeSpan grace;
    private bool disposed;
    private readonly StreamReader? output;
    private readonly StreamReader? errors;

    private OwnedProcess(nint handle, int pid, StreamWriter input, StreamReader? output, StreamReader? errors, TimeSpan grace)
    {
        this.handle = handle;
        Id = pid;
        StandardInput = input;
        this.output = output;
        this.errors = errors;
        this.grace = grace;
        // The native wait blocks until process-tree cleanup finishes; it must not
        // occupy a worker needed by redirected output and shutdown continuations.
        completion = Task.Factory.StartNew(Wait, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
    }

    internal StreamReader StandardOutput => output ?? throw new InvalidOperationException("Output was not redirected.");
    internal StreamReader StandardError => errors ?? throw new InvalidOperationException("Errors were not redirected.");
    internal StreamWriter StandardInput { get; }
    internal bool HasExited => completion.IsCompleted;
    internal int ExitCode => completion.IsCompleted ? completion.GetAwaiter().GetResult() : throw new InvalidOperationException("The owned process has not exited.");
    internal int Id { get; }

    internal static unsafe OwnedProcess Start(ProcessStartInfo child, TimeSpan grace)
    {
        NativeRuntime.EnsureLoaded();
        NativeProcessStarted started;
        using (var arguments = new NativeArguments())
        {
            var argumentList = child.ArgumentList.ToArray();
            var environment = child.Environment.Where(pair => pair.Value is not null).ToDictionary(pair => pair.Key, pair => pair.Value!);
            var settings = new NativeProcessStart
            {
                executable = arguments.Text(child.FileName),
                arguments = arguments.Texts(argumentList),
                arguments_length = (nuint)argumentList.Length,
                directory = arguments.Text(string.IsNullOrEmpty(child.WorkingDirectory) ? Environment.CurrentDirectory : child.WorkingDirectory),
                environment = arguments.Settings(environment),
                environment_length = (nuint)environment.Count,
                capture_output = child.RedirectStandardOutput,
                capture_errors = child.RedirectStandardError,
                cleanup_grace_ms = checked((ulong)grace.TotalMilliseconds),
            };
            NativeRuntime.Check(NativeMethods.isoplane_process_start(&settings, &started));
        }
        var handle = (nint)started.handle;
        var inputHandle = new SafeFileHandle((nint)started.stdin, ownsHandle: true);
        var outputHandle = child.RedirectStandardOutput ? new SafeFileHandle((nint)started.stdout, ownsHandle: true) : null;
        var errorHandle = child.RedirectStandardError ? new SafeFileHandle((nint)started.stderr, ownsHandle: true) : null;
        StreamWriter? input = null;
        StreamReader? output = null;
        StreamReader? errors = null;
        try
        {
            input = new StreamWriter(new FileStream(inputHandle, FileAccess.Write)) { AutoFlush = true };
            if (outputHandle is not null) output = new StreamReader(new FileStream(outputHandle, FileAccess.Read));
            if (errorHandle is not null) errors = new StreamReader(new FileStream(errorHandle, FileAccess.Read));
            return new OwnedProcess(handle, checked((int)started.pid), input, output, errors, grace);
        }
        catch
        {
            input?.Dispose(); output?.Dispose(); errors?.Dispose();
            inputHandle.Dispose(); outputHandle?.Dispose(); errorHandle?.Dispose();
            NativeMethods.isoplane_process_free((void*)handle);
            throw;
        }
    }

    private unsafe int Wait()
    {
        int exitCode;
        NativeRuntime.Check(NativeMethods.isoplane_process_wait((void*)handle, &exitCode));
        return exitCode;
    }

    internal async Task WaitForExitAsync(CancellationToken cancellation = default) => await completion.WaitAsync(cancellation);

    internal async Task StopAsync(TimeSpan deadline)
    {
        StandardInput.Close();
        if (!completion.IsCompleted && grace > TimeSpan.Zero)
        {
            try { await completion.WaitAsync(grace < deadline ? grace : deadline); }
            catch (TimeoutException) { /* The grace period ended; terminate the owned group below. */ }
        }
        Terminate();
        await completion.WaitAsync(deadline);
    }

    private unsafe void Terminate()
    {
        if (!disposed) NativeRuntime.Check(NativeMethods.isoplane_process_stop((void*)handle));
    }

    public unsafe void Dispose()
    {
        if (disposed) return;
        try
        {
            Terminate();
            completion.GetAwaiter().GetResult();
        }
        finally
        {
            disposed = true;
            StandardInput.Dispose(); output?.Dispose(); errors?.Dispose();
            NativeMethods.isoplane_process_free((void*)handle);
        }
    }
}
