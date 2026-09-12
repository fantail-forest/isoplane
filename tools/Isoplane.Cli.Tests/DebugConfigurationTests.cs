using Xunit;

namespace Wisp.Cli.Tests;

public sealed class DebugConfigurationTests
{
    [Fact]
    public async Task debugging_refuses_a_project_selected_nativeaot_configuration_before_publication()
    {
        var directory = Directory.CreateTempSubdirectory("wisp-debug-configuration-");
        try
        {
            await File.WriteAllTextAsync(Path.Combine(directory.FullName, "App.csproj"),
                "<Project><PropertyGroup><WispGuestRuntime>NativeAOT</WispGuestRuntime></PropertyGroup></Project>");
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            var options = new DevelopmentOptions(directory.FullName, null, false, false, false, null, true);
            var failure = await Assert.ThrowsAsync<ArgumentException>(() => DebugDevelopment.RunAsync(options,
                Path.Combine(AppContext.BaseDirectory, "runtime", "wisp"), deadline.Token));
            Assert.True(failure.Message.Contains("explicitly selected interpreted", StringComparison.Ordinal),
                "Debugging must explain that the project needs an interpreted configuration instead of silently substituting one.");
            Assert.False(Directory.Exists(Path.Combine(directory.FullName, ".wisp")),
                "A refused NativeAOT debug request must not create publication or session state.");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
