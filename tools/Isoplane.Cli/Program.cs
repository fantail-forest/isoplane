using System.Reflection;
using System.Runtime.InteropServices;
using Wisp.Cli;

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
        Console.WriteLine("isoplane bundle --output FILE [--project DIRECTORY] [--configuration CONFIGURATION]\nCompile managed deployment input as a managed application archive.");
        return 0;
    }
    if (args is ["process-owner-path"])
    {
        var supervisor = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp-process");
        if (!File.Exists(supervisor))
            throw new InvalidOperationException("The CLI package is missing its process supervisor; reinstall a complete package.");
        if (!OperatingSystem.IsWindows())
            File.SetUnixFileMode(supervisor, File.GetUnixFileMode(supervisor) | UnixFileMode.UserExecute);
        Console.WriteLine(supervisor);
        return 0;
    }
    using var cancellation = new CancellationTokenSource();
    Console.CancelKeyPress += (_, e) => { e.Cancel = true; cancellation.Cancel(); };
    using var termination = OperatingSystem.IsWindows() ? null : PosixSignalRegistration.Create(PosixSignal.SIGTERM, context =>
    {
        context.Cancel = true;
        cancellation.Cancel();
    });
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
        var probes = new[]
        {
            (Name: ".NET SDK", Path: "dotnet/dotnet"),
            (Name: "WebAssembly compiler", Path: "wasi-sdk/bin/clang"),
            (Name: "WebAssembly tools", Path: "bin/wasm-tools"),
            (Name: "Isoplane runtime", Path: "bin/wisp"),
        };
        foreach (var probe in probes)
        {
            Console.Error.WriteLine($"Checking {probe.Name}...");
            await Processes.CheckAsync(Path.Combine(installed, probe.Path), ["--version"], installed,
                Path.Combine(installed, "bin", "wisp"), cancellation.Token);
        }
        Console.WriteLine("Your Isoplane toolchain is ready.");
        return 0;
    }
    var runtime = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp");
    if (!File.Exists(runtime))
        throw new InvalidOperationException("The CLI package is missing its shared runtime; reinstall a complete local package.");
    if (!OperatingSystem.IsWindows())
        File.SetUnixFileMode(runtime, File.GetUnixFileMode(runtime) | UnixFileMode.UserExecute);
    if (args[0] == "bundle")
    {
        var options = BundleOptions.Parse(args[1..]);
        await ManagedBundle.CreateAsync(options, runtime, cancellation.Token);
        Console.WriteLine(options.Output);
        return 0;
    }
    return args[0] switch
    {
        "dev" => await Development.RunAsync(DevelopmentOptions.Parse(args[1..]), runtime, cancellation.Token),
        "doctor" => await Processes.RunAsync(runtime, args, Environment.CurrentDirectory, runtime, cancellation.Token),
        _ => throw new ArgumentException($"Unknown command '{args[0]}'. Run isoplane --help.")
    };
}
catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
{
    return args.Length > 0 && args[0] is "bundle" ? 130 : 0;
}
catch (Exception error) when (error is System.Text.Json.JsonException or ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
{
    Console.Error.WriteLine($"isoplane: {error.Message}");
    return 1;
}
