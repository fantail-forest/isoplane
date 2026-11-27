using System.CommandLine;

namespace Isoplane.Cli;

/// <summary>The command definitions shared by argument parsing, help, shell completion and the CLI reference.</summary>
internal static class CliCommandLine
{
    /// <summary>
    /// The <c>isoplane</c> command and its subcommands, each running its member of <paramref name="actions"/>;
    /// without them, the definitions alone, which parse, complete and describe a command line but run nothing.
    /// </summary>
    internal static Command Create(CliActions? actions = null)
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
        dev.Add(new Option<string[]>("--content") { Description = "Directory of files added to the application's content for the session, each at its path beneath the directory; repeat for several. A later directory's file replaces an earlier one's and the project's at the same path. Isoplane.Testing uses it." });
        Value(dev, "--debug-launch-endpoint", "Debugger's IPv4 loopback endpoint.").Hidden = true;
        Flag(dev, "--no-launch-profile", "Do not load a launch profile.");
        Flag(dev, "--no-watch", "Do not watch for source changes.");
        Flag(dev, "--no-build", "Use compiled assemblies; requires --no-watch.");
        Flag(dev, "--debug", "Preserve the debugger connection across requests and rebuilds.");
        Flag(dev, "--shutdown-on-stdin-close", "Stop when standard input closes.");
        Json(dev);
        Act(dev, actions?.Dev);
        root.Add(dev);
        var debugAdapter = new Command("debug-adapter", "Serve the Debug Adapter Protocol on standard input/output.");
        Act(debugAdapter, actions?.DebugAdapter);
        root.Add(debugAdapter);
        var setup = new Command("setup", "Sets up Isoplane's tooling on this machine.");
        Value(setup, "--source", "Release directory or URL.");
        Flag(setup, "--yes", "Install missing native prerequisites and agree to Microsoft's license terms for its Debug Adapter Protocol library, without prompting.", "-y");
        Act(setup, actions?.Setup);
        root.Add(setup);
        var uninstall = new Command("uninstall-sdk", "Remove everything setup installed, keeping the CLI.");
        Act(uninstall, actions?.UninstallSdk);
        root.Add(uninstall);
        var cacheInfo = new Command("info", "Show runtime cache usage.");
        Act(cacheInfo, actions?.CacheInfo);
        var cacheClean = new Command("clean", "Remove compiled runtimes.");
        Act(cacheClean, actions?.CacheClean);
        root.Add(new Command("cache", "Manage compiled runtimes shared by local sessions.") { cacheInfo, cacheClean });
        var prepare = new Command("prepare-runtime", "Compile a shared runtime into the local cache.");
        Value(prepare, "--runtime", "SDK tools/runtime directory.").Required = true;
        Act(prepare, actions?.PrepareRuntime);
        root.Add(prepare);
        var doctor = new Command("doctor", "Scans for and reports common install and configuration issues.");
        Act(doctor, actions?.Doctor);
        root.Add(doctor);
        var init = new Command("init", "Register the Isoplane MCP server in a coding agent's project configuration.");
        var harness = new Option<string[]>("--harness") { Description = $"The coding agent to configure; repeat for several: {string.Join(", ", AgentHarness.Names)}.", Required = true, AllowMultipleArgumentsPerToken = true };
        harness.AcceptOnlyFromAmong(AgentHarness.Names);
        init.Add(harness);
        Value(init, "--directory", "Directory whose configuration is written (default: current directory), usually the repository root.");
        Act(init, actions?.AgentInit);
        var mcp = new Command("mcp", "Serve the Isoplane MCP server on standard input and output, for a coding agent to start.");
        Act(mcp, actions?.AgentMcp);
        root.Add(new Command("agent", "Connect a coding agent to this machine's Isoplane tools.") { init, mcp });
        var install = new Command("install", "Write the isoplane skill for the Isoplane packages a project references into skills directories. With --dot-agents or --dot-claude, it goes where the skill already is at or above the project, and otherwise under the nearest directory at or above the project that holds a .git entry or a solution file. Asks which skills directories to use when no option names one.");
        Value(install, "--project", "Project directory (default: current directory). The command restores the project first.");
        foreach (var directory in AgentSkill.Directories)
            Flag(install, directory.Option, $"Install into {directory.Path}, for {directory.Readers}.");
        Value(install, "--skills-path", "Install into this skills directory, such as .agents/skills or .claude/skills, relative to the current directory. The skill goes in its isoplane directory.");
        Flag(install, "--force", "Overwrite files the skill would create that already exist. Without it, the command writes nothing when any exists.");
        Act(install, actions?.SkillsInstall);
        root.Add(new Command("skills", "Give a coding agent the isoplane skill for a project.") { install });
        var bundle = new Command("bundle", "Build a managed application archive.");
        Project(bundle);
        Value(bundle, "--configuration", "Build configuration (default: Release).");
        Value(bundle, "--output", "Output archive file.");
        Flag(bundle, "--no-build", "Bundle existing build outputs without compiling or restoring.");
        Act(bundle, actions?.Bundle);
        root.Add(bundle);
        var daemon = new Command("daemon", "Run the local multi-application host in the foreground. Requires isoplaned on PATH.");
        Value(daemon, "--config", "Host configuration file.").Required = true;
        Flag(daemon, "--check", "Validate the host configuration without serving traffic.");
        Flag(daemon, "--print-config", "Print effective host configuration without application secrets.");
        daemon.Validators.Add(result =>
        {
            if (result.GetValue<bool>("--check") && result.GetValue<bool>("--print-config"))
                result.AddError("Choose either --check or --print-config.");
        });
        Act(daemon, actions?.Daemon);
        root.Add(daemon);
        var deploy = new Command("deploy", "Prepare a bundle on this machine and select it for the next daemon start. Requires isoplaned on PATH.");
        Flag(deploy, "--local", "Deploy on this machine; required.");
        Value(deploy, "--config", "Host configuration file.").Required = true;
        Value(deploy, "--app", "Application name from the host configuration.").Required = true;
        Value(deploy, "--bundle", "Portable application archive created by isoplane bundle.").Required = true;
        deploy.Validators.Add(result =>
        {
            if (!result.GetValue<bool>("--local")) result.AddError("Local deployment requires --local.");
        });
        Act(deploy, actions?.Deploy);
        root.Add(deploy);
        var status = new Command("status", "Show telemetry status.");
        Act(status, actions?.TelemetryStatus);
        var enable = new Command("enable", "Enable anonymous usage data.");
        Act(enable, actions?.TelemetryEnable);
        var disable = new Command("disable", "Disable anonymous usage data.");
        Act(disable, actions?.TelemetryDisable);
        root.Add(new Command("telemetry", $"Manage anonymous usage data. {TelemetryVariables.OptOut.Name}=1 or {TelemetryVariables.DoNotTrack.Name}=1 also disables it; {TelemetryVariables.Debug.Name}=1 prints events without sending them.")
        {
            status, enable, disable
        });
        var completions = new Command("completions", "Print a shell completion script. See the installation guide for shell setup.");
        var shells = Enum.GetNames<CompletionShell>().Select(name => name.ToLowerInvariant()).ToArray();
        var shell = new Argument<string>("shell") { Description = $"The shell: {string.Join(", ", shells)}." };
        shell.AcceptOnlyFromAmong(shells);
        completions.Add(shell);
        Act(completions, actions?.Completions);
        root.Add(completions);
        var complete = new Command("complete", "Return completion candidates for a command line.") { Hidden = true };
        complete.Add(new Argument<string>("line"));
        Act(complete, actions?.Complete);
        root.Add(complete);
        return root;
    }

    private static void Act(Command command, Func<ParseResult, Task<int>>? action)
    {
        if (action is not null) command.SetAction((parsed, _) => action(parsed));
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

/// <summary>
/// What the CLI runs for each command <see cref="CliCommandLine.Create"/> defines, given the command's
/// parse result. Each returns the exit status. A command that observes Ctrl+C makes its own
/// <see cref="CancellationToken"/>, so that one which does not is still ended by it.
/// </summary>
internal sealed record CliActions
{
    internal required Func<ParseResult, Task<int>> Dev { get; init; }
    internal required Func<ParseResult, Task<int>> DebugAdapter { get; init; }
    internal required Func<ParseResult, Task<int>> Setup { get; init; }
    internal required Func<ParseResult, Task<int>> UninstallSdk { get; init; }
    internal required Func<ParseResult, Task<int>> CacheInfo { get; init; }
    internal required Func<ParseResult, Task<int>> CacheClean { get; init; }
    internal required Func<ParseResult, Task<int>> PrepareRuntime { get; init; }
    internal required Func<ParseResult, Task<int>> Doctor { get; init; }
    internal required Func<ParseResult, Task<int>> AgentInit { get; init; }
    internal required Func<ParseResult, Task<int>> AgentMcp { get; init; }
    internal required Func<ParseResult, Task<int>> SkillsInstall { get; init; }
    internal required Func<ParseResult, Task<int>> Bundle { get; init; }
    internal required Func<ParseResult, Task<int>> Daemon { get; init; }
    internal required Func<ParseResult, Task<int>> Deploy { get; init; }
    internal required Func<ParseResult, Task<int>> TelemetryStatus { get; init; }
    internal required Func<ParseResult, Task<int>> TelemetryEnable { get; init; }
    internal required Func<ParseResult, Task<int>> TelemetryDisable { get; init; }
    internal required Func<ParseResult, Task<int>> Completions { get; init; }
    internal required Func<ParseResult, Task<int>> Complete { get; init; }
}
