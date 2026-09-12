using System.Diagnostics;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Xml.Linq;
using Xunit;

public sealed class RuntimePublicationTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("wisp-publication-test-").FullName;
    private static string SdkDirectory([CallerFilePath] string source = "") => Path.GetFullPath(Path.Combine(Path.GetDirectoryName(source)!, ".."));

    [Theory]
    [InlineData("", "linux-x64", "compiler is missing")]
    [InlineData("test-compiler", "linux-arm64", "supports linux-x64")]
    public async Task native_compiler_prerequisites_fail_with_actionable_diagnostics(string version, string host, string diagnostic)
    {
        var project = WriteProject(
            new XElement("PropertyGroup",
                new XElement("_WispGuestRuntimeFeedDirectory", Path.Combine(directory, "feed")),
                new XElement("WispContainerBuild", "false"),
                new XElement("WispGuestCompilerVersion", version),
                new XElement("WispGuestCompilerRuntimeIdentifier", "linux-x64"),
                new XElement("NETCoreSdkRuntimeIdentifier", host)),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Wisp.NativeAot.props"))));
        var result = await Run(Msbuild(project, "-t:WispRequireGuestCompiler"));
        Assert.True(result.Exit != 0 && result.Output.Contains(diagnostic, StringComparison.Ordinal),
            "A missing or incompatible compiler must fail before publication with its prerequisite diagnosis: " + result.Output);
    }

    [Theory]
    [InlineData(0)]
    [InlineData(2)]
    public async Task a_container_publication_mounts_the_feed_and_requires_host_validation(int verdict)
    {
        var tools = Path.Combine(directory, "tools");
        var feed = Path.Combine(directory, "external-feed");
        var wasiSdk = Path.Combine(directory, "external-packs", "wasi-sdk");
        Directory.CreateDirectory(Path.Combine(wasiSdk, "Sdk"));
        File.WriteAllText(Path.Combine(wasiSdk, "Sdk", "Sdk.targets"), "<Project />");
        var app = Path.Combine(directory, "app");
        var publish = Path.Combine(directory, "published");
        foreach (var path in new[] { tools, feed, app }) Directory.CreateDirectory(path);
        File.WriteAllText(Path.Combine(feed, "Wisp.GuestRuntime.props"), "<Project />");
        var argsFile = Path.Combine(directory, "container.json");
        var validatorLog = Path.Combine(directory, "validator.json");
        WriteExecutable(Path.Combine(tools, "docker"), """
            #!/usr/bin/env python3
            import json, os, pathlib, sys
            args = sys.argv[1:]
            if args[0] == 'create':
                pathlib.Path(os.environ['WISP_TEST_CONTAINER_ARGS']).write_text(json.dumps(args))
                for arg in args:
                    if arg.endswith(':/build'):
                        output = pathlib.Path(arg[:-len(':/build')]) / 'publish'
                        output.mkdir()
                        (output / 'test.wasm').write_bytes(b'fixture module')
                        (output / 'test.wasm.runtime').write_text('test-runtime-identity')
            """);
        var validator = Path.Combine(tools, "validator");
        WriteExecutable(validator, """
            #!/usr/bin/env python3
            import json, os, pathlib, sys
            pathlib.Path(os.environ['WISP_TEST_VALIDATOR_LOG']).write_text(json.dumps(sys.argv[1:]))
            sys.exit(int(os.environ['WISP_TEST_VERDICT']))
            """);
        var project = ContainerProject(app, feed, wasiSdk, validator, publish);
        var start = Msbuild(project, "-t:Exercise");
        start.Environment["PATH"] = tools + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH");
        start.Environment["WISP_TEST_CONTAINER_ARGS"] = argsFile;
        start.Environment["WISP_TEST_VALIDATOR_LOG"] = validatorLog;
        start.Environment["WISP_TEST_VERDICT"] = verdict.ToString();
        var result = await Run(start);
        Assert.True((result.Exit == 0) == (verdict == 0), "Host validation must determine publication success: " + result.Output);
        var args = JsonSerializer.Deserialize<string[]>(File.ReadAllText(argsFile))!;
        Assert.True(args.Contains(feed + ":" + feed + ":ro"), "The external runtime feed must be mounted read-only.");
        Assert.True(args.Contains(wasiSdk + ":" + wasiSdk + ":ro"), "The unpacked WASI SDK referenced by the external feed must be accessible inside the compiler.");
        Assert.True(args.Contains("-p:WispGuestRuntimeFeed=" + feed + "/"), "The compiler must receive the selected runtime feed.");
        Assert.True(args.Contains("-p:_WispContainerIntermediate=true"), "The container must produce an intermediate artifact for host validation.");
        Assert.True(!args.Contains("-p:WispValidate=false"), "Container publication must not depend on the obsolete public validation bypass.");
        var validation = JsonSerializer.Deserialize<string[]>(File.ReadAllText(validatorLog))!;
        Assert.True(validation[0] == "doctor" && validation[1].EndsWith("test.wasm", StringComparison.Ordinal), "The host CLI must judge the actual linked artifact.");
        Assert.True(File.Exists(Path.Combine(publish, "test.wasm")) == (verdict == 0), "Rejected intermediate artifacts must not be published.");
        if (verdict == 0) Assert.True(File.ReadAllText(Path.Combine(publish, "test.wasm.runtime")) == "test-runtime-identity", "Publication must preserve the linked runtime identity.");
    }

    [Fact]
    public async Task an_incomplete_runtime_sdk_fails_before_the_container_is_started()
    {
        var tools = Path.Combine(directory, "tools");
        var feed = Path.Combine(directory, "feed");
        var app = Path.Combine(directory, "app");
        foreach (var path in new[] { tools, feed, app }) Directory.CreateDirectory(path);
        File.WriteAllText(Path.Combine(feed, "Wisp.GuestRuntime.props"), "<Project />");
        var marker = Path.Combine(directory, "docker-started");
        WriteExecutable(Path.Combine(tools, "docker"), """
            #!/usr/bin/env python3
            import os, pathlib, sys
            pathlib.Path(os.environ['WISP_TEST_DOCKER_MARKER']).touch()
            sys.exit(1)
            """);
        var project = ContainerProject(app, feed, Path.Combine(directory, "missing-sdk"), "unused-validator", Path.Combine(directory, "publish"));
        var start = Msbuild(project, "-t:Exercise");
        start.Environment["PATH"] = tools + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH");
        start.Environment["WISP_TEST_DOCKER_MARKER"] = marker;
        var result = await Run(start);
        Assert.True(result.Exit != 0 && result.Output.Contains("unpacked WASI SDK is missing", StringComparison.Ordinal), "An incomplete runtime feed must name its missing WASI SDK: " + result.Output);
        Assert.True(!File.Exists(marker), "Missing build inputs must fail before acquiring container resources.");
    }

    [Fact]
    public async Task ordinary_publication_cannot_opt_out_of_admission()
    {
        var module = Path.Combine(directory, "test.wasm");
        File.WriteAllText(module, "fixture");
        var project = WriteProject(new XElement("PropertyGroup", new XElement("NativeBinary", module),
                new XElement("WispCli", Path.Combine(directory, "missing-validator")), new XElement("WispValidate", "false")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Wisp.Validation.targets"))));
        var result = await Run(Msbuild(project, "-t:WispValidateModule"));
        Assert.True(result.Exit != 0 && result.Output.Contains("WISP0002", StringComparison.Ordinal), "A missing validator must fail ordinary publication even when the obsolete bypass is supplied: " + result.Output);
    }

    private string ContainerProject(string app, string feed, string wasiSdk, string validator, string publish)
    {
        return WriteProject(
            new XElement("UsingTask", new XAttribute("TaskName", "WispContainerPublish"), new XAttribute("TaskFactory", "RoslynCodeTaskFactory"),
                new XAttribute("AssemblyFile", "$(MSBuildToolsPath)/Microsoft.Build.Tasks.Core.dll"),
                new XElement("Task", new XElement("Code", new XAttribute("Type", "Class"), new XAttribute("Language", "cs"),
                    new XAttribute("Source", Path.Combine(SdkDirectory(), "Build/ContainerPublish.cs"))))),
            new XElement("Target", new XAttribute("Name", "Exercise"),
                new XElement("WispContainerPublish", new XAttribute("ProjectPath", Path.Combine(app, "guest.csproj")),
                    new XAttribute("SourceRoot", app), new XAttribute("SdkDirectory", SdkDirectory()),
                    new XAttribute("GuestRuntimeFeed", feed), new XAttribute("WasiSdkPackDirectory", wasiSdk), new XAttribute("Validator", validator),
                    new XAttribute("PublishDirectory", publish), new XAttribute("Configuration", "Release"),
                    new XAttribute("CacheDirectory", Path.Combine(directory, "cache")), new XAttribute("CommandTimeoutSeconds", "30"))));
    }

    private string WriteProject(params XElement[] elements)
    {
        var project = Path.Combine(directory, "test.proj");
        new XDocument(new XElement("Project", elements)).Save(project);
        return project;
    }

    private static ProcessStartInfo Msbuild(string project, string argument)
    {
        var start = new ProcessStartInfo("dotnet") { WorkingDirectory = Path.GetDirectoryName(project)!, UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var arg in new[] { "msbuild", project, "-nologo", "-nodeReuse:false", argument }) start.ArgumentList.Add(arg);
        start.Environment.Remove("WISP_GUEST_RUNTIME_FEED");
        return start;
    }

    private static async Task<(int Exit, string Output)> Run(ProcessStartInfo start)
    {
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(90));
        try
        {
            await process.WaitForExitAsync(timeout.Token);
            return (process.ExitCode, await output + await error);
        }
        finally
        {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
        }
    }

    private static void WriteExecutable(string path, string text)
    {
        File.WriteAllText(path, text + "\n");
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
