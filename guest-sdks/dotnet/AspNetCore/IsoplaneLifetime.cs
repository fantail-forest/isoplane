using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;

namespace Isoplane.AspNetCore;

/// <summary>
/// The host lifetime of a sandboxed application. The platform owns when an
/// instance runs and stops, so there are no process signals to wait for, and
/// work that would continue between dispatches is refused before anything starts.
/// </summary>
internal sealed class IsoplaneLifetime(IServiceProvider services) : IHostLifetime
{
    public Task WaitForStartAsync(CancellationToken cancellationToken)
    {
        if (services.GetServices<IHostedService>().Any(service => service is BackgroundService))
            throw new NotSupportedException("Isoplane ASP.NET does not support BackgroundService work between completed dispatches. Use hosted services for bounded startup work.");
        return Task.CompletedTask;
    }

    public Task StopAsync(CancellationToken cancellationToken) => Task.CompletedTask;
}
