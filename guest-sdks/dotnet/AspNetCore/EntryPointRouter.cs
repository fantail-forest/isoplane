using Isoplane.Abi;

namespace Isoplane.AspNetCore;

/// <summary>
/// Routes every HTTP request to the ASP.NET application an entry point starts
/// by awaiting <c>RunAsync</c>. The entry point runs during initialization, at boot,
/// and stays suspended at that await for the life of each instance.
/// </summary>
internal sealed class EntryPointRouter : IStreamingHttpRouter
{
    private readonly Func<Task> entryPoint;
    private readonly ApplicationStartup startup;
    private Task? running;
    private Task<IsoplaneServer>? started;

    internal EntryPointRouter(Func<Task> entryPoint, string applicationName)
    {
        this.entryPoint = entryPoint;
        startup = new(applicationName);
    }

    // Initialization waits for the host, so hooks the entry point subscribes can use the started application.
    public void Boot()
    {
        running = startup.Invoke(entryPoint);
        Initialization.Starting += () => Started();
        Initialization.Completing += async () => await (await Started()).LoadDataProtectionKeysAsync();
    }

    public bool StreamsRequest(RequestHead request) => true;

    public async Task<Response> RouteStreamingAsync(StreamingRequest request) =>
        await (await Started()).SendAsync(request, request.Body, request.CanHaveBody);

    public async Task<Response> Route(DispatchRecord d) =>
        await (await Started()).SendAsync(Request.FromDispatch(d));

    private Task<IsoplaneServer> Started() => started ??= StartedAsync();

    private async Task<IsoplaneServer> StartedAsync()
    {
        if (await Task.WhenAny(startup.Started, running!) != startup.Started)
        {
            await running!;
            throw new InvalidOperationException("The application's entry point returned without awaiting RunAsync on an Isoplane application.");
        }
        var server = await startup.Started;
        server.PrimeEndpoints();
        return server;
    }
}
