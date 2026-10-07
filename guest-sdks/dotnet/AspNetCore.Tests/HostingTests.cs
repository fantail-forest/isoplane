using Microsoft.Extensions.Logging;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Isoplane.Abi;
using Isoplane.AspNetCore;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class HostingTests
{
    private static WebApplicationBuilder Builder()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(HostingTests).Assembly.GetName().Name!, new Microsoft.Extensions.Configuration.Memory.MemoryConfigurationSource());
        builder.Logging.ClearProviders();
        builder.Services.AddSingleton<Func<ResponseTransport>>(() => new RecordingResponseTransport());
        return builder;
    }
    private static DispatchRecord Dispatch(string path = "/") => new()
    {
        Kind = DispatchKind.Http, Method = "GET", Path = path.Split('?')[0], Url = path
    };
    private static Request Request(string path = "/") => Isoplane.Request.FromDispatch(Dispatch(path));

    [Fact]
    public async Task hosted_services_start_once_when_requests_begin()
    {
        var events = new List<string>();
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new Service(events));
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        Assert.True(events.Count == 0, "building the application must not start hosted services");
        await app.HandleAsync(Request());
        await app.HandleAsync(Request());
        Assert.True(events.SequenceEqual(["start"]), "the host starts once across sequential requests");
        await app.StopAsync();
        Assert.True(events.SequenceEqual(["start", "stop"]), "the .NET host stops registered services");
    }

    [Fact]
    public async Task a_request_waits_for_the_host_its_entry_point_is_starting()
    {
        var startupEntered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var startupRelease = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var events = new List<string>();
        WebApplication? application = null;
        var router = new EntryPointRouter(async () =>
        {
            var builder = Builder();
            builder.Services.AddSingleton<IHostedService>(new StartupService(async () =>
            {
                startupEntered.SetResult();
                await startupRelease.Task;
                events.Add("host");
            }));
            application = builder.Build();
            application.MapGet("/", () => { events.Add("request"); return "ready"; });
            await application.RunAsync();
        }, "Application");
        router.Boot();
        try
        {
            var first = router.Route(Dispatch(), null!);
            await startupEntered.Task.WaitAsync(TimeSpan.FromSeconds(30));
            Assert.True(events.Count == 0 && !first.IsCompleted, "a request must wait for hosted services to start");
            startupRelease.SetResult();
            Assert.Equal("ready", System.Text.Encoding.UTF8.GetString((await first).Body));
            await router.Route(Dispatch(), null!);
            Assert.Equal(["host", "request", "request"], events);
        }
        finally
        {
            startupRelease.TrySetResult();
            if (application is not null) await application.DisposeAsync();
        }
    }

    [Fact]
    public async Task an_entry_point_that_returns_without_running_an_application_is_refused()
    {
        var router = new EntryPointRouter(() => Task.CompletedTask, "Application");
        router.Boot();
        var error = await Record.ExceptionAsync(() => router.Route(Dispatch(), null!));
        Assert.True(error is InvalidOperationException && error.Message.Contains("RunAsync", StringComparison.Ordinal),
            "an entry point that starts no application must be refused with the missing call named: " + error);
    }

    [Fact]
    public async Task a_failed_host_startup_fails_every_request_without_restarting_services()
    {
        int starts = 0;
        var router = new EntryPointRouter(async () =>
        {
            var builder = Builder();
            builder.Services.AddSingleton<IHostedService>(new StartupService(() =>
            {
                starts++;
                throw new InvalidOperationException("startup failed");
            }));
            await using var application = builder.Build();
            await application.RunAsync();
        }, "Application");
        router.Boot();
        var failure = await Record.ExceptionAsync(() => router.Route(Dispatch(), null!));
        var repeated = await Record.ExceptionAsync(() => router.Route(Dispatch(), null!));
        Assert.True(failure is InvalidOperationException { Message: "startup failed" } && ReferenceEquals(failure, repeated) && starts == 1,
            "a startup failure must reach every request without rerunning hosted services");
    }

    private sealed class StartupService(Func<Task> start) : IHostedService
    {
        public Task StartAsync(CancellationToken cancellationToken) => start();
        public Task StopAsync(CancellationToken cancellationToken) => Task.CompletedTask;
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
        await app.HandleAsync(Request());
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
        for (var i = 0; i < 2; i++)
        {
            var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
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
        var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
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
        var response = await app.HandleAsync(Request());
        Assert.True(response.Status == 201 && response.Body.SequenceEqual(new byte[] { 0, 255, 1 }), "the transport preserves status and binary bytes");
        Assert.True(response.Headers.Where(header => header.Key == "Set-Cookie").Select(header => header.Value).SequenceEqual(["a=one", "b=two"]), "Set-Cookie values remain separate");
    }

    [Fact]
    public async Task a_host_cancellation_wake_reaches_aspnet_and_disposes_the_scope()
    {
        Isoplane.Runtime.FutureTable.Reset();
        Isoplane.Runtime.FutureTable.BeginCancellation(42);
        var scope = new Scope();
        var builder = Builder();
        builder.Services.AddScoped(_ => scope);
        await using var app = builder.Build();
        app.MapGet("/", async context =>
        {
            context.RequestServices.GetRequiredService<Scope>();
            var cancelled = new TaskCompletionSource();
            using var registration = context.RequestAborted.Register(() => cancelled.SetResult());
            Isoplane.Runtime.FutureTable.Complete(42, new ResultRecord { Error = FutureError.Cancelled });
            await cancelled.Task;
            context.RequestAborted.ThrowIfCancellationRequested();
        });
        try
        {
            var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
            Assert.True(error is OperationCanceledException, "host cancellation must end the request");
            Assert.True(scope.Disposed, "host cancellation must dispose the request scope");
        }
        finally { Isoplane.Runtime.FutureTable.Reset(); }
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
        await app.HandleAsync(Request());
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
        var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
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
        var response = await app.HandleAsync(Request());
        Assert.True(response.Body.SequenceEqual(new byte[] { 1, 2, 3 }),
            "server completion flushes pending response pipe bytes");
    }

    [Fact]
    public async Task a_synchronous_response_write_fails_explicitly()
    {
        var builder = Builder();
        await using var app = builder.Build();
        app.Run(context =>
        {
            context.Features.Get<IHttpResponseBodyFeature>()!.Stream.Write([1], 0, 1);
            return Task.CompletedTask;
        });
        var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
        Assert.True(error is NotSupportedException, "a synchronous write must fail explicitly at the response boundary");
    }

    [Fact]
    public async Task a_file_is_sent_as_the_requested_part_of_the_response_body()
    {
        var file = Path.Combine(Directory.CreateTempSubdirectory("isoplane-send-file-").FullName, "asset.txt");
        await File.WriteAllTextAsync(file, "0123456789");
        try
        {
            var builder = Builder();
            await using var app = builder.Build();
            app.Run(context => context.Features.Get<IHttpResponseBodyFeature>()!.SendFileAsync(file, 2, 5));
            var response = await app.HandleAsync(Request());
            Assert.Equal("23456", System.Text.Encoding.UTF8.GetString(response.Body));
        }
        finally { Directory.Delete(Path.GetDirectoryName(file)!, recursive: true); }
    }

    [Fact]
    public async Task background_work_that_outlives_startup_is_refused_by_name()
    {
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new Background(stopping => Task.Delay(Timeout.Infinite, stopping)));
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        var error = await Record.ExceptionAsync(() => app.HandleAsync(Request()));
        Assert.True(error is NotSupportedException && error.Message.Contains(typeof(Background).FullName!, StringComparison.Ordinal),
            "work still running after startup must be refused, naming the service: " + error);
    }

    [Fact]
    public async Task a_background_service_that_finishes_during_startup_is_startup_work()
    {
        var ran = false;
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new Background(_ => { ran = true; return Task.CompletedTask; }));
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        var response = await app.HandleAsync(Request());
        Assert.True(ran && response.Status == 200, "a background service with nothing left to do must not stop the application");
    }

    [Fact]
    public async Task a_background_service_its_library_never_starts_is_accepted()
    {
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new NeverStarted());
        await using var app = builder.Build();
        app.MapGet("/", () => "ready");
        var response = await app.HandleAsync(Request());
        Assert.Equal(200, response.Status);
    }

    private sealed class Background(Func<CancellationToken, Task> work) : BackgroundService
    {
        protected override Task ExecuteAsync(CancellationToken stoppingToken) => work(stoppingToken);
    }

    /// <summary>What a library does with a service it registers unconditionally and needs only in some modes.</summary>
    private sealed class NeverStarted : BackgroundService
    {
        public override Task StartAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        protected override Task ExecuteAsync(CancellationToken stoppingToken) => Task.Delay(Timeout.Infinite, stoppingToken);
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
        var request = Isoplane.Request.FromDispatch(new DispatchRecord
        {
            Kind = DispatchKind.Http, Method = "POST", Path = "/snow/%E2%98%83",
            Url = "/snow/%E2%98%83?tag=one&tag=two&empty=",
            Headers = [new("X-Input", "first"), new("X-Input", "second")],
            Body = [0, 255]
        });
        await app.HandleAsync(request);
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
        (await app.StartedAsync()).PrimeEndpoints();
        Assert.True(routes && !scoped, "pre-initialization materializes routing while request scopes remain uncreated");
        await app.HandleAsync(Request());
        Assert.True(scoped, "the request creates its scoped dependencies after activation");
    }

    [Fact]
    public async Task warmup_runs_the_pipeline_and_reuses_host_startup_for_real_requests()
    {
        var events = new List<string>();
        var scopes = new List<Scope>();
        var transports = 0;
        var builder = Builder();
        builder.Services.AddSingleton<IHostedService>(new Service(events));
        builder.Services.AddScoped(_ => { var scope = new Scope(); scopes.Add(scope); return scope; });
        builder.Services.AddSingleton<Func<ResponseTransport>>(() => { transports++; return new RecordingResponseTransport(); });
        await using var app = builder.Build();
        app.Use(async (context, next) =>
        {
            events.Add("middleware");
            context.Response.OnCompleted(() => { events.Add("completed"); return Task.CompletedTask; });
            await next(context);
        });
        app.MapGet("/todos", (Scope scope, int page) => new { page });
        await app.StartedAsync();
        await app.WarmupAsync("/todos?page=1");
        Assert.True(transports == 0, "warmup must not create a host response stream");
        Assert.True(scopes.Count == 1 && scopes[0].Disposed, "warmup must dispose its request scope before capture");
        Assert.True(events.SequenceEqual(["start", "middleware", "completed"]), "warmup must run middleware and completion callbacks on the started host");
        await app.HandleAsync(Request("/todos?page=2"));
        Assert.True(transports == 1 && events.Count(e => e == "start") == 1,
            "a request after warmup must reuse the started host and send its response to the host");
    }

    [Fact]
    public async Task the_data_protection_key_exists_once_the_host_has_started()
    {
        var keys = Directory.CreateTempSubdirectory("isoplane-data-protection-");
        try
        {
            var builder = Builder();
            builder.Services.AddDataProtection().PersistKeysToFileSystem(keys);
            await using var app = builder.Build();
            await app.StartedAsync();
            // Starting the host is part of initialization, so the key is in the template
            // and every instance of a deployment protects and unprotects with it.
            Assert.True(keys.EnumerateFiles("key-*.xml").Any(), "the key must be created when the host starts, not by an instance's first use");
        }
        finally { keys.Delete(recursive: true); }
    }

    [Fact]
    public async Task a_warmup_request_names_a_host()
    {
        string? host = null;
        await using var app = Builder().Build();
        app.MapGet("/", (HttpContext context) =>
        {
            host = context.Request.Host.Value;
            return new Uri($"{context.Request.Scheme}://{context.Request.Host}{context.Request.Path}").AbsoluteUri;
        });
        await app.StartedAsync();
        await app.WarmupAsync("/");
        Assert.Equal("localhost", host);
    }

    [Theory]
    [InlineData(404)]
    [InlineData(500)]
    public async Task warmup_rejects_unsuccessful_responses_after_disposing_the_scope(int status)
    {
        var scope = new Scope();
        var builder = Builder();
        builder.Services.AddScoped(_ => scope);
        await using var app = builder.Build();
        app.MapGet("/", (Scope _) => Results.StatusCode(status));
        await app.StartedAsync();
        var error = await Record.ExceptionAsync(() => app.WarmupAsync("/"));
        Assert.True(error is HttpRequestException { StatusCode: var code } && (int?)code == status,
            "warmup must report the endpoint's unsuccessful HTTP status");
        Assert.True(scope.Disposed, "failed warmup responses must still dispose scoped services");
    }

    [Theory]
    [InlineData("")]
    [InlineData("https://example.com/todos")]
    [InlineData("//example.com/todos")]
    public async Task warmup_requires_an_application_relative_path(string path)
    {
        await using var app = Builder().Build();
        await app.StartedAsync();
        var error = await Record.ExceptionAsync(() => app.WarmupAsync(path));
        Assert.True(error is ArgumentException, "warmup must refuse paths that name an external host");
    }

    [Fact]
    public async Task warmup_refuses_an_application_that_has_not_started()
    {
        await using var app = Builder().Build();
        app.MapGet("/", () => "ready");
        var error = await Record.ExceptionAsync(() => app.WarmupAsync("/"));
        Assert.True(error is InvalidOperationException && error.Message.Contains("RunAsync", StringComparison.Ordinal),
            "warmup before the host starts must say when it can run: " + error);
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
