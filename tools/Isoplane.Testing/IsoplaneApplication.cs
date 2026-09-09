using System.Diagnostics;
using System.Text.Json;

namespace Wisp.Testing;

/// <summary>Owns an installed CLI session and HTTP client that execute an app in the real WASM sandbox.</summary>
public sealed class IsoplaneApplication : IAsyncDisposable
{
    private readonly Process process;
    private readonly ApplicationOptions options;
    private readonly Diagnostics diagnostics;
    private readonly Task errors;
    private readonly CancellationTokenSource outputLifetime = new();
    private int? processGroup;
    private Task output = Task.CompletedTask;
    private readonly TaskCompletionSource<Uri> ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly object disposalGate = new();
    private Task? disposal;

    private IsoplaneApplication(Process process, ApplicationOptions options)
    {
        this.process = process;
        this.options = options;
        diagnostics = new Diagnostics(options.DiagnosticLimitCharacters);
        errors = diagnostics.DrainAsync(process.StandardError, outputLifetime.Token);
        Client = new HttpClient();
    }

    /// <summary>An owned HTTP client whose base address is the actual bound local listener.</summary>
    public HttpClient Client { get; }

    /// <summary>The most recent bounded CLI diagnostics, useful when an application assertion fails.</summary>
    public string DiagnosticOutput => diagnostics.ToString();

    /// <summary>Publishes and starts a project using the installed CLI, on an automatically assigned local port.</summary>
    /// <remarks>The project must contain one csproj and wisp.toml. Cancellation bounds startup and disposes owned children; it does not cancel an already returned session.</remarks>
    /// <exception cref="ArgumentException">The directory or options are invalid.</exception>
    /// <exception cref="InvalidOperationException">The CLI cannot start, exits before readiness, or emits an invalid readiness record.</exception>
    /// <exception cref="OperationCanceledException">Startup was cancelled.</exception>
    /// <exception cref="PlatformNotSupportedException">The platform cannot provide the validated Linux process ownership.</exception>
    /// <exception cref="AggregateException">Startup failed and owned process cleanup also reported failure.</exception>
    public static async Task<IsoplaneApplication> StartAsync(string projectDirectory, ApplicationOptions? options = null, CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(projectDirectory);
        options ??= new ApplicationOptions();
        ArgumentException.ThrowIfNullOrWhiteSpace(options.CliPath);
        if (options.ShutdownTimeout <= TimeSpan.Zero || options.ShutdownTimeout.TotalMilliseconds > uint.MaxValue - 1)
            throw new ArgumentOutOfRangeException(nameof(options), "ShutdownTimeout must be a positive supported process-cleanup duration.");
        if (options.DiagnosticLimitCharacters <= 0)
            throw new ArgumentOutOfRangeException(nameof(options), "DiagnosticLimitCharacters must be positive.");
        var directory = Path.GetFullPath(projectDirectory);
        if (!Directory.Exists(directory))
            throw new ArgumentException($"Project directory does not exist: {directory}", nameof(projectDirectory));
        cancellationToken.ThrowIfCancellationRequested();
        if (!OperatingSystem.IsLinux())
            throw new PlatformNotSupportedException("The packaged local runtime is validated on Linux; this fixture requires Linux process-group ownership (setsid and kill).");
        await ProcessOwnership.ValidateCleanupToolAsync("kill", options.ShutdownTimeout, cancellationToken);
        var start = new ProcessStartInfo("setsid")
        {
            WorkingDirectory = directory,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        foreach (var argument in new[] { "--wait", "sh", "-c", ProcessOwnership.Launch, "isoplane-test-session", options.CliPath, "dev", "--project", directory, "--port", "0", "--no-watch", "--json", "--shutdown-on-stdin-close" })
            start.ArgumentList.Add(argument);
        Process process;
        try
        {
            process = Process.Start(start) ?? throw new InvalidOperationException($"Could not start {options.CliPath}.");
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException($"Could not start installed CLI '{options.CliPath}'; install Isoplane.Cli and Linux setsid/kill, or set ApplicationOptions.CliPath.", error);
        }
        var application = new IsoplaneApplication(process, options);
        try
        {
            application.processGroup = await ProcessOwnership.VerifyAsync(process.StandardOutput, cancellationToken);
            await process.StandardInput.WriteLineAsync("start".AsMemory(), cancellationToken);
            await process.StandardInput.FlushAsync(cancellationToken);
            application.output = application.ReadOutputAsync(application.outputLifetime.Token);
            var exited = process.WaitForExitAsync(cancellationToken);
            if (await Task.WhenAny(application.ready.Task, exited).WaitAsync(cancellationToken) == exited)
            {
                await exited;
                throw new InvalidOperationException($"The CLI exited with code {process.ExitCode} before readiness.");
            }
            application.Client.BaseAddress = await application.ready.Task.WaitAsync(cancellationToken);
            return application;
        }
        catch (Exception failure)
        {
            if (application.output == Task.CompletedTask)
                application.output = application.diagnostics.DrainAsync(process.StandardOutput, application.outputLifetime.Token);
            try { await application.DisposeAsync(); }
            catch (Exception cleanup)
            {
                throw new AggregateException("Application startup and owned process cleanup both failed.", failure, cleanup);
            }
            if (failure is OperationCanceledException)
                throw;
            throw new InvalidOperationException($"The Isoplane application did not become ready: {failure.Message}\n{application.DiagnosticOutput}", failure);
        }
    }

    private async Task ReadOutputAsync(CancellationToken cancellation)
    {
        try
        {
            while (await process.StandardOutput.ReadLineAsync(cancellation) is { } line)
            {
                diagnostics.Append(line.AsSpan());
                diagnostics.Append("\n".AsSpan());
                using var record = JsonDocument.Parse(line);
                var root = record.RootElement;
                if (!root.TryGetProperty("event", out var kind) || kind.GetString() != "ready")
                    continue;
                var url = root.GetProperty("url").GetString();
                if (!Uri.TryCreate(url, UriKind.Absolute, out var address) || address.Scheme != Uri.UriSchemeHttp || !address.IsLoopback || address.Port == 0)
                    throw new InvalidOperationException("The CLI ready record did not identify a bound loopback HTTP listener.");
                ready.TrySetResult(address);
            }
            ready.TrySetException(new InvalidOperationException("The CLI closed its output before readiness."));
        }
        catch (Exception error)
        {
            if (!ready.TrySetException(error))
                throw;
        }
    }

    /// <summary>Closes the client, requests CLI cleanup through stdin EOF, and awaits all owned process output.</summary>
    /// <exception cref="TimeoutException">The CLI required forced termination.</exception>
    /// <exception cref="InvalidOperationException">The CLI exited unsuccessfully.</exception>
    /// <exception cref="AggregateException">More than one cleanup operation failed.</exception>
    public ValueTask DisposeAsync()
    {
        lock (disposalGate)
            return new ValueTask(disposal ??= StopAsync());
    }

    private async Task StopAsync()
    {
        Client.Dispose();
        List<Exception> failures = [];
        var timedOut = false;
        var forced = false;
        using var deadline = new CancellationTokenSource(options.ShutdownTimeout);
        try
        {
            process.StandardInput.Close();
            await process.WaitForExitAsync(deadline.Token);
            await Task.WhenAll(output, errors).WaitAsync(deadline.Token);
        }
        catch (OperationCanceledException) when (deadline.IsCancellationRequested)
        {
            timedOut = true;
        }
        catch (Exception error) { failures.Add(error); }
        try
        {
            if (processGroup is { } group)
                forced = await ProcessOwnership.TerminateAsync(group, options.ShutdownTimeout);
            else if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                forced = true;
            }
            using var cleanup = new CancellationTokenSource(options.ShutdownTimeout);
            await process.WaitForExitAsync(cleanup.Token);
        }
        catch (Exception error) { failures.Add(error); }
        finally
        {
            outputLifetime.Cancel();
            try { await Task.WhenAll(output, errors).WaitAsync(options.ShutdownTimeout); }
            catch (OperationCanceledException) when (outputLifetime.IsCancellationRequested)
            {
                diagnostics.Append("\nOutput collection ended during process cleanup.\n".AsSpan());
            }
            catch (Exception error)
            {
                if (!failures.Contains(error))
                    failures.Add(error);
            }
            if (timedOut || forced)
            {
                var reason = forced ? "Remaining owned CLI processes required termination." : "CLI cleanup exceeded its shutdown deadline.";
                failures.Add(new TimeoutException($"{reason}\n{DiagnosticOutput}"));
            }
            if (process.HasExited && process.ExitCode != 0)
                failures.Add(new InvalidOperationException($"The CLI exited with code {process.ExitCode}.\n{DiagnosticOutput}"));
            process.Dispose();
            outputLifetime.Dispose();
        }
        if (failures.Count == 1)
            System.Runtime.ExceptionServices.ExceptionDispatchInfo.Throw(failures[0]);
        if (failures.Count > 1)
            throw new AggregateException("Owned CLI cleanup failed.", failures);
    }
}
