using Isoplane.Abi;
using Isoplane;
using Microsoft.AspNetCore.Builder;

namespace Isoplane.AspNetCore;

/// <summary>
/// Routes every HTTP request, body streamed, through the application a
/// factory builds once per wasm instance. This is the router the SDK
/// generates for an application factory returning <see cref="WebApplication"/>.
/// </summary>
public class WebApplicationRouter : IStreamingHttpRouter
{
    private readonly Func<WebApplication> factory;
    private IsoplaneApplication? application;

    /// <summary>Routes through the application <paramref name="factory"/> returns at boot.</summary>
    public WebApplicationRouter(Func<WebApplication> factory) => this.factory = factory;

    /// <summary>
    /// Builds the application and schedules host startup after user initialization hooks. Throws
    /// <see cref="InvalidOperationException"/> when the factory returns none or
    /// the application does not use the Isoplane server.
    /// </summary>
    public void Boot()
    {
        application = new(factory()
            ?? throw new InvalidOperationException("the application factory returned no WebApplication"));
        Initialization.Completing += application.StartAsync;
    }

    /// <summary>Streams every request body to the application.</summary>
    public bool StreamsRequest(RequestHead request) => true;

    /// <summary>Starts the application once, then handles the request as its body arrives.</summary>
    public Task<Response> RouteStreamingAsync(StreamingRequest request, Ctx ctx) =>
        application!.HandleStreamingAsync(request);

    /// <summary>Starts the application once, then handles the dispatch as a buffered request.</summary>
    public Task<Response> Route(DispatchRecord d, Ctx ctx) =>
        application!.HandleAsync(Request.FromDispatch(d));
}
