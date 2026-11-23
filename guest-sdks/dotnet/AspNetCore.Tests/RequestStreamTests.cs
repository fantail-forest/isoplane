using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class RequestStreamTests
{
    [Fact]
    public async Task Empty_reads_do_not_call_the_host_even_when_upload_content_is_possible()
    {
        using var stream = new RequestStream(true);
        int count = await stream.ReadAsync(Memory<byte>.Empty);
        Assert.True(count == 0, "A zero-length read must succeed without invoking native host imports.");
    }

    [Fact]
    public async Task A_request_that_cannot_have_a_body_ends_on_every_read_without_calling_the_host()
    {
        using var stream = new RequestStream(false);
        byte[] buffer = new byte[1];
        Assert.True(await stream.ReadAsync(buffer) == 0 && await stream.ReadAsync(buffer) == 0, "The end of the body must remain repeatable without host calls.");
    }

    [Fact]
    public async Task Array_reads_reject_null_and_invalid_ranges_before_calling_the_host()
    {
        using var stream = new RequestStream(true);
        await Assert.ThrowsAsync<ArgumentNullException>(() => stream.ReadAsync(null!, 0, 0, default));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => stream.ReadAsync(new byte[1], -1, 1, default));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => stream.ReadAsync(new byte[1], 0, -1, default));
        await Assert.ThrowsAsync<ArgumentException>(() => stream.ReadAsync(new byte[1], 1, 1, default));
    }

    [Fact]
    public async Task Disposal_prevents_new_reads()
    {
        var stream = new RequestStream(true);
        stream.Dispose();
        await Assert.ThrowsAsync<ObjectDisposedException>(() => stream.ReadAsync(new byte[1]).AsTask());
        Assert.False(stream.CanRead, "A disposed upload must not advertise readability.");
    }

    [Fact]
    public async Task Cancellation_is_observed_before_a_read_starts()
    {
        using var stream = new RequestStream(true);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => stream.ReadAsync(new byte[1], cancellation.Token).AsTask());
    }
}
