using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class DebugConfigurationTests
{
    [Fact]
    public async Task debugging_refuses_to_override_a_project_selected_nativeaot_configuration()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-debug-configuration-");
        try
        {
            await File.WriteAllTextAsync(Path.Combine(directory.FullName, "App.csproj"),
                "<Project><PropertyGroup><IsoplaneGuestRuntime>NativeAOT</IsoplaneGuestRuntime></PropertyGroup></Project>");
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            var options = new DevelopmentOptions(directory.FullName, null, false, false, false, GuestRuntimeProfile.Mono, true);
            var failure = await Assert.ThrowsAsync<ArgumentException>(() => DebugDevelopment.RunAsync(options,
                deadline.Token));
            Assert.True(failure.Message.Contains("conflicts with the project configuration", StringComparison.Ordinal),
                "Debugging must require an explicit configuration change instead of substituting interpretation.");
            Assert.False(Directory.Exists(Path.Combine(directory.FullName, ".isoplane")),
                "A conflicting runtime override must not create publication or session state.");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
