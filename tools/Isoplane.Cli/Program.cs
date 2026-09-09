using System.Reflection;
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
    if (args.Length == 0 || args is ["--help"] or ["-h"])
    {
        Console.WriteLine("isoplane dev [--project DIRECTORY] [--port PORT] [--no-watch] [--json] [--shutdown-on-stdin-close]\nisoplane doctor MODULE [OPTIONS]\nisoplane --version");
        return 0;
    }
    using var cancellation = new CancellationTokenSource();
    Console.CancelKeyPress += (_, e) => { e.Cancel = true; cancellation.Cancel(); };
    var runtime = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp");
    if (!File.Exists(runtime))
        throw new InvalidOperationException("The CLI package is missing its shared runtime; reinstall a complete local package.");
    if (!OperatingSystem.IsWindows())
        File.SetUnixFileMode(runtime, File.GetUnixFileMode(runtime) | UnixFileMode.UserExecute);
    return args[0] switch
    {
        "dev" => await Development.RunAsync(DevelopmentOptions.Parse(args[1..]), runtime, cancellation.Token),
        "doctor" => await Processes.RunAsync(runtime, args, Environment.CurrentDirectory, runtime, cancellation.Token),
        _ => throw new ArgumentException($"Unknown command '{args[0]}'. Run isoplane --help.")
    };
}
catch (OperationCanceledException)
{
    return 0;
}
catch (Exception error) when (error is ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
{
    Console.Error.WriteLine($"isoplane: {error.Message}");
    return 1;
}
