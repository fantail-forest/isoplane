using Microsoft.Extensions.DependencyInjection;
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

/// <summary>
/// Refuses background work that outlives startup. The platform suspends an
/// instance between requests, so such work would run only while some request
/// happened to be in progress. A <see cref="BackgroundService"/> that has
/// finished by the time the host has started, or that a library registers
/// and never starts, is ordinary startup work.
/// </summary>
internal sealed class BackgroundWorkGuard(IServiceProvider services) : IHostedLifecycleService
{
    public Task StartedAsync(CancellationToken cancellationToken)
    {
        var running = services.GetServices<IHostedService>().OfType<BackgroundService>()
            .Where(service => service.ExecuteTask is { IsCompleted: false })
            .Select(service => service.GetType().FullName).ToArray();
        if (running.Length != 0)
            throw new NotSupportedException("Isoplane suspends an application between requests, so background work cannot continue after startup. " +
                $"Still running once the application had started: {string.Join(", ", running)}. " +
                "Do the work while handling a request, or in a hosted service whose StartAsync completes it.");
        return Task.CompletedTask;
    }

    public Task StartingAsync(CancellationToken cancellationToken) => Task.CompletedTask;
    public Task StartAsync(CancellationToken cancellationToken) => Task.CompletedTask;
    public Task StoppingAsync(CancellationToken cancellationToken) => Task.CompletedTask;
    public Task StopAsync(CancellationToken cancellationToken) => Task.CompletedTask;
    public Task StoppedAsync(CancellationToken cancellationToken) => Task.CompletedTask;
}
