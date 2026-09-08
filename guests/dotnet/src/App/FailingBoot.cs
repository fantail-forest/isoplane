using System;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>A failed application factory for the pre-initialization regression.</summary>
public sealed class FailingBoot : IWispHandler
{
    /// <summary>Refuses to construct the application.</summary>
    public FailingBoot() => throw new InvalidOperationException("fixture application construction failed");

    /// <summary>Cannot be reached when construction failed.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        throw new InvalidOperationException("a failed application must never dispatch");
}
