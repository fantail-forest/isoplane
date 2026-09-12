using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using Wisp.Testing;
using Xunit;

namespace Wisp.Testing.Tests;

public sealed class ProcessLifetimeTests
{
    [Fact]
    public async Task open_stdin_allows_readiness_and_eof_releases_the_owned_session()
    {
        using var fixture = new SessionFixture();
        fixture.Script(fixture.Ready + "cat >/dev/null\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        await fixture.WithApplicationAsync(app =>
        {
            Assert.True(app.Client.BaseAddress == fixture.Address, "Readiness must expose the listener URL from the machine record.");
            return Task.CompletedTask;
        }, deadline.Token);
    }

    [Fact]
    public async Task a_descendant_holding_output_after_parent_exit_is_terminated_and_reported()
    {
        using var fixture = new SessionFixture();
        fixture.Script("sleep 300 &\necho $! > child.pid\n" + fixture.Ready + "cat >/dev/null\nexit 0\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var child = 0;
        var failure = await fixture.WithApplicationAsync(async app =>
        {
            child = int.Parse(await File.ReadAllTextAsync(Path.Combine(fixture.Directory, "child.pid"), deadline.Token));
        }, deadline.Token, typeof(TimeoutException));
        Assert.True(failure!.Message.Contains("termination", StringComparison.Ordinal), "Forced cleanup must be visible to the test consumer.");
        await AssertStoppedAsync(child, deadline.Token);
    }

    [Fact]
    public async Task a_parent_exiting_before_readiness_cannot_leave_descendants_or_hang_startup()
    {
        using var fixture = new SessionFixture();
        fixture.Script("sleep 300 &\necho $! > child.pid\nprintf 'application configuration failed\\n' >&2\nexit 23\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = await Assert.ThrowsAnyAsync<Exception>(() => IsoplaneApplication.StartAsync(fixture.Directory, fixture.Options, deadline.Token));
        Assert.True(failure.ToString().Contains("configuration failed", StringComparison.Ordinal), "Startup failure must retain actionable CLI diagnostics.");
        Assert.False(deadline.IsCancellationRequested, "Parent exit must end startup independently of its caller deadline.");
        var child = int.Parse(await File.ReadAllTextAsync(Path.Combine(fixture.Directory, "child.pid"), deadline.Token));
        await AssertStoppedAsync(child, deadline.Token);
    }

    [Fact]
    public async Task cancellation_during_startup_closes_the_owned_session()
    {
        using var fixture = new SessionFixture();
        fixture.Script("echo $$ > child.tmp\nmv child.tmp child.pid\ncat >/dev/null\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var cancellation = new CancellationTokenSource();
        var startup = IsoplaneApplication.StartAsync(fixture.Directory, fixture.Options, cancellation.Token);
        try
        {
            var pidPath = Path.Combine(fixture.Directory, "child.pid");
            while (!File.Exists(pidPath))
                await Task.Delay(10, deadline.Token);
            var child = int.Parse(await File.ReadAllTextAsync(pidPath, deadline.Token));
            cancellation.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => startup);
            await AssertStoppedAsync(child, deadline.Token);
        }
        finally
        {
            cancellation.Cancel();
            try
            {
                var app = await startup;
                await app.DisposeAsync();
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
                // Startup cancellation disposes its session even when the PID handshake assertion fails.
                Assert.True(startup.IsCompleted, "Cancelled startup must finish its owned cleanup.");
            }
        }
    }

    [Fact]
    public async Task a_nonzero_exit_during_graceful_cleanup_is_reported_with_bounded_diagnostics()
    {
        using var fixture = new SessionFixture();
        fixture.Script(fixture.Ready + "cat >/dev/null\nprintf '%0200d cleanup failed\\n' 0 >&2\nexit 17\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        IsoplaneApplication? observed = null;
        var failure = await fixture.WithApplicationAsync(app =>
        {
            observed = app;
            return Task.CompletedTask;
        }, deadline.Token, typeof(InvalidOperationException), fixture.Options with { DiagnosticLimitCharacters = 80 });
        Assert.True(failure!.Message.Contains("17", StringComparison.Ordinal), "Cleanup failure must retain the CLI exit code.");
        Assert.True(observed!.DiagnosticOutput.Length <= 80, "The diagnostic buffer must respect the consumer's configured bound.");
        Assert.True(observed.DiagnosticOutput.Contains("cleanup failed", StringComparison.Ordinal), "The diagnostic buffer must retain the newest error.");
    }

    [Theory]
    [InlineData("{not-json}")]
    [InlineData("{\"event\":\"ready\",\"url\":\"https://127.0.0.1:443\"}")]
    [InlineData("{\"event\":\"ready\",\"url\":\"http://127.0.0.1:0\"}")]
    [InlineData("{\"event\":\"ready\",\"url\":\"http://example.com:80\"}")]
    public async Task invalid_machine_readiness_is_refused_and_its_process_is_closed(string record)
    {
        using var fixture = new SessionFixture();
        fixture.Script("echo $$ > child.pid\nprintf '%s\\n' '" + record + "'\ncat >/dev/null\n");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = await Assert.ThrowsAsync<InvalidOperationException>(() => IsoplaneApplication.StartAsync(fixture.Directory, fixture.Options, deadline.Token));
        Assert.True(failure.Message.Contains(record, StringComparison.Ordinal), "Refused readiness must retain the offending record in bounded diagnostics.");
        var child = int.Parse(await File.ReadAllTextAsync(Path.Combine(fixture.Directory, "child.pid"), deadline.Token));
        await AssertStoppedAsync(child, deadline.Token);
    }

    [Theory]
    [InlineData(0, 1)]
    [InlineData(-1, 1)]
    [InlineData(1, 0)]
    public async Task invalid_cleanup_configuration_is_rejected_before_starting_processes(int timeoutMilliseconds, int diagnosticCharacters)
    {
        using var fixture = new SessionFixture();
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => IsoplaneApplication.StartAsync(fixture.Directory,
            fixture.Options with { ShutdownTimeout = TimeSpan.FromMilliseconds(timeoutMilliseconds), DiagnosticLimitCharacters = diagnosticCharacters }));
    }

    [Fact]
    public async Task a_missing_cli_is_reported_before_session_acquisition()
    {
        using var fixture = new SessionFixture();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = await Assert.ThrowsAsync<InvalidOperationException>(() => ProcessOwnership.ResolveAsync(
            Path.Combine(fixture.Directory, "absent-kill"), fixture.Options.ShutdownTimeout, deadline.Token));
        Assert.True(failure.Message.Contains("install Isoplane.Cli", StringComparison.Ordinal), "The preflight must explain which cleanup tool is missing.");
    }

    private static async Task AssertStoppedAsync(int pid, CancellationToken cancellation)
    {
        var statPath = $"/proc/{pid}/stat";
        while (File.Exists(statPath))
        {
            string stat;
            try { stat = await File.ReadAllTextAsync(statPath, cancellation); }
            catch (FileNotFoundException) { return; }
            if (stat[(stat.LastIndexOf(')') + 2)..].StartsWith("Z ", StringComparison.Ordinal))
                return;
            await Task.Delay(10, cancellation);
        }
    }

    private sealed class SessionFixture : IDisposable
    {
        private readonly TcpListener listener = new(IPAddress.Loopback, 0);

        internal SessionFixture()
        {
            Directory = System.IO.Directory.CreateTempSubdirectory("isoplane fixture '").FullName;
            listener.Start();
            Address = new Uri($"http://127.0.0.1:{((IPEndPoint)listener.LocalEndpoint).Port}/");
        }

        internal string Directory { get; }
        internal Uri Address { get; }
        internal ApplicationOptions Options => new() { CliPath = Path.Combine(Directory, "test cli"), ShutdownTimeout = TimeSpan.FromSeconds(1) };
        internal string Ready => "printf '%s\\n' '" + JsonSerializer.Serialize(new { @event = "ready", url = Address.ToString() }) + "'\n";

        internal void Script(string body)
        {
            if (!OperatingSystem.IsLinux())
                throw new PlatformNotSupportedException("Process ownership tests require the packaged runtime's Linux platform.");
            var helper = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp-process");
            File.WriteAllText(Options.CliPath, "#!/bin/sh\nif [ \"$1\" = process-owner-path ]; then printf '%s\\n' '" + helper.Replace("'", "'\\''") + "'; exit 0; fi\n" + body);
            File.SetUnixFileMode(Options.CliPath, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        }

        internal async Task<Exception?> WithApplicationAsync(Func<IsoplaneApplication, Task> check, CancellationToken cancellation, Type? expectedCleanupFailure = null, ApplicationOptions? options = null)
        {
            var app = await IsoplaneApplication.StartAsync(Directory, options ?? Options, cancellation);
            Exception? assertion = null;
            Exception? cleanup = null;
            try { await check(app); }
            catch (Exception error) { assertion = error; }
            try
            {
                await app.DisposeAsync();
                await app.DisposeAsync();
            }
            catch (Exception error) { cleanup = error; }
            if (assertion is not null)
                throw new AggregateException("Session assertion failed; owned cleanup was attempted.", cleanup is null ? [assertion] : [assertion, cleanup]);
            Assert.True(cleanup?.GetType() == expectedCleanupFailure, $"Cleanup must report the expected outcome; received: {cleanup}");
            return cleanup;
        }

        public void Dispose()
        {
            listener.Stop();
            System.IO.Directory.Delete(Directory, recursive: true);
        }
    }
}
