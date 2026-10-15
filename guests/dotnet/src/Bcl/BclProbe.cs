using System;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Base class library behaviour backed by the platform, one probe per first path segment.</summary>
public static class BclProbe
{
    [Initializer]
    public static async Task InitializeAsync()
    {
        await RandomnessProbe.InitializeAsync();
        await MemoryProbe.InitializeAsync();
        await TrustProbe.InitializeAsync();
    }

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => ProbeRoute.Split(request.Path) switch
    {
        ("certificates", var path) => CertificateProbe.HandleAsync(path),
        ("hashing", var path) => HashingProbe.HandleAsync(request, path),
        ("memory", _) => MemoryProbe.HandleAsync(),
        ("randomness", var path) => RandomnessProbe.HandleAsync(request, path),
        ("streaming", var path) => StreamingProbe.HandleAsync(path),
        ("trust", var path) => TrustProbe.HandleAsync(path),
        _ => throw new ArgumentException($"No base class library probe serves {request.Path}."),
    };
}
