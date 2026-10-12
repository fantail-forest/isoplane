namespace Isoplane.Testing;

/// <summary>How a test locates and shuts down its installed development tool.</summary>
public sealed record ApplicationOptions
{
    /// <summary>The CLI executable, resolved through PATH unless absolute. Defaults to <c>isoplane</c>.</summary>
    public string CliPath { get; init; } = "isoplane";

    /// <summary>The application's ordinary MSBuild configuration. Debug and Release use the same managed runtime. Defaults to <c>Debug</c>.</summary>
    public string Configuration { get; init; } = "Debug";

    /// <summary>Time allowed for owned process cleanup before the remaining process tree is terminated. Defaults to 10 seconds.</summary>
    public TimeSpan ShutdownTimeout { get; init; } = TimeSpan.FromSeconds(10);

    /// <summary>Maximum retained diagnostic characters from the child process. Defaults to 16,384.</summary>
    public int DiagnosticLimitCharacters { get; init; } = 16 * 1024;

    /// <summary>
    /// Configuration settings for this session, such as a connection string to a database the test
    /// started. Keys use ASP.NET's colon-separated form. They override the project's launch profile
    /// and User Secrets.
    /// </summary>
    public IReadOnlyDictionary<string, string> Settings { get; init; } = new Dictionary<string, string>();
}
