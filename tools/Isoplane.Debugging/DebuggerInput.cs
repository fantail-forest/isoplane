namespace Isoplane.Debugging;

// Console input does not interrupt a pending read on cancellation. The DAP reader is
// a foreground thread, so its wait must be cancellable for the CLI process to exit.
internal sealed class DebuggerInput(Stream input) : Stream
{
    public override bool CanRead => true;
    public override bool CanSeek => false;
    public override bool CanWrite => false;
    public override long Length => throw new NotSupportedException("Debugger input is not seekable.");
    public override long Position
    {
        get => throw new NotSupportedException("Debugger input is not seekable.");
        set => throw new NotSupportedException("Debugger input is not seekable.");
    }

    public override Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken cancellation) =>
        input.ReadAsync(buffer, offset, count, cancellation).WaitAsync(cancellation);

    public override int Read(byte[] buffer, int offset, int count) => input.Read(buffer, offset, count);
    public override void Flush() => throw new NotSupportedException("Debugger input is read-only.");
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException("Debugger input is not seekable.");
    public override void SetLength(long value) => throw new NotSupportedException("Debugger input is read-only.");
    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException("Debugger input is read-only.");

    protected override void Dispose(bool disposing)
    {
        if (disposing) input.Dispose();
        base.Dispose(disposing);
    }
}
