using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>
/// Handler behaviour seen through a node, one probe per first path segment.
/// Any other path reaches the request probe, whose subject includes targets
/// with no path segment at all.
/// </summary>
public static class NodeProbe
{
    [Initializer]
    public static Task InitializeAsync() => FilesystemProbe.InitializeAsync();

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => ProbeRoute.Split(request.Path) switch
    {
        ("filesystem", var path) => FilesystemProbe.HandleAsync(path),
        ("logs", var path) => LogProbe.HandleAsync(path),
        _ => RequestProbe.HandleAsync(request),
    };
}
