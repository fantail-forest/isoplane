using System.Reflection;
using Wisp.Cli;
using Wisp.Native;

try
{
    if (args is ["--version"])
    {
        var version = typeof(Program).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion
            ?? throw new InvalidOperationException("The CLI package is missing its version metadata; reinstall a complete package.");
        Console.WriteLine($"isoplane {version}");
        return 0;
    }
    if (args.Length == 0 || args is ["--help"] or ["-h"] or [_, "--help" or "-h"])
    {
        Console.WriteLine("isoplane dev [--project DIRECTORY] [--port PORT] [--no-watch] [--guest-runtime mono|nativeaot] [--debug] [--json] [--shutdown-on-stdin-close]\n--debug requires --no-watch and serves one request with an external Mono debugger.\nisoplane setup --source DIRECTORY\nisoplane doctor\nisoplane doctor MODULE [OPTIONS]\nisoplane --version");
        Console.WriteLine("isoplane prepare --project DIRECTORY --configuration CONFIGURATION --module FILE [--json]\nPrepare a built application for local execution without starting it.");
        Console.WriteLine("isoplane bundle --output FILE [--project DIRECTORY] [--configuration CONFIGURATION]\nCompile managed deployment input as a managed application archive.");
        return 0;
    }
    using var cancellation = new CommandCancellation();
    if (args is ["setup", "--source", var source])
    {
        var installed = await Toolchain.InstallAsync(source, Toolchain.Home, cancellation.Token, Path.Combine(AppContext.BaseDirectory, "runtime"));
        Console.WriteLine($"Installed and verified toolchain: {installed}");
        Console.WriteLine($"Use {Path.Combine(Toolchain.Home, "dotnet")} as DOTNET_ROOT and put it first on PATH for ordinary dotnet commands.");
        return 0;
    }
    if (args is ["doctor"])
    {
        Console.Error.WriteLine("Checking toolchain integrity...");
        var installed = await Toolchain.DiagnoseAsync(Toolchain.Home, cancellation.Token, Path.Combine(AppContext.BaseDirectory, "runtime"));
        Console.Error.WriteLine("Checking NativeAOT compiler dependencies...");
        Toolchain.ProbeCompiler(installed);
        NativeRuntime.EnsureLoaded();
        var probes = new[]
        {
            (Name: ".NET SDK", Path: "dotnet/dotnet"),
            (Name: "WebAssembly compiler", Path: "wasi-sdk/bin/clang"),
            (Name: "WebAssembly tools", Path: "bin/wasm-tools"),
        };
        foreach (var probe in probes)
        {
            Console.Error.WriteLine($"Checking {probe.Name}...");
            await Processes.CheckAsync(Path.Combine(installed, probe.Path), ["--version"], installed, cancellation.Token);
        }
        Console.WriteLine("Your Isoplane toolchain is ready.");
        return 0;
    }
    if (args[0] == "doctor") return ModuleDoctor.Run(args);
    if (args[0] == "bundle")
    {
        var options = BundleOptions.Parse(args[1..]);
        await ManagedBundle.CreateAsync(options, cancellation.Token);
        Console.WriteLine(options.Output);
        return 0;
    }
    return args[0] switch
    {
        "prepare" => await Development.PrepareAsync(args[1..], cancellation.Token),
        "dev" => await Development.RunAsync(DevelopmentOptions.Parse(args[1..]), cancellation.Token),
        _ => throw new ArgumentException($"Unknown command '{args[0]}'. Run isoplane --help.")
    };
}
catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
{
    return args.Length > 0 && args[0] is "bundle" or "prepare" ? 130 : 0;
}
catch (Exception error) when (error is System.Text.Json.JsonException or ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
{
    Console.Error.WriteLine($"isoplane: {error.Message}");
    return 1;
}
