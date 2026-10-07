using System.IO.Pipelines;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;

namespace Isoplane.AspNetCore;

internal sealed class StreamingResponse : IHttpResponseFeature, IHttpResponseBodyFeature, IDisposable
{
    private readonly ResponseTransport transport;
    private readonly Stack<(Func<object, Task> Callback, object State)> starting = new();
    private readonly Stack<(Func<object, Task> Callback, object State)> completed = new();
    private readonly ResponseStream stream;
    private PipeWriter? writer;
    private int status = 200;
    private string? reason;
    private IHeaderDictionary headers = new HeaderDictionary();
    private Task? start;
    private bool complete;

    internal StreamingResponse(ResponseTransport transport)
    {
        this.transport = transport;
        stream = new ResponseStream(this);
    }
    public int StatusCode { get => status; set { CheckMutable(); status = value; } }
    public string? ReasonPhrase { get => reason; set { CheckMutable(); reason = value; } }
    public IHeaderDictionary Headers { get => headers; set { CheckMutable(); headers = value; } }
    public bool HasStarted { get; private set; }
    public Stream Body { get => stream; set => throw new NotSupportedException("Replace the IHttpResponseBodyFeature to wrap an Isoplane response body."); }
    public Stream Stream => stream;
    public PipeWriter Writer => writer ??= PipeWriter.Create(stream, new StreamPipeWriterOptions(leaveOpen: true));

    private void CheckMutable()
    {
        if (HasStarted) throw new InvalidOperationException("The HTTP response has already started.");
    }
    public void OnStarting(Func<object, Task> callback, object state)
    {
        CheckMutable();
        starting.Push((callback, state));
    }
    public void OnCompleted(Func<object, Task> callback, object state) => completed.Push((callback, state));
    public Task StartAsync(CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return start ??= StartCoreAsync();
    }
    private async Task StartCoreAsync()
    {
        while (starting.TryPop(out var registration)) await registration.Callback(registration.State);
        headers = new HeaderDictionary(headers.ToDictionary(pair => pair.Key, pair => pair.Value)) { IsReadOnly = true };
        var pairs = new List<KeyValuePair<string, string>>();
        foreach (var header in headers)
            foreach (var value in header.Value)
                pairs.Add(new(header.Key, value ?? ""));
        transport.Start(checked((ushort)status), pairs);
        HasStarted = true;
    }
    public async Task CompleteAsync()
    {
        if (complete) return;
        if (writer is not null) await writer.FlushAsync();
        await StartAsync();
        complete = true;
    }
    internal async Task FinishCallbacksAsync()
    {
        List<Exception>? errors = null;
        while (completed.TryPop(out var registration))
        {
            try { await registration.Callback(registration.State); }
            catch (Exception error) { (errors ??= []).Add(error); }
        }
        if (errors is not null) throw new AggregateException("HTTP completion callbacks failed.", errors);
    }
    public void DisableBuffering()
    {
        // Every write already awaits the host transport without a response buffer.
    }
    // The host has no file to hand to a socket: an application's files are part of its package,
    // so a file is read here and written like any other body. Static file middleware sends this way.
    public Task SendFileAsync(string path, long offset, long? count, CancellationToken cancellationToken = default) =>
        SendFileFallback.SendFileAsync(stream, path, offset, count, cancellationToken);
    internal Response ToResponse()
    {
        if (!complete) throw new InvalidOperationException("The HTTP response has not completed.");
        return transport.Complete();
    }
    public void Dispose()
    {
        writer?.Complete();
        stream.Dispose();
    }
    private sealed class ResponseStream(StreamingResponse response) : Stream
    {
        public override bool CanRead => false;
        public override bool CanSeek => false;
        public override bool CanWrite => !response.complete;
        public override long Length => throw new NotSupportedException("HTTP response length is not seekable.");
        public override long Position { get => throw new NotSupportedException("HTTP response is not seekable."); set => throw new NotSupportedException("HTTP response is not seekable."); }
        public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException("HTTP response is write-only.");
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException("HTTP response is not seekable.");
        public override void SetLength(long value) => throw new NotSupportedException("HTTP response is not seekable.");
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException("Use asynchronous HTTP response writes.");
        public override void Flush() => throw new NotSupportedException("Use asynchronous HTTP response flushes.");
        public override async Task FlushAsync(CancellationToken cancellationToken)
        {
            await response.StartAsync(cancellationToken);
        }
        public override Task WriteAsync(byte[] buffer, int offset, int count, CancellationToken cancellationToken) =>
            WriteAsync(buffer.AsMemory(offset, count), cancellationToken).AsTask();
        public override async ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken cancellationToken = default)
        {
            if (response.complete) throw new InvalidOperationException("The HTTP response is complete.");
            await response.StartAsync(cancellationToken);
            await response.transport.WriteAsync(buffer, cancellationToken);
        }
    }
}
