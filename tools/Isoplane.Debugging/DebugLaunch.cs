using System.Net;

namespace Isoplane.Debugging;

/// <summary>Identifies the local project and build configuration an editor starts.</summary>
/// <param name="Project">Absolute project directory.</param>
/// <param name="Configuration">MSBuild configuration to compile.</param>
/// <param name="Port">HTTP listener port, or null to use the project's default.</param>
/// <param name="Watch">Rebuild source edits for subsequent requests.</param>
public sealed record DebugLaunchOptions(string Project, string Configuration, ushort? Port, bool Watch = true);

/// <summary>A command whose process is owned by the editor terminal.</summary>
/// <param name="Directory">Working directory.</param>
/// <param name="Arguments">Executable followed by its arguments, without shell quoting.</param>
public sealed record DebugTerminal(string Directory, IReadOnlyList<string> Arguments);

/// <summary>Owns local application execution for an editor's debug session.</summary>
public interface DebugLauncher
{
    /// <summary>Runs until application exit or cancellation and reports its debugger endpoint once. Uses the supplied terminal callback when present; otherwise owns and releases the application process.</summary>
    Task RunAsync(DebugLaunchOptions options, Func<IPEndPoint, Task> ready, Action<string> output, Func<DebugTerminal, CancellationToken, Task>? terminal, CancellationToken cancellation);
}
