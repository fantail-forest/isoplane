using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;

namespace Isoplane.AspNetCore;

internal sealed class IsoplaneServer : IServer
{
    private readonly Func<ResponseTransport> createTransport;
    private readonly IServiceProvider services;
    private Func<IFeatureCollection, Task>? dispatch;

    public IsoplaneServer(Func<ResponseTransport> createTransport, IServiceProvider services, IHostApplicationLifetime lifetime, ApplicationStartup? startup = null)
    {
        this.createTransport = createTransport;
        this.services = services;
        startup?.Observe(this, lifetime);
    }

    internal bool Started => dispatch is not null;

    /// <summary>Builds the route table now, so a template captures it instead of each instance's first request.</summary>
    internal void PrimeEndpoints() => _ = services.GetService<Microsoft.AspNetCore.Routing.EndpointDataSource>()?.Endpoints;

    /// <summary>Loads the application's Data Protection keys if it keeps them in a store and has not loaded them itself.</summary>
    internal Task LoadDataProtectionKeysAsync() => services.GetService<LoadedKeyStore>()?.LoadAsync() ?? Task.CompletedTask;

    public IFeatureCollection Features { get; } = new FeatureCollection();

    public Task StartAsync<TContext>(IHttpApplication<TContext> application, CancellationToken cancellationToken)
        where TContext : notnull
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (dispatch is not null) throw new InvalidOperationException("The Isoplane server has already started.");
        dispatch = async features =>
        {
            var context = application.CreateContext(features);
            Exception? failure = null;
            var response = (StreamingResponse)features.Get<IHttpResponseFeature>()!;
            try
            {
                try
                {
                    await application.ProcessRequestAsync(context);
                    features.Get<IHttpRequestLifetimeFeature>()!.RequestAborted.ThrowIfCancellationRequested();
                    await features.Get<IHttpResponseBodyFeature>()!.CompleteAsync();
                }
                catch (Exception error) { failure = error; }
                try { await response.FinishCallbacksAsync(); }
                catch (Exception error)
                {
                    failure = failure is null ? error : new AggregateException(failure, error);
                }
            }
            finally { application.DisposeContext(context, failure); }
            if (failure is not null) System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(failure).Throw();
        };
        return Task.CompletedTask;
    }

    internal Task<Response> SendAsync(Request request) => SendAsync(request, createTransport());

    internal async Task<Response> SendAsync(Request request, ResponseTransport transport)
    {
        using var input = new MemoryStream(request.Body.ToArray(), writable: false);
        return await SendAsync(request, input, input.Length > 0, transport);
    }

    internal Task<Response> SendAsync(RequestHead request, Stream input, bool canHaveBody) =>
        SendAsync(request, input, canHaveBody, createTransport());

    private async Task<Response> SendAsync(RequestHead request, Stream input, bool canHaveBody, ResponseTransport transport)
    {
        var handler = dispatch ?? throw new InvalidOperationException("The Isoplane server has not started.");
        using var response = new StreamingResponse(transport);
        using var lifetime = new RequestLifetime(request.Aborted);
        var headers = new HeaderDictionary();
        foreach (var header in request.Headers) headers.Append(header.Key, header.Value);
        var target = request.Url;
        var queryStart = target.IndexOf('?');
        var scheme = Uri.TryCreate(target, UriKind.Absolute, out var absolute)
            && (absolute.Scheme == "http" || absolute.Scheme == "https") ? absolute.Scheme : "http";
        var features = new FeatureCollection();
        features.Set<IHttpRequestFeature>(new HttpRequestFeature
        {
            Method = request.Method,
            Path = PathString.FromUriComponent(request.Path).Value ?? "",
            PathBase = "",
            QueryString = queryStart < 0 ? "" : target[queryStart..],
            RawTarget = target,
            Scheme = scheme,
            Protocol = "HTTP/1.1",
            Body = input,
            Headers = headers
        });
        features.Set<IHttpRequestBodyDetectionFeature>(new RequestBody(canHaveBody));
        features.Set<IHttpRequestIdentifierFeature>(new RequestIdentifier());
        features.Set<IHttpRequestLifetimeFeature>(lifetime);
        features.Set<IHttpResponseFeature>(response);
        features.Set<IHttpResponseBodyFeature>(response);
        await handler(features);
        return response.ToResponse();
    }

    public Task StopAsync(CancellationToken cancellationToken)
    {
        dispatch = null;
        return Task.CompletedTask;
    }
    public void Dispose() => dispatch = null;

    private sealed record RequestBody(bool CanHaveBody) : IHttpRequestBodyDetectionFeature;

    // ASP.NET numbers requests from a static counter, which the template snapshot
    // copies once a warm-up request has started it: every instance would then hand
    // out the same identifiers. A request draws its own instead, and only if asked.
    private sealed class RequestIdentifier : IHttpRequestIdentifierFeature
    {
        private string? identifier;

        public string TraceIdentifier
        {
            get => identifier ??= Random.Shared.NextInt64().ToString("X16");
            set => identifier = value;
        }
    }

    private sealed class RequestLifetime(CancellationToken hostCancellation) : IHttpRequestLifetimeFeature, IDisposable
    {
        private readonly CancellationTokenSource cancellation = CancellationTokenSource.CreateLinkedTokenSource(hostCancellation);
        private CancellationToken? overridden;
        public CancellationToken RequestAborted
        {
            get => overridden ?? cancellation.Token;
            set => overridden = value;
        }
        public void Abort() => cancellation.Cancel();
        public void Dispose() => cancellation.Dispose();
    }
}
