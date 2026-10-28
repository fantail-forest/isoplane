using System.Text.Json.Nodes;
using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary>
/// A local application built from its project and running in the sandbox: what <c>isoplane dev</c>
/// owns for the terminal and the agent server owns for a coding agent. Session events, the ones
/// <see cref="LocalEvents"/> renders, go to one sink; with watching on, a source change rebuilds the
/// project and reloads the application, and a failed rebuild is reported as a <c>build_failed</c> event
/// while the last working application keeps serving.
/// </summary>
internal sealed class DevelopmentSession : IAsyncDisposable
{
    private readonly CancellationTokenSource lifetime;
    private readonly LocalSession host;
    private readonly SourceChanges? changes;
    private readonly CancellationTokenRegistration stopping;
    private readonly Task watching;

    private DevelopmentSession(CancellationTokenSource lifetime, LocalSession host, SourceChanges? changes, PackageBuild build, DevelopmentSettings settings, Action<JsonObject> events)
    {
        this.lifetime = lifetime;
        this.host = host;
        this.changes = changes;
        stopping = lifetime.Token.Register(host.Stop);
        watching = changes is null ? Task.CompletedTask : SourceReload.RunAsync(changes, host.Completion, async token =>
        {
            try
            {
                await build.BuildAsync(token);
                host.Reload(await build.ModuleAsync(token), build.SessionOptions(settings));
            }
            catch (InvalidOperationException error)
            {
                events(new JsonObject { ["event"] = "build_failed", ["detail"] = error.Message });
            }
        }, lifetime.Token);
    }

    /// <summary>Completes when the application stops, by <see cref="Stop"/> or on its own.</summary>
    internal Task Completion => host.Completion;

    /// <summary>
    /// Builds the project and starts it. A build that fails throws <see cref="InvalidOperationException"/>
    /// with the build's own message, and nothing is left running.
    /// </summary>
    internal static async Task<DevelopmentSession> StartAsync(DevelopmentOptions options, Action<JsonObject> events, CancellationToken cancellation)
    {
        var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        SourceChanges? changes = null;
        try
        {
            changes = options.Watch ? await SourceChanges.CreateAsync(options, lifetime.Token) : null;
            var build = new PackageBuild(options);
            await build.BuildAsync(lifetime.Token);
            var settings = await DevelopmentSettings.ResolveAsync(options, cancellation);
            var host = new LocalSession(await build.ModuleAsync(lifetime.Token), build.SessionOptions(settings), events);
            return new DevelopmentSession(lifetime, host, changes, build, settings, events);
        }
        catch
        {
            changes?.Dispose();
            lifetime.Dispose();
            throw;
        }
    }

    internal void Stop()
    {
        lifetime.Cancel();
        host.Stop();
    }

    public async ValueTask DisposeAsync()
    {
        Stop();
        try { await watching; }
        catch (OperationCanceledException) when (lifetime.IsCancellationRequested) { }
        await stopping.DisposeAsync();
        await host.DisposeAsync();
        changes?.Dispose();
        lifetime.Dispose();
    }

    /// <summary>An interpreted project, built as a package for the shared runtime its SDK carries.</summary>
    private sealed class PackageBuild(DevelopmentOptions options)
    {
        private string? runtime;

        /// <summary>Builds the package; a failure is an <see cref="InvalidOperationException"/> carrying the build's message.</summary>
        internal async Task BuildAsync(CancellationToken cancellation)
        {
            var package = new LocalProject(options.Directory).Package;
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(package)!);
                runtime = (await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), cancellation)).Module;
            }
            catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
            {
                throw new InvalidOperationException($"package build failed: {error.Message}", error);
            }
        }

        internal Task<byte[]> ModuleAsync(CancellationToken cancellation) =>
            File.ReadAllBytesAsync(runtime ?? throw new InvalidOperationException("The package has not been built."), cancellation);

        internal byte[] SessionOptions(DevelopmentSettings settings) => new LocalProject(options.Directory).SessionOptions(settings);
    }
}
