namespace Wisp.Testing;

/// <summary>How a test locates and shuts down its installed development tool.</summary>
public sealed record ApplicationOptions
{
    /// <summary>The installed isoplane executable, resolved through PATH unless an absolute path is supplied.</summary>
    public string CliPath { get; init; } = "isoplane";

    /// <summary>The application's ordinary MSBuild configuration, which selects its guest execution mode.</summary>
    public string Configuration { get; init; } = "Debug";

    /// <summary>Time allowed for owned process cleanup before the remaining process tree is terminated.</summary>
    public TimeSpan ShutdownTimeout { get; init; } = TimeSpan.FromSeconds(10);

    /// <summary>Maximum retained diagnostic characters from the child process.</summary>
    public int DiagnosticLimitCharacters { get; init; } = 16 * 1024;
}
