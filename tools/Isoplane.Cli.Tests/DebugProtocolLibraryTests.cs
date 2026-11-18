using System.Diagnostics;
using Isoplane.Debugging;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class DebugProtocolLibraryTests
{
    private static readonly Func<string, Task<bool>> NobodyIsAsked = _ => throw new InvalidOperationException("the license question must not be asked");

    private static Task<int> SetUpAsync(ReleaseFixture fixture, SetupMachine machine, StaticOriginHandler handler, bool yes = false) =>
        Setup.RunAsync(new SetupOptions(fixture.Release, yes), fixture.Cli, fixture.Home, machine, new HttpClient(handler), CancellationToken.None);

    private static SetupMachine Machine(ReleaseFixture fixture, Func<string, Task<bool>> agree) =>
        Machines.Ready(fixture, new FakeRegistrations()) with { AgreeToLicense = agree };

    private static string Library(ReleaseFixture fixture) => DebugProtocolLibrary.PathIn(fixture.Home);

    [Fact]
    public async Task Agreeing_to_the_license_installs_the_verified_assembly_and_records_it_as_owned()
    {
        using var fixture = new ReleaseFixture();
        var handler = new StaticOriginHandler(fixture.Root);
        var asked = new List<string>();
        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, question => { asked.Add(question); return Task.FromResult(true); }), handler));

        Assert.Single(asked);
        Assert.Equal([ReleaseFixture.DebugProtocolRequest], handler.Requests);
        Assert.Equal(ReleaseFixture.DebugProtocolAssembly, await File.ReadAllBytesAsync(Library(fixture)));
        Assert.Equal([Path.GetFileName(Library(fixture))], Directory.GetFileSystemEntries(Path.GetDirectoryName(Library(fixture))!).Select(Path.GetFileName));
        var installation = Installation.Load(fixture.Home)!;
        Assert.Equal(new InstalledResource(InstalledResourceKind.File, Library(fixture), Owned: true, fixture.DebugProtocol.AssemblySha256),
            installation.Find(InstalledResourceKind.File, Library(fixture)));
        Assert.True(installation.Find(InstalledResourceKind.Directory, Path.GetDirectoryName(Library(fixture))!)?.Owned, "setup owns the directory it created for the library");
    }

    [Fact]
    public async Task Yes_agrees_to_the_license_and_installs_the_library_without_asking()
    {
        using var fixture = new ReleaseFixture();
        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, NobodyIsAsked), new StaticOriginHandler(fixture.Root), yes: true));
        Assert.Equal(ReleaseFixture.DebugProtocolAssembly, await File.ReadAllBytesAsync(Library(fixture)));
    }

    [Fact]
    public async Task Declining_the_license_completes_setup_without_downloading_or_recording_the_library()
    {
        using var fixture = new ReleaseFixture();
        var handler = new StaticOriginHandler(fixture.Root);
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await SetUpAsync(fixture, Machines.Ready(fixture, registrations) with { AgreeToLicense = _ => Task.FromResult(false) }, handler));

        Assert.Empty(handler.Requests);
        Assert.False(Directory.Exists(Path.GetDirectoryName(Library(fixture))), "a declined library leaves nothing behind");
        Assert.Null(Installation.Load(fixture.Home)!.Find(InstalledResourceKind.File, Library(fixture)));
        Assert.Equal(ReleaseFixture.Version, registrations.Templates["Isoplane.Templates"]);
    }

    [Fact]
    public async Task Setup_run_again_with_the_library_present_neither_asks_nor_downloads()
    {
        using var fixture = new ReleaseFixture();
        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, _ => Task.FromResult(true)), new StaticOriginHandler(fixture.Root)));
        var manifest = await File.ReadAllTextAsync(Installation.PathIn(fixture.Home));

        var handler = new StaticOriginHandler(fixture.Root);
        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, NobodyIsAsked), handler));
        Assert.Empty(handler.Requests);
        Assert.Equal(manifest, await File.ReadAllTextAsync(Installation.PathIn(fixture.Home)));
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task A_package_or_assembly_with_another_digest_fails_setup_and_installs_nothing(bool alteredPackage)
    {
        using var fixture = new ReleaseFixture();
        var other = new string('0', 64);
        var package = alteredPackage ? fixture.DebugProtocol with { Sha256 = other } : fixture.DebugProtocol with { AssemblySha256 = other };
        var machine = Machine(fixture, NobodyIsAsked) with { DebugProtocol = package };
        var error = await Assert.ThrowsAsync<DebugProtocolPackageException>(() => SetUpAsync(fixture, machine, new StaticOriginHandler(fixture.Root), yes: true));
        Assert.Contains("expected 000000000000", error.Message);
        Assert.False(Directory.Exists(Path.GetDirectoryName(Library(fixture))), "an unverified download must not reach the installation");
        Assert.Empty(Installation.Load(fixture.Home)!.Resources);
    }

    [Fact]
    public async Task A_package_that_cannot_be_downloaded_fails_setup_naming_its_address()
    {
        using var fixture = new ReleaseFixture();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Missing.Add(ReleaseFixture.DebugProtocolRequest);
        var error = await Assert.ThrowsAsync<DebugProtocolPackageException>(() => SetUpAsync(fixture, Machine(fixture, NobodyIsAsked), handler, yes: true));
        Assert.Contains(fixture.DebugProtocol.Download.AbsoluteUri, error.Message);
    }

    [Fact]
    public async Task Removal_deletes_the_library_setup_installed()
    {
        using var fixture = new ReleaseFixture();
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await SetUpAsync(fixture, Machines.Ready(fixture, registrations) with { AgreeToLicense = _ => Task.FromResult(true) }, new StaticOriginHandler(fixture.Root)));
        Assert.True(File.Exists(Library(fixture)), "setup must have installed the library for removal to delete");

        Assert.Equal(0, await SdkRemoval.RunAsync(fixture.Home, _ => registrations, CancellationToken.None));
        Assert.False(Directory.Exists(Path.GetDirectoryName(Library(fixture))), "removal deletes the library and its directory");
    }

    [Fact]
    public async Task Doctor_reports_whether_the_library_is_installed()
    {
        using var fixture = new ReleaseFixture();
        var missing = EnvironmentDoctor.DescribeDebugging(fixture.Home, fixture.DebugProtocol);
        Assert.StartsWith("Debugging is not available", missing);
        Assert.Contains("isoplane setup", missing);

        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, NobodyIsAsked), new StaticOriginHandler(fixture.Root), yes: true));
        Assert.StartsWith("Debugging is available", EnvironmentDoctor.DescribeDebugging(fixture.Home, fixture.DebugProtocol));
    }

    [Fact]
    public async Task The_adapter_refuses_an_installation_without_the_library_naming_it_and_the_command_that_installs_it()
    {
        using var fixture = new ReleaseFixture();
        var missing = await Assert.ThrowsAsync<DebugProtocolLibraryMissingException>(() => AdapterCommand.RunAsync(fixture.Home, CancellationToken.None));
        Assert.Equal(Library(fixture), missing.Path);
        Assert.Contains(DebugProtocolLibrary.PackageId, missing.Message);
        Assert.Contains("is not installed", missing.Message);
        Assert.Contains("isoplane setup", missing.Message);

        // The stand-in is not the assembly the debugger was compiled against, as after a CLI upgrade.
        Assert.Equal(0, await SetUpAsync(fixture, Machine(fixture, NobodyIsAsked), new StaticOriginHandler(fixture.Root), yes: true));
        var stale = await Assert.ThrowsAsync<DebugProtocolLibraryMissingException>(() => AdapterCommand.RunAsync(fixture.Home, CancellationToken.None));
        Assert.Contains("another version", stale.Message);
    }

    [Fact]
    public async Task The_debug_adapter_command_reports_a_missing_library_as_one_line_without_a_loader_exception()
    {
        var home = Directory.CreateTempSubdirectory("isoplane-adapter-").FullName;
        try
        {
            var start = new ProcessStartInfo("dotnet")
            {
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
            start.ArgumentList.Add("debug-adapter");
            start.Environment["ISOPLANE_HOME"] = home;
            start.Environment["ISOPLANE_TELEMETRY_OPTOUT"] = "1";
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the debug adapter.");
            process.StandardInput.Close();
            var output = process.StandardOutput.ReadToEndAsync();
            var errors = process.StandardError.ReadToEndAsync();
            using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                var reported = (await errors).Trim();
                Assert.Equal(1, process.ExitCode);
                Assert.Equal($"isoplane: {Assert.Throws<DebugProtocolLibraryMissingException>(() => { _ = AdapterCommand.RunAsync(home, CancellationToken.None); }).Message}", reported);
                Assert.Empty(await output);
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
        finally
        {
            Directory.Delete(home, recursive: true);
        }
    }
}
