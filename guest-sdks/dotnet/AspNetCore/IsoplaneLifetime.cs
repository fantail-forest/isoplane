using Microsoft.Extensions.Hosting;

namespace Isoplane.AspNetCore;

/// <summary>
/// The host lifetime of a sandboxed application. The platform owns when an
/// instance runs and stops, so there are no process signals to wait for.
/// </summary>
internal sealed class IsoplaneLifetime : IHostLifetime
{
    public Task WaitForStartAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task StopAsync(CancellationToken cancellationToken) => Task.CompletedTask;
}
