using System.Diagnostics;
using System.Text.Json;
using Isoplane.Processes;

namespace Isoplane.Testing;

/// <summary>
/// Runs your application in the Isoplane sandbox for an integration test, and gives the test an
/// <c>HttpClient</c> to call it with.
/// </summary>
/// <remarks>
/// <para>
/// Use it to test the application as a client would, running as it runs under Isoplane. Test logic
/// that does not depend on the sandbox with ordinary unit tests, which are faster.
/// </para>
/// <para>
/// <see cref="StartAsync"/> runs the <c>isoplane</c> command on the test machine to build the
/// project and start it, so the command must be installed there, at the same version as this
/// package. The test itself runs outside the sandbox, in any test framework. Dispose the
/// application to stop it; <c>await using</c> does so.
/// </para>
/// <para>
/// Starting builds the project, so tests that can share one running application are faster when
/// they do, through a fixture of the test framework. Two applications running at the same time need
/// separate project directories.
/// <see href="https://isoplane.dev/docs/test-an-application">Test an application</see> sets up a test
/// project from scratch.
/// </para>
/// </remarks>
/// <example>
/// An xUnit test of the application in the sibling directory <c>MyApi</c>:
/// <code>
/// [Fact]
/// public async Task The_home_page_answers()
/// {
///     using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
///     var project = Path.GetFullPath("../../../../MyApi", AppContext.BaseDirectory);
///     await using var app = await IsoplaneApplication.StartAsync(project, cancellationToken: startup.Token);
///
///     using var response = await app.Client.GetAsync("/");
///
///     Assert.True(response.IsSuccessStatusCode, app.DiagnosticOutput);
/// }
/// </code>
/// </example>
public sealed class IsoplaneApplication : IAsyncDisposable
{
    private readonly OwnedProcess process;
    private readonly ApplicationOptions options;
    private readonly Diagnostics diagnostics;
    private readonly Task errors;
    private readonly CancellationTokenSource outputLifetime = new();
    private Task output = Task.CompletedTask;
    private readonly TaskCompletionSource<Uri> ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly object disposalGate = new();
    private Task? disposal;

    private IsoplaneApplication(OwnedProcess process, ApplicationOptions options)
    {
        this.process = process;
        this.options = options;
        diagnostics = new Diagnostics(options.DiagnosticLimitCharacters);
        errors = diagnostics.DrainAsync(process.StandardError, outputLifetime.Token);
        Client = new HttpClient();
    }

    /// <summary>An <c>HttpClient</c> for calling the running application.</summary>
    /// <remarks>
    /// Its <c>BaseAddress</c> is the application's address, plain HTTP on a local port chosen when it
    /// started, so request relative addresses: <c>app.Client.GetAsync("/todos")</c>. The client is
    /// disposed with the application; do not dispose it yourself.
    /// </remarks>
    public HttpClient Client { get; }

    /// <summary>
    /// What the <c>isoplane</c> command has printed while building and running the application, most
    /// recent last.
    /// </summary>
    /// <remarks>
    /// Pass it as the message of an assertion, so that a failing test shows a build error or a failure
    /// inside the application and not only an unexpected response. It holds at most
    /// <see cref="ApplicationOptions.DiagnosticLimitCharacters"/> characters, dropping the oldest, and
    /// can be read at any time, including after the application is disposed.
    /// </remarks>
    public string DiagnosticOutput => diagnostics.ToString();

    /// <summary>
    /// Builds the application and starts it in the sandbox, returning once it is ready to receive
    /// requests.
    /// </summary>
    /// <param name="projectDirectory">
    /// The directory holding the project file of the application under test, which must be the only
    /// project file there. A relative path is resolved against the current directory, which varies
    /// between test runners, so build an absolute one from <c>AppContext.BaseDirectory</c>.
    /// </param>
    /// <param name="options">How to run the application; null uses the defaults.</param>
    /// <param name="cancellationToken">
    /// Limits how long starting may take. Starting has no time limit of its own, so pass a token that
    /// times out. Cancelling it after the method has returned does nothing.
    /// </param>
    /// <returns>The running application. Dispose it to stop the application.</returns>
    /// <remarks>
    /// The application listens on a free local port chosen as it starts, so several can run on one
    /// machine. When the method throws there is nothing to dispose: it stops what it started first.
    /// </remarks>
    /// <exception cref="ArgumentException">The directory does not exist, or an option is not valid.</exception>
    /// <exception cref="InvalidOperationException">
    /// The application did not start: the <c>isoplane</c> command could not be run, the project did
    /// not build, or the application failed while starting. The message includes what the command
    /// printed.
    /// </exception>
    /// <exception cref="OperationCanceledException">The token was cancelled before the application was ready.</exception>
    /// <exception cref="PlatformNotSupportedException">The operating system is not one the package supports.</exception>
    /// <exception cref="AggregateException">The application did not start, and stopping what had been started also failed.</exception>
    public static async Task<IsoplaneApplication> StartAsync(string projectDirectory, ApplicationOptions? options = null, CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(projectDirectory);
        options ??= new ApplicationOptions();
        ArgumentException.ThrowIfNullOrWhiteSpace(options.CliPath);
        ArgumentException.ThrowIfNullOrWhiteSpace(options.Configuration);
        if (options.ShutdownTimeout <= TimeSpan.Zero || options.ShutdownTimeout.TotalMilliseconds > uint.MaxValue - 1)
            throw new ArgumentOutOfRangeException(nameof(options), "ShutdownTimeout must be a positive supported process-cleanup duration.");
        if (options.DiagnosticLimitCharacters <= 0)
            throw new ArgumentOutOfRangeException(nameof(options), "DiagnosticLimitCharacters must be positive.");
        var directory = Path.GetFullPath(projectDirectory);
        if (!Directory.Exists(directory))
            throw new ArgumentException($"Project directory does not exist: {directory}", nameof(projectDirectory));
        cancellationToken.ThrowIfCancellationRequested();
        var start = new ProcessStartInfo(options.CliPath)
        {
            WorkingDirectory = directory,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        foreach (var argument in new[] { "dev", "--project", directory, "--configuration", options.Configuration, "--port", "0", "--no-watch", "--json", "--shutdown-on-stdin-close" })
            start.ArgumentList.Add(argument);
        // The CLI reads application settings from its environment, which keeps secrets off its command line.
        foreach (var setting in options.Settings)
            start.Environment["ISOPLANE_APP_" + setting.Key.Replace(":", "__", StringComparison.Ordinal)] = setting.Value;
        OwnedProcess process;
        try
        {
            process = OwnedProcess.Start(start, options.ShutdownTimeout);
        }
        catch (IOException error)
        {
            throw new InvalidOperationException($"Could not start installed CLI '{options.CliPath}'; install Isoplane.Cli, or set ApplicationOptions.CliPath.", error);
        }
        var application = new IsoplaneApplication(process, options);
        try
        {
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

    /// <summary>Stops the application and releases its port and its <see cref="Client"/>.</summary>
    /// <remarks>
    /// <c>await using</c> calls it. A second call returns the result of the first. It throws when the application
    /// did not stop cleanly, so that a test does not pass over an application that crashed or hung.
    /// </remarks>
    /// <exception cref="TimeoutException">
    /// The application did not stop within <see cref="ApplicationOptions.ShutdownTimeout"/> and was
    /// stopped by force. The message includes <see cref="DiagnosticOutput"/>.
    /// </exception>
    /// <exception cref="InvalidOperationException">
    /// The <c>isoplane</c> command reported a failure as it stopped. The message includes
    /// <see cref="DiagnosticOutput"/>.
    /// </exception>
    /// <exception cref="AggregateException">More than one of these happened.</exception>
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
            await process.StopAsync(options.ShutdownTimeout + TimeSpan.FromSeconds(1));
            forced = process.ExitCode == 125 || process.ExitCode >= 128;
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
            if (process.HasExited && process.ExitCode != 0 && !forced)
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
