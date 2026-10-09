using System.Diagnostics;
using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LocalSecretsTests
{
    [Fact]
    public async Task local_configuration_reads_the_project_store_created_by_dotnet_user_secrets()
    {
        var home = Directory.CreateTempSubdirectory("isoplane-secrets-test-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        var id = Guid.NewGuid().ToString("N");
        try
        {
            await RunAsync(home.FullName, deadline.Token, "user-secrets", "set", "ConnectionStrings:Todo", "test-only-value", "--id", id);
            Assert.Single(Directory.EnumerateFiles(home.FullName, "secrets.json", SearchOption.AllDirectories));
            var output = await RunAsync(home.FullName, deadline.Token, typeof(OwnedChild).Assembly.Location, "--read-local-secrets", id);
            using var values = JsonDocument.Parse(output);
            Assert.True(values.RootElement.GetProperty("ConnectionStrings:Todo").GetString() == "test-only-value",
                "The CLI must read ordinary .NET User Secrets without a second secret-store format.");
            var unrelated = await RunAsync(home.FullName, deadline.Token, typeof(OwnedChild).Assembly.Location, "--read-local-secrets", Guid.NewGuid().ToString("N"));
            using var other = JsonDocument.Parse(unrelated);
            Assert.True(!other.RootElement.EnumerateObject().Any(), "Another project's secret store must remain isolated.");
        }
        finally
        {
            home.Delete(recursive: true);
        }
    }

    private static async Task<string> RunAsync(string home, CancellationToken cancellation, params string[] arguments)
    {
        var start = new ProcessStartInfo("dotnet")
        {
            RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false
        };
        start.Environment["APPDATA"] = Path.Combine(home, "AppData");
        start.Environment["DOTNET_NOLOGO"] = "1";
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not run the local secrets test command.");
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
            Assert.True(process.ExitCode == 0, "The .NET secrets command must succeed: " + await errors);
            return await output;
        }
        finally
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                await process.WaitForExitAsync();
            }
        }
    }
}
