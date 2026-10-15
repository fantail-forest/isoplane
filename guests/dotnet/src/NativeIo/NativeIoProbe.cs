using System;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Managed networking and timers over host-owned I/O, one probe per first path segment.</summary>
public static class NativeIoProbe
{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => ProbeRoute.Split(request.Path) switch
    {
        ("timer", var path) => TimerProbe.HandleAsync(path),
        ("dns", var path) => DnsProbe.HandleAsync(path),
        ("connect", var path) => ConnectProbe.HandleAsync(path),
        ("stream", var path) => StreamProbe.HandleAsync(path),
        ("refusal", var path) => NetworkRefusalProbe.HandleAsync(path),
        ("cleanup", var path) => CleanupProbe.HandleAsync(path),
        _ => throw new ArgumentException($"No native I/O probe serves {request.Path}."),
    };
}
