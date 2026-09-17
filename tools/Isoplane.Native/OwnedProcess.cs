using System.Diagnostics;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;
using Wisp.Native;
using Wisp.Native.Interop;

namespace Wisp.Processes;

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
        completion = Task.Run(Wait);
    }

    internal StreamReader StandardOutput => output ?? throw new InvalidOperationException("Output was not redirected.");
    internal StreamReader StandardError => errors ?? throw new InvalidOperationException("Errors were not redirected.");
    internal StreamWriter StandardInput { get; }
    internal bool HasExited => completion.IsCompleted;
    internal int ExitCode => completion.IsCompleted ? completion.GetAwaiter().GetResult() : throw new InvalidOperationException("The owned process has not exited.");
    internal int Id { get; }

    internal static unsafe OwnedProcess Start(ProcessStartInfo child, TimeSpan grace)
    {
        if (!OperatingSystem.IsLinux()) throw new PlatformNotSupportedException("Local process ownership is currently supported on Linux.");
        NativeRuntime.EnsureLoaded();
        var settings = JsonSerializer.SerializeToUtf8Bytes(new
        {
            executable = child.FileName, arguments = child.ArgumentList.ToArray(),
            directory = string.IsNullOrEmpty(child.WorkingDirectory) ? Environment.CurrentDirectory : child.WorkingDirectory,
            environment = child.Environment.Where(pair => pair.Value is not null).ToDictionary(pair => pair.Key, pair => pair.Value!),
            capture_output = child.RedirectStandardOutput, capture_errors = child.RedirectStandardError,
        });
        fixed (byte* data = settings)
        {
            var result = NativeRuntime.Read(NativeMethods.isoplane_process_start(data, (nuint)settings.Length));
            var handle = (nint)result["handle"]!.GetValue<ulong>();
            var inputHandle = new SafeFileHandle((nint)result["stdin"]!.GetValue<int>(), ownsHandle: true);
            var outputHandle = result["stdout"] is { } stdout ? new SafeFileHandle((nint)stdout.GetValue<int>(), ownsHandle: true) : null;
            var errorHandle = result["stderr"] is { } stderr ? new SafeFileHandle((nint)stderr.GetValue<int>(), ownsHandle: true) : null;
            StreamWriter? input = null;
            StreamReader? output = null;
            StreamReader? errors = null;
            try
            {
                input = new StreamWriter(new FileStream(inputHandle, FileAccess.Write)) { AutoFlush = true };
                if (outputHandle is not null) output = new StreamReader(new FileStream(outputHandle, FileAccess.Read));
                if (errorHandle is not null) errors = new StreamReader(new FileStream(errorHandle, FileAccess.Read));
                return new OwnedProcess(handle, result["pid"]!.GetValue<int>(), input, output, errors, grace);
            }
            catch
            {
                input?.Dispose(); output?.Dispose(); errors?.Dispose();
                inputHandle.Dispose(); outputHandle?.Dispose(); errorHandle?.Dispose();
                NativeMethods.isoplane_process_free((void*)handle);
                throw;
            }
        }
    }

    private unsafe int Wait() => NativeRuntime.Read(NativeMethods.isoplane_process_wait((void*)handle))["exit_code"]!.GetValue<int>();

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
        if (!disposed) NativeRuntime.Read(NativeMethods.isoplane_process_stop((void*)handle));
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
