using Microsoft.Extensions.Logging;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Wisp.Abi;
using Wisp.AspNetCore;
using Xunit;

namespace Wisp.AspNetCore.Tests;

public sealed class HostingTests
{
    private static WebApplicationBuilder Builder()
    {
        var builder = WispHosting.CreateBuilder(typeof(HostingTests).Assembly.GetName().Name!);
        builder.Logging.ClearProviders();
        builder.Services.AddSingleton<Func<ResponseTransport>>(() => new RecordingResponseTransport());
        return builder;
    }
    private static Request Request(string path = "/") => Wisp.Request.FromDispatch(new DispatchRecord
    {
        Kind = DispatchKind.Http, Method = "GET", Path = path, Url = path
    });

    [Fact]
    public async Task hosted_services_start_only_after_activation()
    {
        var events = new List<string>();
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new Service(events));
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        var runtime = new WispApplication(app);
        Assert.True(events.Count == 0, "building template state must not start hosted services");
        await runtime.HandleAsync(Request());
        await runtime.HandleAsync(Request());
        Assert.True(events.SequenceEqual(["start"]), "the host starts once across sequential requests");
        await app.StopAsync();
        Assert.True(events.SequenceEqual(["start", "stop"]), "the .NET host stops registered services");
    }

    [Fact]
    public async Task middleware_and_callbacks_observe_the_framework_order()
    {
        var events = new List<string>();
        var builder = Builder();
        await using var app = builder.Build();
        app.Use(async (context, next) =>
        {
            events.Add("before");
            context.Response.OnStarting(() => { events.Add("start-first"); return Task.CompletedTask; });
            context.Response.OnCompleted(() => { events.Add("complete-first"); return Task.CompletedTask; });
            await next(context);
            events.Add("after");
        });
        app.MapGet("/", async context =>
        {
            events.Add("handler");
            context.Response.OnStarting(() => { events.Add("start-last"); return Task.CompletedTask; });
            context.Response.OnCompleted(() => { events.Add("complete-last"); return Task.CompletedTask; });
            await context.Response.WriteAsync("ok");
        });
        await new WispApplication(app).HandleAsync(Request());
        Assert.True(events.SequenceEqual(["before", "handler", "start-last", "start-first", "after", "complete-last", "complete-first"]),
            "middleware wraps the handler; callbacks run in reverse registration order");
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task request_scopes_dispose_on_success_and_exception(bool throws)
    {
        var scopes = new List<Scope>();
        var builder = Builder();
        builder.Services.AddScoped(_ => { var scope = new Scope(); scopes.Add(scope); return scope; });
        await using var app = builder.Build();
        app.MapGet("/", context =>
        {
            context.RequestServices.GetRequiredService<Scope>();
            if (throws) throw new InvalidOperationException("fixture failure");
            return Task.CompletedTask;
        });
        var runtime = new WispApplication(app);
        for (var i = 0; i < 2; i++)
        {
            var error = await Record.ExceptionAsync(() => runtime.HandleAsync(Request()));
            Assert.True(throws ? error is InvalidOperationException : error is null,
                "the request pipeline propagates handler exceptions and succeeds otherwise");
        }
        Assert.True(scopes.Count == 2 && scopes.All(scope => scope.Disposed), "each request has a separate scope disposed even after an exception");
    }

    [Fact]
    public async Task abort_cancels_the_request_and_disposes_its_scope()
    {
        var scope = new Scope();
        var builder = Builder();
        builder.Services.AddScoped(_ => scope);
        await using var app = builder.Build();
        app.MapGet("/", context =>
        {
            context.RequestServices.GetRequiredService<Scope>();
            context.Abort();
            Assert.True(context.RequestAborted.IsCancellationRequested, "Abort cancels RequestAborted");
            return Task.CompletedTask;
        });
        var error = await Record.ExceptionAsync(() => new WispApplication(app).HandleAsync(Request()));
        Assert.True(error is OperationCanceledException, "aborted requests must end with cancellation");
        Assert.True(scope.Disposed, "aborted requests dispose their DI scope");
    }

    [Fact]
    public async Task response_preserves_status_bytes_and_repeated_headers()
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.MapGet("/", async context =>
        {
            context.Response.StatusCode = 201;
            context.Response.Headers.Append("Set-Cookie", "a=one");
            context.Response.Headers.Append("Set-Cookie", "b=two");
            await context.Response.Body.WriteAsync(new byte[] { 0, 255, 1 });
        });
        var response = await new WispApplication(app).HandleAsync(Request());
        Assert.True(response.Status == 201 && response.Body.SequenceEqual(new byte[] { 0, 255, 1 }), "the transport preserves status and binary bytes");
        Assert.True(response.Headers.Where(header => header.Key == "Set-Cookie").Select(header => header.Value).SequenceEqual(["a=one", "b=two"]), "Set-Cookie values remain separate");
    }

    [Fact]
    public async Task a_host_cancellation_wake_reaches_aspnet_and_disposes_the_scope()
    {
        Wisp.Runtime.FutureTable.Reset();
        Wisp.Runtime.FutureTable.BeginCancellation(42);
        var scope = new Scope();
        var builder = Builder();
        builder.Services.AddScoped(_ => scope);
        await using var app = builder.Build();
        app.MapGet("/", async context =>
        {
            context.RequestServices.GetRequiredService<Scope>();
            var cancelled = new TaskCompletionSource();
            using var registration = context.RequestAborted.Register(() => cancelled.SetResult());
            Wisp.Runtime.FutureTable.Complete(42, new ResultRecord { Error = FutureError.Cancelled });
            await cancelled.Task;
            context.RequestAborted.ThrowIfCancellationRequested();
        });
        try
        {
            var error = await Record.ExceptionAsync(() => new WispApplication(app).HandleAsync(Request()));
            Assert.True(error is OperationCanceledException, "host cancellation must end the request");
            Assert.True(scope.Disposed, "host cancellation must dispose the request scope");
        }
        finally { Wisp.Runtime.FutureTable.Reset(); }
    }

    [Fact]
    public async Task response_start_freezes_status_and_headers()
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.MapGet("/", async context =>
        {
            await context.Response.StartAsync();
            Assert.True(context.Response.HasStarted, "StartAsync marks the response as started even without body bytes");
            var status = Record.Exception(() => context.Response.StatusCode = 201);
            var header = Record.Exception(() => context.Response.Headers.Append("late", "value"));
            Assert.True(status is InvalidOperationException && header is InvalidOperationException,
                "started response status and headers must be immutable");
        });
        await new WispApplication(app).HandleAsync(Request());
    }

    [Fact]
    public async Task failed_completion_callbacks_do_not_prevent_scope_disposal()
    {
        var scope = new Scope();
        var builder = Builder();
        builder.Services.AddScoped(_ => scope);
        await using var app = builder.Build();
        app.MapGet("/", context =>
        {
            context.RequestServices.GetRequiredService<Scope>();
            context.Response.OnCompleted(() => throw new InvalidOperationException("completion failure"));
            throw new ApplicationException("handler failure");
        });
        var error = await Record.ExceptionAsync(() => new WispApplication(app).HandleAsync(Request()));
        Assert.True(scope.Disposed, "a failed completion callback must not suppress request scope disposal");
        Assert.True(error is AggregateException aggregate && aggregate.Flatten().InnerExceptions.Count == 2,
            "handler and completion failures must both be reported");
    }

    [Fact]
    public async Task unflushed_pipe_writes_are_completed_by_the_server()
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.MapGet("/", context =>
        {
            var writer = context.Response.BodyWriter;
            new byte[] { 1, 2, 3 }.CopyTo(writer.GetMemory(3));
            writer.Advance(3);
            return Task.CompletedTask;
        });
        var response = await new WispApplication(app).HandleAsync(Request());
        Assert.True(response.Body.SequenceEqual(new byte[] { 1, 2, 3 }),
            "server completion flushes pending response pipe bytes");
    }

    [Theory]
    [InlineData("file")]
    [InlineData("synchronous write")]
    public async Task unsupported_response_operations_fail_explicitly(string operation)
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.Run(async context =>
        {
            var body = context.Features.Get<IHttpResponseBodyFeature>()!;
            switch (operation)
            {
                case "file": await body.SendFileAsync("/file", 0, null); break;
                case "synchronous write": body.Stream.Write([1], 0, 1); break;
                default: throw new ArgumentOutOfRangeException(nameof(operation));
            }
        });
        var error = await Record.ExceptionAsync(() => new WispApplication(app).HandleAsync(Request()));
        Assert.True(error is NotSupportedException,
            $"unsupported {operation} must fail explicitly at the response boundary");
    }

    [Fact]
    public async Task background_services_are_rejected_before_they_start()
    {
        var background = new Background();
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(background);
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        var runtime = new WispApplication(app);
        var error = await Record.ExceptionAsync(() => runtime.HandleAsync(Request()));
        Assert.True(error is NotSupportedException, "ongoing background services require a supported host-owned lifetime");
        Assert.True(!background.Started, "unsupported background work must be rejected before execution");
    }

    private sealed class Background : BackgroundService
    {
        internal bool Started;
        protected override Task ExecuteAsync(CancellationToken stoppingToken)
        {
            Started = true;
            return Task.CompletedTask;
        }
    }

    [Fact]
    public async Task request_features_preserve_encoded_query_headers_and_body()
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.Run(async context =>
        {
            Assert.True(context.Request.Scheme == "http" && context.Request.Path == "/snow/☃",
                "root-relative host URLs retain HTTP scheme and ASP.NET decodes path components");
            Assert.True(context.Request.Query["tag"].SequenceEqual(["one", "two"]) && context.Request.Query["empty"] == "",
                "ASP.NET parses repeated and empty query values");
            Assert.True(context.Request.Headers["X-Input"].SequenceEqual(["first", "second"]),
                "repeated host request headers reach ASP.NET separately");
            using var body = new MemoryStream();
            await context.Request.Body.CopyToAsync(body);
            Assert.True(body.ToArray().SequenceEqual(new byte[] { 0, 255 }), "binary request bodies reach ASP.NET unchanged");
        });
        var request = Wisp.Request.FromDispatch(new DispatchRecord
        {
            Kind = DispatchKind.Http, Method = "POST", Path = "/snow/%E2%98%83",
            Url = "/snow/%E2%98%83?tag=one&tag=two&empty=",
            Headers = [new("X-Input", "first"), new("X-Input", "second")],
            Body = [0, 255]
        });
        await new WispApplication(app).HandleAsync(request);
    }

    [Fact]
    public async Task route_conventions_are_built_without_creating_request_scopes()
    {
        var scoped = false;
        var routes = false;
        var builder = Builder();
        builder.Services.AddScoped(_ => { scoped = true; return new Scope(); });
        await using var app = builder.Build();
        app.MapGet("/", (Scope scope) => "ok").Add(_ => routes = true);
        var runtime = new WispApplication(app);
        Assert.True(routes && !scoped, "pre-initialization materializes routing while request scopes remain uncreated");
        await runtime.HandleAsync(Request());
        Assert.True(scoped, "the request creates its scoped dependencies after activation");
    }

    private sealed class Scope : IAsyncDisposable
    {
        internal bool Disposed { get; private set; }
        public ValueTask DisposeAsync() { Disposed = true; return ValueTask.CompletedTask; }
    }
    private sealed class Service(List<string> events) : IHostedService
    {
        public async Task StartAsync(CancellationToken cancellationToken) { await Task.Yield(); events.Add("start"); }
        public Task StopAsync(CancellationToken cancellationToken) { events.Add("stop"); return Task.CompletedTask; }
    }
}
