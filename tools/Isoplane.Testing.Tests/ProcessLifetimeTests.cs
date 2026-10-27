using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using Isoplane.Testing;
using Xunit;

namespace Isoplane.Testing.Tests;

public sealed class ProcessLifetimeTests
{
    [Fact]
    public void the_cli_is_told_that_a_test_started_its_session()
    {
        var start = IsoplaneApplication.StartInfo(Path.GetTempPath(), new ApplicationOptions());
        Assert.Equal("1", start.Environment[Isoplane.Cli.TelemetryVariables.TestRun.Name]);
    }

    [Fact]
    public void a_tests_settings_reach_the_cli_as_one_settings_argument()
    {
        var options = new ApplicationOptions { Settings = new Dictionary<string, string> { ["ConnectionStrings:Todos"] = "Host=test" } };
        var start = IsoplaneApplication.StartInfo(Path.GetTempPath(), options);
        var index = start.ArgumentList.IndexOf("--settings");
        Assert.True(index >= 0, "The settings must travel as the --settings argument.");
        Assert.Equal("""{"ConnectionStrings:Todos":"Host=test"}""", start.ArgumentList[index + 1]);
        Assert.DoesNotContain("--settings", IsoplaneApplication.StartInfo(Path.GetTempPath(), new ApplicationOptions()).ArgumentList);
    }

    [Fact]
    public async Task open_stdin_allows_readiness_and_eof_releases_the_owned_session()
    {
        using var fixture = new SessionFixture();
        fixture.Configure("ready");
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
        fixture.Configure("orphan-after-eof");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var child = 0;
        await fixture.WithApplicationAsync(async app =>
        {
            child = int.Parse(await File.ReadAllTextAsync(Path.Combine(fixture.Directory, "child.pid"), deadline.Token));
        }, deadline.Token, typeof(TimeoutException), fixture.Options with { ShutdownTimeout = TimeSpan.FromSeconds(1) });
        await AssertStoppedAsync(child, deadline.Token);
    }

    [Fact]
    public async Task a_parent_exiting_before_readiness_cannot_leave_descendants_or_hang_startup()
    {
        using var fixture = new SessionFixture();
        fixture.Configure("startup-failure");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = await Assert.ThrowsAnyAsync<Exception>(() => IsoplaneApplication.StartAsync(fixture.Directory,
            fixture.Options with { ShutdownTimeout = TimeSpan.FromSeconds(1) }, deadline.Token));
        Assert.True(failure.ToString().Contains("configuration failed", StringComparison.Ordinal), "Startup failure must retain actionable CLI diagnostics.");
        Assert.False(deadline.IsCancellationRequested, "Parent exit must end startup independently of its caller deadline.");
        var child = int.Parse(await File.ReadAllTextAsync(Path.Combine(fixture.Directory, "child.pid"), deadline.Token));
        await AssertStoppedAsync(child, deadline.Token);
    }

    [Fact]
    public async Task cancellation_during_startup_closes_the_owned_session()
    {
        using var fixture = new SessionFixture();
        fixture.Configure("startup-cancel");
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
        fixture.Configure("cleanup-failure");
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
        fixture.Configure("invalid-readiness", record);
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
        var failure = await Assert.ThrowsAsync<InvalidOperationException>(() => IsoplaneApplication.StartAsync(fixture.Directory,
            fixture.Options with { CliPath = Path.Combine(fixture.Directory, "absent-cli") }, deadline.Token));
        Assert.True(failure.Message.Contains("install Isoplane.Cli", StringComparison.Ordinal), "Startup must identify the missing CLI and explain how to install it.");
    }

    private static async Task AssertStoppedAsync(int pid, CancellationToken cancellation)
    {
        while (await IsRunningAsync(pid, cancellation))
            await Task.Delay(10, cancellation);
    }

    private static async Task<bool> IsRunningAsync(int pid, CancellationToken cancellation)
    {
        if (OperatingSystem.IsWindows())
        {
            try
            {
                using var process = Process.GetProcessById(pid);
                return !process.HasExited;
            }
            catch (ArgumentException) { return false; }
        }
        var start = new ProcessStartInfo("ps")
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        foreach (var argument in new[] { "-o", "stat=", "-p", pid.ToString() })
            start.ArgumentList.Add(argument);
        using var query = Process.Start(start) ?? throw new InvalidOperationException("Could not inspect fixture process.");
        var output = query.StandardOutput.ReadToEndAsync(cancellation);
        var error = query.StandardError.ReadToEndAsync(cancellation);
        await query.WaitForExitAsync(cancellation);
        Assert.True(query.ExitCode is 0 or 1, "Process inspection failed: " + await error);
        return query.ExitCode == 0 && !(await output).TrimStart().StartsWith("Z", StringComparison.Ordinal);
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
        internal ApplicationOptions Options => new()
        {
            CliPath = Environment.GetEnvironmentVariable("ISOPLANE_TEST_PROCESS_FIXTURE")
                ?? throw new InvalidOperationException("Run task test:managed to prepare the process fixture.")
        };

        internal void Configure(string scenario, string? readiness = null)
        {
            Assert.True(File.Exists(Options.CliPath), "Run task test:managed to prepare the process fixture.");
            File.WriteAllText(Path.Combine(Directory, "process-fixture.json"), JsonSerializer.Serialize(new
            {
                scenario,
                readiness = readiness ?? JsonSerializer.Serialize(new { @event = "ready", url = Address.ToString() })
            }));
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
