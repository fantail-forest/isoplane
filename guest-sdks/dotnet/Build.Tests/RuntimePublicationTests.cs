using System.Text.Json;
using System.Xml.Linq;
using Xunit;

public sealed class RuntimePublicationTests : MsbuildTests
{
    [Fact]
    public async Task an_unchanged_pal_build_preserves_objects_and_registers_them_for_linking()
    {
        var project = PalProject();
        await BuildPal(project);
        var objects = Directory.GetFiles(Path.Combine(directory, "obj"), "*.o")
            .ToDictionary(path => path, path => (Bytes: File.ReadAllBytes(path), Modified: File.GetLastWriteTimeUtc(path)));
        await BuildPal(project);
        Assert.True(File.ReadAllLines(Path.Combine(directory, "compiler.log")).Length == 2,
            "An unchanged publication must reuse both PAL object compilations.");
        foreach (var (path, previous) in objects)
        {
            Assert.True(File.ReadAllBytes(path).SequenceEqual(previous.Bytes) && File.GetLastWriteTimeUtc(path) == previous.Modified,
                "Reused PAL objects must retain their bytes and filesystem timestamps.");
            Assert.True(File.ReadAllLines(Path.Combine(directory, "link-inputs")).Contains(path),
                "Every publication must register each PAL object with the native linker, including a skipped compile.");
        }
        Assert.True(File.ReadAllLines(Path.Combine(directory, "link-args")).Contains("-Wl,--wrap=wrapped_symbol"),
            "A skipped PAL compilation must preserve the host capability wrapper arguments.");
    }

    [Theory]
    [InlineData("source")]
    [InlineData("header")]
    [InlineData("removed-header")]
    [InlineData("toolchain-header")]
    [InlineData("compiler")]
    [InlineData("configuration")]
    [InlineData("missing-object")]
    public async Task changed_pal_inputs_rebuild_the_native_objects(string change)
    {
        var project = PalProject();
        await BuildPal(project);
        var path = change switch
        {
            "source" => Path.Combine(directory, "sdk/native/isoplane_pal.c"),
            "header" or "removed-header" => Path.Combine(directory, "sdk/native/pal.h"),
            "toolchain-header" => Path.Combine(directory, "toolchain/share/wasi-sysroot/include/system.h"),
            "compiler" => Path.Combine(directory, "toolchain/bin/clang"),
            "configuration" => project,
            "missing-object" => Path.Combine(directory, "obj/isoplane_pal.o"),
            _ => throw new ArgumentOutOfRangeException(nameof(change))
        };
        if (change is "removed-header" or "missing-object") File.Delete(path);
        else if (change == "configuration")
        {
            var document = XDocument.Load(project);
            document.Descendants("LinkerArg").First().SetAttributeValue("Include", "-Wl,--max-memory=33554432");
            document.Save(project);
        }
        else
        {
            File.AppendAllText(path, "\n");
            File.SetLastWriteTimeUtc(path, new DateTime(2100, 1, 1, 0, 0, 0, DateTimeKind.Utc));
        }
        await BuildPal(project);
        Assert.True(File.ReadAllLines(Path.Combine(directory, "compiler.log")).Length == 4,
            "Changing a PAL source, header, compiler or configuration, or losing an object, must rebuild both native objects: " + change);
    }

    [Fact]
    public async Task a_deleted_pal_source_fails_instead_of_reusing_an_old_object()
    {
        var project = PalProject();
        await BuildPal(project);
        File.Delete(Path.Combine(directory, "sdk/native/isoplane_pal.c"));
        var result = await Run(Msbuild(project, "-t:Exercise"));
        Assert.True(result.Exit != 0 && result.Output.Contains("isoplane_pal.c", StringComparison.Ordinal),
            "A deleted required source must fail compilation instead of silently reusing an old PAL: " + result.Output);
    }

    private async Task BuildPal(string project)
    {
        var result = await Run(Msbuild(project, "-t:Exercise"));
        Assert.True(result.Exit == 0, "The PAL build target must complete: " + result.Output);
    }

    private string PalProject()
    {
        var sdk = Directory.CreateDirectory(Path.Combine(directory, "sdk/native")).Parent!.FullName;
        var toolchain = Directory.CreateDirectory(Path.Combine(directory, "toolchain/bin")).Parent!.FullName;
        foreach (var name in new[] { "isoplane_pal.c", "isoplane_clock.c", "pal.h" })
            File.WriteAllText(Path.Combine(sdk, "native", name), "fixture " + name);
        var headers = Directory.CreateDirectory(Path.Combine(toolchain, "share/wasi-sysroot/include"));
        File.WriteAllText(Path.Combine(headers.FullName, "system.h"), "fixture system header");
        WriteExecutable(Path.Combine(toolchain, "bin/clang"), """
            #!/usr/bin/env python3
            import pathlib, sys
            args = sys.argv[1:]
            source = pathlib.Path(args[-1])
            data = source.read_bytes()
            output = pathlib.Path(args[args.index('-o') + 1])
            output.write_bytes(data)
            with (output.parent.parent / 'compiler.log').open('a') as log:
                log.write(str(source) + '\n')
            """);
        return WriteProject(
            new XElement("PropertyGroup",
                new XElement("_IsoplaneSdkDirectory", sdk + Path.DirectorySeparatorChar),
                new XElement("WASI_SDK_PATH", toolchain),
                new XElement("IntermediateOutputPath", Path.Combine(directory, "obj") + Path.DirectorySeparatorChar)),
            new XElement("ItemGroup",
                new XElement("LinkerArg", new XAttribute("Include", "-Wl,--max-memory=16777216")),
                new XElement("IsoplaneWrappedSymbol", new XAttribute("Include", "wrapped_symbol"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Publish.targets"))),
            new XElement("Target", new XAttribute("Name", "SetupProperties")),
            new XElement("Target", new XAttribute("Name", "Exercise"), new XAttribute("DependsOnTargets", "IsoplaneCompilePal"),
                new XElement("WriteLinesToFile", new XAttribute("File", Path.Combine(directory, "link-inputs")),
                    new XAttribute("Lines", "@(NativeLibrary)"), new XAttribute("Overwrite", "true")),
                new XElement("WriteLinesToFile", new XAttribute("File", Path.Combine(directory, "link-args")),
                    new XAttribute("Lines", "@(LinkerArg)"), new XAttribute("Overwrite", "true"))));
    }

    [Theory]
    [InlineData("NativeLibrary")]
    [InlineData("NativeObjects")]
    [InlineData("LinkerArg")]
    [InlineData("framework-shims")]
    public async Task selecting_an_older_native_input_still_invalidates_the_link(string input)
    {
        var project = PalProject();
        var original = Path.Combine(directory, "original.a");
        var replacement = Path.Combine(directory, "replacement.a");
        foreach (var path in new[] { original, replacement })
        {
            File.WriteAllText(path, path);
            File.SetLastWriteTimeUtc(path, new DateTime(2000, 1, 1, 0, 0, 0, DateTimeKind.Utc));
        }
        var document = XDocument.Load(project);
        if (input == "framework-shims")
            document.Root!.Element("PropertyGroup")!.Add(new XElement("IlcFrameworkNativePath", "$(SelectedNativeInput)/"));
        else
            document.Root!.Element("ItemGroup")!.Add(new XElement(input, new XAttribute("Include", "$(SelectedNativeInput)")));
        document.Save(project);
        foreach (var selected in new[] { original, replacement })
        {
            var start = Msbuild(project, "-t:Exercise");
            start.ArgumentList.Add("-p:SelectedNativeInput=" + selected);
            var result = await Run(start);
            Assert.True(result.Exit == 0, "Resolving a native input must complete: " + result.Output);
        }
        Assert.True(File.ReadAllLines(Path.Combine(directory, "compiler.log")).Length == 4,
            "Changing a native input path must rebuild a linker input even when the replacement has an older timestamp: " + input);
    }

    [Theory]
    [InlineData("host", "missing", "no verified package identity")]
    [InlineData("host", "different", "differs from the supplied toolchain")]
    [InlineData("target", "missing", "records no package digest")]
    [InlineData("target", "different", "is not the pack the feed offers")]
    public async Task compiler_setup_rejects_an_unverified_member_of_the_toolchain_pair(string member, string change, string diagnostic)
    {
        var project = NativeAotResolutionProject();
        var digest = Directory.GetFiles(Path.Combine(directory, member), "*.sha512").Single();
        if (change == "missing") File.Delete(digest);
        else File.WriteAllText(digest, "unapproved package");
        var result = await Run(Msbuild(project, "-t:ImportRuntimeIlcPackageTarget"));
        Assert.True(result.Exit != 0 && result.Output.Contains(diagnostic, StringComparison.Ordinal),
            "Compiler setup must reject an unverified toolchain member before compilation, including an incremental build: " + result.Output);
    }

    [Fact]
    public async Task compiler_setup_rejects_missing_restore_metadata()
    {
        var project = NativeAotResolutionProject();
        var document = XDocument.Load(project);
        document.Root!.Element("ItemGroup")!.Remove();
        document.Save(project);
        var result = await Run(Msbuild(project, "-t:ImportRuntimeIlcPackageTarget"));
        Assert.True(result.Exit != 0 && result.Output.Contains("requires restored package definitions", StringComparison.Ordinal),
            "Compiler setup must diagnose missing restore metadata before selecting toolchain packages: " + result.Output);
    }

    [Fact]
    public async Task compiler_setup_accepts_independently_versioned_members_of_the_verified_toolchain_pair()
    {
        var project = NativeAotResolutionProject();
        var start = Msbuild(project, "-t:ImportRuntimeIlcPackageTarget");
        start.ArgumentList.Add("-warnAsError");
        var result = await Run(start);
        Assert.True(result.Exit == 0,
            "The verified host and framework packages must resolve without a version mismatch warning: " + result.Output);
    }

    private string NativeAotResolutionProject()
    {
        const string host = "runtime.linux-x64.Microsoft.DotNet.ILCompiler.LLVM";
        const string target = "runtime.wasi-wasm.Microsoft.DotNet.ILCompiler.LLVM";
        var hostDirectory = Directory.CreateDirectory(Path.Combine(directory, "host")).FullName;
        var targetDirectory = Directory.CreateDirectory(Path.Combine(directory, "target")).FullName;
        File.WriteAllText(Path.Combine(hostDirectory, host.ToLowerInvariant() + ".1.0.0-host.nupkg.sha512"), "host digest");
        File.WriteAllText(Path.Combine(targetDirectory, target.ToLowerInvariant() + ".2.0.0-target.nupkg.sha512"), "target digest");
        return WriteProject(
            new XElement("PropertyGroup",
                new XElement("IsoplaneGuestRuntime", "NativeAOT"),
                new XElement("_targetArchitecture", "wasm"),
                new XElement("_hostPackageName", host),
                new XElement("_targetPackageName", target),
                new XElement("NETCoreSdkRuntimeIdentifier", "linux-x64"),
                new XElement("IsoplaneGuestCompilerVersion", "1.0.0-host"),
                new XElement("IsoplaneGuestCompilerSha512", "host digest"),
                new XElement("IsoplaneGuestNativeAotPackVersion", "2.0.0-target"),
                new XElement("IsoplaneGuestNativeAotPackSha512", "target digest")),
            new XElement("ItemGroup",
                new XElement("PackageDefinitions", new XAttribute("Include", host),
                    new XElement("Name", host), new XElement("Version", "1.0.0-host"), new XElement("ResolvedPath", hostDirectory)),
                new XElement("PackageDefinitions", new XAttribute("Include", target),
                    new XElement("Name", target), new XElement("Version", "2.0.0-target"), new XElement("ResolvedPath", targetDirectory))),
            new XElement("Target", new XAttribute("Name", "IsoplaneRequireGuestCompiler")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.NativeAot.targets"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Isoplane.Guest.targets"))));
    }

    [Theory]
    [InlineData("", "linux-x64", "compiler is missing")]
    [InlineData("test-compiler", "linux-arm64", "supports linux-x64")]
    public async Task native_compiler_prerequisites_fail_with_actionable_diagnostics(string version, string host, string diagnostic)
    {
        var project = WriteProject(
            new XElement("PropertyGroup",
                new XElement("_IsoplaneGuestRuntimeFeedDirectory", Path.Combine(directory, "feed")),
                new XElement("IsoplaneContainerBuild", "false"),
                new XElement("IsoplaneGuestCompilerVersion", version),
                new XElement("IsoplaneGuestCompilerRuntimeIdentifier", "linux-x64"),
                new XElement("NETCoreSdkRuntimeIdentifier", host)),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Isoplane.NativeAot.props"))));
        var result = await Run(Msbuild(project, "-t:IsoplaneRequireGuestCompiler"));
        Assert.True(result.Exit != 0 && result.Output.Contains(diagnostic, StringComparison.Ordinal),
            "A missing or incompatible compiler must fail before publication with its prerequisite diagnosis: " + result.Output);
    }

    [Theory]
    [InlineData("IsoplaneInterpretedDebug", "false")]
    [InlineData("IsoplaneNativeAotRelease", "true")]
    public async Task template_configurations_select_their_named_optimization_mode(string configuration, string optimize)
    {
        var template = XDocument.Load(Path.Combine(SdkDirectory(), "Templates/content/aspnet/SampleApp.csproj"));
        var project = WriteProject(
            new XElement("Import", new XAttribute("Project", "Sdk.props"), new XAttribute("Sdk", "Microsoft.NET.Sdk")),
            new XElement("PropertyGroup", new XElement("Configuration", configuration), new XElement("TargetFramework", "net10.0")),
            template.Root!.Element("PropertyGroup")!,
            new XElement("Import", new XAttribute("Project", "Sdk.targets"), new XAttribute("Sdk", "Microsoft.NET.Sdk")));
        var result = await Run(Msbuild(project, "-getProperty:Optimize"));
        Assert.True(result.Exit == 0 && result.Output.Trim() == optimize,
            "The named template configuration must select its intended optimization mode: " + result.Output);
    }

    [Theory]
    [InlineData(0, false)]
    [InlineData(2, false)]
    [InlineData(0, true)]
    [InlineData(2, true)]
    public async Task a_container_publication_mounts_the_feed_and_requires_host_validation(int verdict, bool overrideMemory)
    {
        var tools = Path.Combine(directory, "tools");
        var feed = Path.Combine(directory, "external-feed");
        var wasiSdk = Path.Combine(directory, "external-packs", "wasi-sdk");
        Directory.CreateDirectory(Path.Combine(wasiSdk, "Sdk"));
        File.WriteAllText(Path.Combine(wasiSdk, "Sdk", "Sdk.targets"), "<Project />");
        var app = Path.Combine(directory, "app");
        var publish = Path.Combine(directory, "published");
        foreach (var path in new[] { tools, feed, app }) Directory.CreateDirectory(path);
        File.WriteAllText(Path.Combine(feed, "Isoplane.GuestRuntime.props"), "<Project />");
        var argsFile = Path.Combine(directory, "container.json");
        var validatorLog = Path.Combine(directory, "validator.json");
        WriteExecutable(Path.Combine(tools, "docker"), """
            #!/usr/bin/env python3
            import json, os, pathlib, sys
            args = sys.argv[1:]
            if args[0] == 'create':
                pathlib.Path(os.environ['ISOPLANE_TEST_CONTAINER_ARGS']).write_text(json.dumps(args))
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
            pathlib.Path(os.environ['ISOPLANE_TEST_VALIDATOR_LOG']).write_text(json.dumps(sys.argv[1:]))
            sys.exit(int(os.environ['ISOPLANE_TEST_VERDICT']))
            """);
        var project = ContainerProject(app, feed, wasiSdk, validator, publish, overrideMemory ? "536870912" : "");
        var start = Msbuild(project, "-t:Exercise");
        start.Environment["PATH"] = tools + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH");
        start.Environment["ISOPLANE_TEST_CONTAINER_ARGS"] = argsFile;
        start.Environment["ISOPLANE_TEST_VALIDATOR_LOG"] = validatorLog;
        start.Environment["ISOPLANE_TEST_VERDICT"] = verdict.ToString();
        var result = await Run(start);
        Assert.True((result.Exit == 0) == (verdict == 0), "Host validation must determine publication success: " + result.Output);
        var args = JsonSerializer.Deserialize<string[]>(File.ReadAllText(argsFile))!;
        Assert.True(args.Contains(feed + ":" + feed + ":ro"), "The external runtime feed must be mounted read-only.");
        Assert.True(args.Contains(wasiSdk + ":" + wasiSdk + ":ro"), "The unpacked WASI SDK referenced by the external feed must be accessible inside the compiler.");
        Assert.True(args.Contains("-p:IsoplaneGuestRuntimeFeed=" + feed + "/"), "The compiler must receive the selected runtime feed.");
        Assert.True(args.Contains("-p:_IsoplaneContainerIntermediate=true"), "The container must produce an intermediate artifact for host validation.");
        Assert.True(!args.Contains("-p:IsoplaneValidate=false"), "Container publication must not depend on the obsolete public validation bypass.");
        var validation = JsonSerializer.Deserialize<string[]>(File.ReadAllText(validatorLog))!;
        Assert.True(validation[0] == "doctor" && validation[1].EndsWith("test.wasm", StringComparison.Ordinal), "The host CLI must judge the actual linked artifact.");
        Assert.True(validation.Contains("--guest-memory-bytes") == overrideMemory &&
            args.Contains("-p:IsoplaneGuestMemoryBytes=536870912") == overrideMemory &&
            args.Contains("-p:IsoplaneMemoryMaxBytes=536870912") == overrideMemory,
            "An explicit memory policy must reach inner linking and host admission without inferring permission from a declaration.");
        if (overrideMemory) Assert.True(validation.Contains("536870912") && validation.Contains("standard"),
            "The explicit memory override must retain Standard non-memory limits.");
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
        File.WriteAllText(Path.Combine(feed, "Isoplane.GuestRuntime.props"), "<Project />");
        var marker = Path.Combine(directory, "docker-started");
        WriteExecutable(Path.Combine(tools, "docker"), """
            #!/usr/bin/env python3
            import os, pathlib, sys
            pathlib.Path(os.environ['ISOPLANE_TEST_DOCKER_MARKER']).touch()
            sys.exit(1)
            """);
        var project = ContainerProject(app, feed, Path.Combine(directory, "missing-sdk"), "unused-validator", Path.Combine(directory, "publish"));
        var start = Msbuild(project, "-t:Exercise");
        start.Environment["PATH"] = tools + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH");
        start.Environment["ISOPLANE_TEST_DOCKER_MARKER"] = marker;
        var result = await Run(start);
        Assert.True(result.Exit != 0 && result.Output.Contains("unpacked WASI SDK is missing", StringComparison.Ordinal), "An incomplete runtime feed must name its missing WASI SDK: " + result.Output);
        Assert.True(!File.Exists(marker), "Missing build inputs must fail before acquiring container resources.");
    }

    [Theory]
    [InlineData("")]
    [InlineData("536870912")]
    public async Task native_validation_uses_only_the_explicit_admission_memory_override(string memoryBytes)
    {
        var module = Path.Combine(directory, "test.wasm");
        File.WriteAllText(module, "fixture");
        var validator = Path.Combine(directory, "validator");
        var log = Path.Combine(directory, "validator.json");
        WriteExecutable(validator, """
            #!/usr/bin/env python3
            import json, os, pathlib, sys
            pathlib.Path(os.environ['ISOPLANE_TEST_VALIDATOR_LOG']).write_text(json.dumps(sys.argv[1:]))
            """);
        var project = WriteProject(new XElement("PropertyGroup", new XElement("NativeBinary", module),
                new XElement("IsoplaneCli", validator), new XElement("IsoplanePlan", "standard"),
                new XElement("IsoplaneMemoryMaxBytes", "536870912"), new XElement("IsoplaneGuestMemoryBytes", memoryBytes)),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Validation.targets"))));
        var start = Msbuild(project, "-t:IsoplaneValidateModule");
        start.Environment["ISOPLANE_TEST_VALIDATOR_LOG"] = log;
        var result = await Run(start);
        Assert.True(result.Exit == 0, "The SDK validation target must run successfully: " + result.Output);
        var arguments = JsonSerializer.Deserialize<string[]>(File.ReadAllText(log))!;
        Assert.True(arguments.Contains("--guest-memory-bytes") == (memoryBytes.Length != 0) && arguments.Contains("standard"),
            "A large linker declaration cannot grant an admission override; an explicit override preserves all other Standard limits.");
        if (memoryBytes.Length != 0) Assert.True(arguments.Contains(memoryBytes), "The admission override must preserve its exact byte value.");
    }

    [Fact]
    public async Task ordinary_publication_cannot_opt_out_of_admission()
    {
        var module = Path.Combine(directory, "test.wasm");
        File.WriteAllText(module, "fixture");
        var project = WriteProject(new XElement("PropertyGroup", new XElement("NativeBinary", module),
                new XElement("IsoplaneCli", Path.Combine(directory, "missing-validator")), new XElement("IsoplaneValidate", "false")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Validation.targets"))));
        var result = await Run(Msbuild(project, "-t:IsoplaneValidateModule"));
        Assert.True(result.Exit != 0 && result.Output.Contains("ISOPLANE0002", StringComparison.Ordinal), "A missing validator must fail ordinary publication even when the obsolete bypass is supplied: " + result.Output);
    }

    [Theory]
    [InlineData("IsoplaneInterpretedDebug")]
    [InlineData("IsoplaneNativeAotRelease")]
    public async Task dotnet_run_passes_the_selected_configuration_to_local_metadata_evaluation(string configuration)
    {
        var project = WriteProject(
            new XElement("PropertyGroup", new XElement("Configuration", configuration),
                new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
        var start = Msbuild(project, "-t:ComputeRunArguments");
        start.ArgumentList.Add("-getProperty:RunArguments,RunCommand");
        var result = await Run(start);
        Assert.True(result.Exit == 0, "Run arguments must evaluate successfully: " + result.Output);
        using var document = JsonDocument.Parse(result.Output);
        var properties = document.RootElement.GetProperty("Properties");
        Assert.True(properties.GetProperty("RunArguments").GetString()!.Contains("--configuration \"" + configuration + "\"", StringComparison.Ordinal),
            "The CLI must evaluate UserSecretsId under the same configuration that built the running guest.");
        Assert.True(properties.GetProperty("RunCommand").GetString() == "isoplane",
            "The run command must launch the sandbox CLI even when dotnet watch invokes it.");
    }

    private string ContainerProject(string app, string feed, string wasiSdk, string validator, string publish, string memoryBytes = "")
    {
        return WriteProject(
            new XElement("UsingTask", new XAttribute("TaskName", "IsoplaneContainerPublish"), new XAttribute("TaskFactory", "RoslynCodeTaskFactory"),
                new XAttribute("AssemblyFile", "$(MSBuildToolsPath)/Microsoft.Build.Tasks.Core.dll"),
                new XElement("Task", new XElement("Code", new XAttribute("Type", "Class"), new XAttribute("Language", "cs"),
                    new XAttribute("Source", Path.Combine(SdkDirectory(), "Build/ContainerPublish.cs"))))),
            new XElement("Target", new XAttribute("Name", "Exercise"),
                new XElement("IsoplaneContainerPublish", new XAttribute("ProjectPath", Path.Combine(app, "guest.csproj")),
                    new XAttribute("SourceRoot", app), new XAttribute("SdkDirectory", SdkDirectory()),
                    new XAttribute("GuestRuntimeFeed", feed), new XAttribute("WasiSdkPackDirectory", wasiSdk), new XAttribute("Validator", validator),
                    new XAttribute("PublishDirectory", publish), new XAttribute("Configuration", "Release"),
                    new XAttribute("GuestMemoryBytes", memoryBytes), new XAttribute("MemoryMaxBytes", memoryBytes), 
                    new XAttribute("CacheDirectory", Path.Combine(directory, "cache")), new XAttribute("CommandTimeoutSeconds", "30"))));
    }

}
