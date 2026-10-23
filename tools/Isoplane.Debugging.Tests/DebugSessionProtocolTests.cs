using System.Net;
using Isoplane.Build;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class DebugSessionProtocolTests
{
    [Fact]
    public async Task a_reader_preserves_coalesced_endpoint_and_reply_messages()
    {
        using var stream = new MemoryStream();
        using var writes = new SemaphoreSlim(1, 1);
        var endpoint = new IPEndPoint(IPAddress.Loopback, 12345);
        var package = Path.GetFullPath("application.tar");
        await DebugSessionProtocol.WriteEndpointAsync(stream, endpoint, package, null, CancellationToken.None);
        await DebugSessionProtocol.ReplyAsync(stream, writes, DebugSessionProtocol.PackageChanged, null, CancellationToken.None);
        await DebugSessionProtocol.ReplyAsync(stream, writes, "detachReady", "preparation failed", CancellationToken.None);
        stream.Position = 0;

        using var reader = new DebugSessionProtocol.Reader(stream);
        var target = await reader.ReadEndpointAsync(CancellationToken.None);
        Assert.Equal(endpoint, target.Guests);
        Assert.Equal(package, target.Package);
        var changed = await reader.ReadReplyAsync(CancellationToken.None);
        Assert.NotNull(changed);
        Assert.Equal(DebugSessionProtocol.PackageChanged, changed.Action);
        var ready = await reader.ReadReplyAsync(CancellationToken.None);
        Assert.NotNull(ready);
        Assert.Equal("detachReady", ready.Action);
        Assert.Equal("preparation failed", ready.Error);
        Assert.Null(await reader.ReadReplyAsync(CancellationToken.None));
    }

    [Fact]
    public async Task a_reader_preserves_coalesced_detach_commands()
    {
        using var commands = new MemoryStream();
        using var replies = new MemoryStream();
        using var stream = new DuplexStream(commands, replies);
        using var writes = new SemaphoreSlim(1, 1);
        await DebugSessionProtocol.RequestDetachAsync(commands, false, CancellationToken.None);
        await DebugSessionProtocol.RequestDetachAsync(commands, true, CancellationToken.None);
        commands.Position = 0;
        var preparations = 0;

        using var reader = new DebugSessionProtocol.Reader(stream);
        var detached = await reader.ReadTerminationAsync(writes, _ =>
        {
            preparations++;
            return Task.CompletedTask;
        }, CancellationToken.None);

        Assert.True(detached);
        Assert.Equal(1, preparations);
        replies.Position = 0;
        using var replyReader = new DebugSessionProtocol.Reader(replies);
        Assert.Equal("detachReady", (await replyReader.ReadReplyAsync(CancellationToken.None))?.Action);
        Assert.Equal("detached", (await replyReader.ReadReplyAsync(CancellationToken.None))?.Action);
        Assert.Null(await replyReader.ReadReplyAsync(CancellationToken.None));
    }

    private sealed class DuplexStream(Stream input, Stream output) : Stream
    {
        public override bool CanRead => input.CanRead;
        public override bool CanSeek => false;
        public override bool CanWrite => output.CanWrite;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override void Flush() => output.Flush();
        public override int Read(byte[] buffer, int offset, int count) => input.Read(buffer, offset, count);
        public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellation = default) => input.ReadAsync(buffer, cancellation);
        public override void Write(byte[] buffer, int offset, int count) => output.Write(buffer, offset, count);
        public override ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken cancellation = default) => output.WriteAsync(buffer, cancellation);
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
    }
}
