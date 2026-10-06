using System.Threading.Tasks;
using Isoplane.Abi;
using Isoplane;

namespace Isoplane.MonoRuntime;

/// <summary>
/// The runtime's router: at boot it loads the served package and hands every
/// dispatch to the router for the package's entry.
/// </summary>
internal sealed class PackageRouter : IStreamingHttpRouter
{
    private IIsoplaneRouter? application;

    public void Boot()
    {
        application = PackageLoader.Load();
        application.Boot();
    }

    public bool StreamsRequest(RequestHead request) =>
        application is IStreamingHttpRouter streaming && streaming.StreamsRequest(request);

    public Task<Response> RouteStreamingAsync(StreamingRequest request, Ctx ctx) =>
        ((IStreamingHttpRouter)application!).RouteStreamingAsync(request, ctx);

    public Task<Response> Route(DispatchRecord d, Ctx ctx) => application!.Route(d, ctx);
}
