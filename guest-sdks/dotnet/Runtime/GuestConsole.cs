using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Extensions.Logging;

namespace Wisp.Runtime;

internal sealed class GuestConsole(int level, Action<int, string> emit) : TextWriter
{
    internal static void Install()
    {
        Console.SetOut(new GuestConsole((int)LogLevel.Information, Abi.GuestLog.Emit));
        Console.SetError(new GuestConsole((int)LogLevel.Error, Abi.GuestLog.Emit));
    }

    public override Encoding Encoding => Encoding.UTF8;
    public override void Write(char value) => emit(level, value.ToString());
    public override void Write(string? value) { if (value is not null) emit(level, value); }
    public override void Write(ReadOnlySpan<char> buffer) => emit(level, buffer.ToString());
    public override void Write(char[] buffer, int index, int count) => Write(buffer.AsSpan(index, count));
    public override void WriteLine() => emit(level, "");
    public override void WriteLine(string? value) => emit(level, value ?? "");
    public override void WriteLine(ReadOnlySpan<char> buffer) => Write(buffer);
    public override void WriteLine(char[] buffer, int index, int count) => Write(buffer.AsSpan(index, count));
    public override void Flush() { }
    public override Task FlushAsync() => Task.CompletedTask;
    public override Task FlushAsync(CancellationToken cancellationToken) => cancellationToken.IsCancellationRequested
        ? Task.FromCanceled(cancellationToken) : Task.CompletedTask;
    public override Task WriteAsync(char value) { Write(value); return Task.CompletedTask; }
    public override Task WriteAsync(string? value) { Write(value); return Task.CompletedTask; }
    public override Task WriteAsync(char[] buffer, int index, int count) { Write(buffer, index, count); return Task.CompletedTask; }
    public override Task WriteAsync(ReadOnlyMemory<char> buffer, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled(cancellationToken);
        Write(buffer.Span);
        return Task.CompletedTask;
    }
    public override Task WriteLineAsync() { WriteLine(); return Task.CompletedTask; }
    public override Task WriteLineAsync(char value) { Write(value); return Task.CompletedTask; }
    public override Task WriteLineAsync(string? value) { WriteLine(value); return Task.CompletedTask; }
    public override Task WriteLineAsync(char[] buffer, int index, int count) { WriteLine(buffer, index, count); return Task.CompletedTask; }
    public override Task WriteLineAsync(ReadOnlyMemory<char> buffer, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled(cancellationToken);
        WriteLine(buffer.Span);
        return Task.CompletedTask;
    }
}
