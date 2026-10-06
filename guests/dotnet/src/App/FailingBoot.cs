using System;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>A failed application initializer for the pre-initialization regression.</summary>
public static class FailingBoot
{
    /// <summary>Refuses to initialize the application.</summary>
    [Initializer]
    public static Task InitializeAsync() => throw new InvalidOperationException("fixture application initialization failed");

    /// <summary>Cannot be reached when initialization failed.</summary>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request, Ctx context) =>
        throw new InvalidOperationException("a failed application must never dispatch");
}
