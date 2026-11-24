using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

public static class NodeProbe
{
    [TemplateInitializer]
    public static Task InitializeAsync() => FilesystemProbe.InitializeAsync();

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => ProbeRoute.Split(request.Path) switch
    {
        ("filesystem", var path) => FilesystemProbe.HandleAsync(path),
        ("logs", var path) => LogProbe.HandleAsync(path),
        ("departed", var path) => DepartureProbe.HandleAsync(path, request),
        ("departure", _) => DepartureProbe.ReportAsync(),
        _ => RequestProbe.HandleAsync(request),
    };
}
