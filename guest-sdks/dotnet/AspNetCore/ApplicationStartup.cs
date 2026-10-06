using Microsoft.Extensions.Hosting;

namespace Isoplane.AspNetCore;

/// <summary>
/// Reports the server an application's entry point starts to whoever invoked
/// that entry point. The invoker is ambient while the entry point runs, so an
/// application that builds its host there needs no handle to it.
/// </summary>
internal sealed class ApplicationStartup(string applicationName)
{
    private static readonly AsyncLocal<ApplicationStartup?> Ambient = new();
    private readonly TaskCompletionSource<IsoplaneServer> started = new(TaskCreationOptions.RunContinuationsAsynchronously);

    internal static ApplicationStartup? Current => Ambient.Value;

    /// <summary>The entry assembly's name, which ASP.NET searches for the application's own parts.</summary>
    internal string ApplicationName { get; } = applicationName;

    internal Task<IsoplaneServer> Started => started.Task;

    internal void Observe(IsoplaneServer server, IHostApplicationLifetime lifetime) =>
        lifetime.ApplicationStarted.Register(() => started.TrySetResult(server));

    /// <summary>Invokes <paramref name="entryPoint"/> with this startup ambient.</summary>
    internal Task Invoke(Func<Task> entryPoint)
    {
        Ambient.Value = this;
        try { return entryPoint(); }
        finally { Ambient.Value = null; }
    }
}
