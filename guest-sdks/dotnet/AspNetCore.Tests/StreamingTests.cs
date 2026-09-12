using Microsoft.Extensions.Logging;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;
using Microsoft.Extensions.DependencyInjection;
using Wisp.Abi;
using Xunit;

namespace Wisp.AspNetCore.Tests;

public sealed class StreamingTests
{
    [Fact]
    public async Task response_headers_and_first_chunk_are_visible_while_the_handler_is_pending()
    {
        var transport = new RecordingResponseTransport();
        var release = new TaskCompletionSource();
        var first = new TaskCompletionSource();
        var builder = Builder(transport);
        await using var app = builder.Build();
        app.Run(async context =>
        {
            context.Features.Get<IHttpResponseBodyFeature>()!.DisableBuffering();
            await context.Response.Body.WriteAsync("A"u8.ToArray());
            first.SetResult();
            await release.Task;
            await context.Response.Body.WriteAsync("B"u8.ToArray());
        });
        var request = new WispApplication(app).HandleAsync(Request());
        await first.Task.WaitAsync(TimeSpan.FromSeconds(10));
        Assert.True(transport.Started.Task.IsCompleted && !request.IsCompleted,
            "response headers must be published before the waiting handler completes");
        Assert.True(transport.Bytes.ToArray().SequenceEqual("A"u8.ToArray()),
            "the first chunk must reach transport while the handler waits");
        release.SetResult();
        var response = await request;
        Assert.True(response.Body.SequenceEqual("AB"u8.ToArray()), "released writes preserve their order");
    }

    [Fact]
    public async Task response_write_awaits_transport_acceptance()
    {
        var entered = new TaskCompletionSource();
        var accepted = new TaskCompletionSource();
        var returned = false;
        var transport = new RecordingResponseTransport
        {
            Writing = async (_, _) => { entered.SetResult(); await accepted.Task; }
        };
        var builder = Builder(transport);
        await using var app = builder.Build();
        app.Run(async context =>
        {
            await context.Response.Body.WriteAsync(new byte[] { 7 });
            returned = true;
        });
        var request = new WispApplication(app).HandleAsync(Request());
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(10));
        Assert.True(!returned && !request.IsCompleted, "guest execution must await a blocked host write");
        accepted.SetResult();
        await request;
        Assert.True(returned, "acceptance must resume the pending write");
    }

    [Fact]
    public async Task explicit_completion_flushes_and_rejects_subsequent_writes_before_callbacks()
    {
        var transport = new RecordingResponseTransport();
        var callback = false;
        var builder = Builder(transport);
        await using var app = builder.Build();
        app.Run(async context =>
        {
            context.Response.OnCompleted(() => { callback = true; return Task.CompletedTask; });
            await context.Response.Body.WriteAsync(new byte[] { 9 });
            await context.Response.CompleteAsync();
            var error = await Record.ExceptionAsync(() => context.Response.Body.WriteAsync(new byte[] { 10 }).AsTask());
            Assert.True(error is InvalidOperationException && !callback,
                "CompleteAsync freezes writes without prematurely running completion callbacks");
        });
        await new WispApplication(app).HandleAsync(Request());
        Assert.True(callback && transport.Bytes.ToArray().SequenceEqual(new byte[] { 9 }),
            "completion callbacks run after the handler with the final accepted body");
    }

    [Fact]
    public async Task a_transport_failure_propagates_after_running_completion_callbacks()
    {
        var callback = false;
        var transport = new RecordingResponseTransport
        {
            Writing = (_, _) => ValueTask.FromException(new IOException("fixture transport closed"))
        };
        var builder = Builder(transport);
        await using var app = builder.Build();
        app.Run(async context =>
        {
            context.Response.OnCompleted(() => { callback = true; return Task.CompletedTask; });
            await context.Response.Body.WriteAsync(new byte[] { 1 });
        });
        var error = await Record.ExceptionAsync(() => new WispApplication(app).HandleAsync(Request()));
        Assert.True(error is IOException && callback, "a failed body write must propagate after completion callbacks run");
        Assert.True(transport.Started.Task.IsCompleted && transport.Bytes.Length == 0,
            "a post-header failure must not fabricate accepted response bytes");
    }

    [Fact]
    public async Task a_pre_cancelled_response_write_does_not_publish_headers()
    {
        var transport = new RecordingResponseTransport();
        using var response = new StreamingResponse(transport);
        var error = await Record.ExceptionAsync(() => response.Stream.WriteAsync(new byte[] { 1 }, new CancellationToken(true)).AsTask());
        Assert.True(error is OperationCanceledException && !transport.Started.Task.IsCompleted,
            "pre-cancelled writes must fail before publishing the response");
    }

    private static WebApplicationBuilder Builder(RecordingResponseTransport transport)
    {
        var builder = WispHosting.CreateBuilder(typeof(StreamingTests).Assembly.GetName().Name!);
        builder.Logging.ClearProviders();
        builder.Services.AddSingleton<Func<ResponseTransport>>(() => transport);
        return builder;
    }

    private static Request Request() => Wisp.Request.FromDispatch(new DispatchRecord
    {
        Kind = DispatchKind.Http, Method = "GET", Path = "/", Url = "/"
    });
}
