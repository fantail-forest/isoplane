using System.Net;

namespace Isoplane.Debugging;

/// <summary>Identifies the local project and build configuration an editor starts.</summary>
/// <param name="Project">Absolute project directory.</param>
/// <param name="Configuration">MSBuild configuration to compile.</param>
/// <param name="Port">HTTP listener port, or null to use the project's default.</param>
public sealed record DebugLaunchOptions(string Project, string Configuration, ushort? Port);

/// <summary>Owns local application execution for an editor's debug session.</summary>
public interface DebugLauncher
{
    /// <summary>Runs until application exit or cancellation, reports its debugger endpoint once, and forwards console output. Releases owned processes before returning.</summary>
    Task RunAsync(DebugLaunchOptions options, Action<IPEndPoint> ready, Action<string> output, CancellationToken cancellation);
}
