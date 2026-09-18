using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class RequestStreamTests
{
    [Fact]
    public async Task Empty_reads_do_not_call_the_host_even_when_upload_content_is_possible()
    {
        using var stream = new RequestStream(ReadOnlyMemory<byte>.Empty, true);
        int count = await stream.ReadAsync(Memory<byte>.Empty);
        Assert.True(count == 0, "A zero-length read must succeed without invoking native host imports.");
    }

    [Fact]
    public async Task Inline_bytes_are_preserved_before_cached_eof()
    {
        using var stream = new RequestStream("AB"u8.ToArray(), false);
        byte[] buffer = new byte[1];
        Assert.True(await stream.ReadAsync(buffer) == 1 && buffer[0] == (byte)'A', "The first read consumes only its requested prefix.");
        Assert.True(await stream.ReadAsync(buffer) == 1 && buffer[0] == (byte)'B', "The remaining inline byte must not disappear.");
        Assert.True(await stream.ReadAsync(buffer) == 0 && await stream.ReadAsync(buffer) == 0, "Cached EOF must remain repeatable without host calls.");
    }

    [Fact]
    public async Task Array_reads_reject_null_and_invalid_ranges_before_calling_the_host()
    {
        using var stream = new RequestStream(ReadOnlyMemory<byte>.Empty, true);
        await Assert.ThrowsAsync<ArgumentNullException>(() => stream.ReadAsync(null!, 0, 0, default));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => stream.ReadAsync(new byte[1], -1, 1, default));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => stream.ReadAsync(new byte[1], 0, -1, default));
        await Assert.ThrowsAsync<ArgumentException>(() => stream.ReadAsync(new byte[1], 1, 1, default));
    }

    [Fact]
    public async Task Disposal_prevents_new_reads()
    {
        var stream = new RequestStream("A"u8.ToArray(), true);
        stream.Dispose();
        await Assert.ThrowsAsync<ObjectDisposedException>(() => stream.ReadAsync(new byte[1]).AsTask());
        Assert.False(stream.CanRead, "A disposed upload must not advertise readability.");
    }

    [Fact]
    public async Task Cancellation_is_observed_before_a_read_starts()
    {
        using var stream = new RequestStream(ReadOnlyMemory<byte>.Empty, true);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => stream.ReadAsync(new byte[1], cancellation.Token).AsTask());
    }
}
