using System.CommandLine;

namespace Isoplane.Cli;

/// <summary>The command definitions shared by argument parsing, help and shell completion.</summary>
internal static class CliCommandLine
{
    internal static Command Create()
    {
        var root = new Command("isoplane", "Build and run Isoplane applications.")
        {
            new System.CommandLine.Help.HelpOption(),
            new VersionOption()
        };
        var dev = new Command("dev", "Run a local application. Launch settings supply the port and environment; --port overrides the port.");
        Project(dev);
        Value(dev, "--configuration", "Build configuration (default: Debug).", "-c");
        Value(dev, "--port", "Local HTTP port (0–65535).");
        Value(dev, "--launch-profile", "Profile from Properties/launchSettings.json.");
        Value(dev, "--settings", "JSON object of application settings for the session, such as {\"Section:Key\":\"value\"}; overrides User Secrets and the launch profile. Isoplane.Testing uses it.");
        Value(dev, "--debug-launch-endpoint", "Debugger's IPv4 loopback endpoint.").Hidden = true;
        Flag(dev, "--no-launch-profile", "Do not load a launch profile.");
        Flag(dev, "--no-watch", "Do not watch for source changes.");
        Flag(dev, "--no-build", "Use compiled assemblies; requires --no-watch.");
        Flag(dev, "--debug", "Preserve the debugger connection across requests and rebuilds.");
        Flag(dev, "--shutdown-on-stdin-close", "Stop when standard input closes.");
        Json(dev);
        root.Add(dev);
        root.Add(new Command("debug-adapter", "Serve the Debug Adapter Protocol on standard input/output."));
        var setup = new Command("setup", "Check prerequisites and install matching project templates.");
        Value(setup, "--source", "Release directory or URL.");
        Flag(setup, "--yes", "Install missing native prerequisites without prompting.", "-y");
        root.Add(setup);
        root.Add(new Command("uninstall-sdk", "Remove everything setup installed, keeping the CLI."));
        root.Add(new Command("cache", "Manage compiled runtimes shared by local sessions.")
        {
            new Command("info", "Show runtime cache usage."),
            new Command("clean", "Remove compiled runtimes.")
        });
        var prepare = new Command("prepare-runtime", "Compile a shared runtime into the local cache.");
        Value(prepare, "--runtime", "SDK tools/runtime directory.").Required = true;
        root.Add(prepare);
        root.Add(new Command("doctor", "Check this machine's .NET SDK, native library and templates."));
        var init = new Command("init", "Register the Isoplane MCP server in a coding agent's project configuration.");
        var harness = new Option<string[]>("--harness") { Description = $"The coding agent to configure; repeat for several: {string.Join(", ", AgentHarness.Names)}.", Required = true, AllowMultipleArgumentsPerToken = true };
        harness.AcceptOnlyFromAmong(AgentHarness.Names);
        init.Add(harness);
        Value(init, "--directory", "Directory whose configuration is written (default: current directory), usually the repository root.");
        root.Add(new Command("agent", "Connect a coding agent to this machine's Isoplane tools.")
        {
            init,
            new Command("mcp", "Serve the Isoplane MCP server on standard input and output, for a coding agent to start.")
        });
        var bundle = new Command("bundle", "Build a managed application archive.");
        Project(bundle);
        Value(bundle, "--configuration", "Build configuration (default: Release).");
        Value(bundle, "--output", "Output archive file.");
        root.Add(bundle);
        root.Add(new Command("telemetry", $"Manage anonymous usage data. {TelemetryVariables.OptOut.Name}=1 or {TelemetryVariables.DoNotTrack.Name}=1 also disables it; {TelemetryVariables.Debug.Name}=1 prints events without sending them.")
        {
            new Command("status", "Show telemetry status."),
            new Command("enable", "Enable anonymous usage data."),
            new Command("disable", "Disable anonymous usage data.")
        });
        var completions = new Command("completions", "Print a shell completion script. See the installation guide for shell setup.");
        var shells = Enum.GetNames<CompletionShell>().Select(name => name.ToLowerInvariant()).ToArray();
        var shell = new Argument<string>("shell") { Description = $"The shell: {string.Join(", ", shells)}." };
        shell.AcceptOnlyFromAmong(shells);
        completions.Add(shell);
        root.Add(completions);
        var complete = new Command("complete", "Return completion candidates for a command line.") { Hidden = true };
        complete.Add(new Argument<string>("line"));
        root.Add(complete);
        return root;
    }

    internal static ParseResult Parse(string[] args)
    {
        var result = Create().Parse(args);
        if (result.Errors.Count > 0)
            throw new ArgumentException(string.Join(Environment.NewLine, result.Errors.Select(error => error.Message)));
        return result;
    }

    private static void Project(Command command) => Value(command, "--project", "Project directory (default: current directory). ISOPLANE_TARGET_DIR moves generated files outside it.");

    private static void Json(Command command) => Flag(command, "--json", "Write machine-readable JSON.");

    private static Option<string> Value(Command command, string name, string description, params string[] aliases)
    {
        var option = new Option<string>(name, aliases) { Description = description };
        command.Add(option);
        return option;
    }

    private static void Flag(Command command, string name, string description, params string[] aliases) =>
        command.Add(new Option<bool>(name, aliases) { Description = description });
}
