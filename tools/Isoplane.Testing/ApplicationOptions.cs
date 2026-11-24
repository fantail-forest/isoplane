namespace Isoplane.Testing;

/// <summary>
/// Optional settings for <see cref="IsoplaneApplication.StartAsync"/>: which build of the application
/// to run, what settings and content files to give it, and how the <c>isoplane</c> command is found and stopped.
/// </summary>
/// <remarks>
/// Every property has a default that suits most tests, so set only what a test needs, in an object
/// initializer. The two most often set are <see cref="Settings"/> and <see cref="Configuration"/>.
/// </remarks>
/// <example>
/// <code>
/// var options = new ApplicationOptions
/// {
///     Settings = new Dictionary&lt;string, string&gt; { ["ConnectionStrings:Todos"] = connectionString }
/// };
/// await using var app = await IsoplaneApplication.StartAsync(projectDirectory, options);
/// </code>
/// </example>
public sealed record ApplicationOptions
{
    /// <summary>
    /// The <c>isoplane</c> command the test runs the application with. Defaults to <c>isoplane</c>, found
    /// on the <c>PATH</c>.
    /// </summary>
    /// <remarks>
    /// Set an absolute path when the command is not on the <c>PATH</c> of the process running the
    /// tests, as can happen on a build server.
    /// </remarks>
    public string CliPath { get; init; } = "isoplane";

    /// <summary>
    /// The build configuration of the application under test, <c>Debug</c> or <c>Release</c>. Defaults to
    /// <c>Debug</c>.
    /// </summary>
    /// <remarks>
    /// It is the configuration the application is built with, not the test project's. The two differ
    /// only in compiler optimization: both run on the same runtime in the sandbox.
    /// </remarks>
    public string Configuration { get; init; } = "Debug";

    /// <summary>
    /// How long <see cref="IsoplaneApplication.DisposeAsync"/> waits for the application to stop before
    /// stopping it by force. Defaults to 10 seconds.
    /// </summary>
    /// <remarks>
    /// When the wait runs out, the processes the test started are terminated and
    /// <see cref="IsoplaneApplication.DisposeAsync"/> throws <c>TimeoutException</c>. The value must be
    /// greater than zero.
    /// </remarks>
    public TimeSpan ShutdownTimeout { get; init; } = TimeSpan.FromSeconds(10);

    /// <summary>
    /// How many characters <see cref="IsoplaneApplication.DiagnosticOutput"/> keeps. Defaults to 16,384.
    /// </summary>
    /// <remarks>
    /// When the output grows past the limit, the oldest characters are dropped. The value must be
    /// greater than zero.
    /// </remarks>
    public int DiagnosticLimitCharacters { get; init; } = 16 * 1024;

    /// <summary>
    /// Settings to give the application for this run, such as the connection string of a database the
    /// test started. Empty by default.
    /// </summary>
    /// <remarks>
    /// Names use ASP.NET's form, <c>Section:Key</c>. The application reads them as it reads any
    /// setting: through <c>IConfiguration</c>, or <c>RuntimeConfiguration.ReadValues</c> in an
    /// application written without ASP.NET. They override the same settings in the project's launch
    /// profile and User Secrets, and apply only to the application this test starts.
    /// </remarks>
    public IReadOnlyDictionary<string, string> Settings { get; init; } = new Dictionary<string, string>();

    /// <summary>
    /// Files to give the application for this run, such as the templates or data files a test's
    /// expectations are written against. Empty by default.
    /// </summary>
    /// <remarks>
    /// <para>
    /// The files join the content the application's project bundles, and the application reads them the
    /// same way. A file at the path of one of the project's replaces it, and of two sources that give the
    /// same path the later one wins.
    /// Nothing is written to the application's project directory, and the files apply only to the
    /// application this test starts.
    /// </para>
    /// <para>
    /// The files count toward the application's package size and file limits, as the project's own do.
    /// <see cref="IsoplaneApplication.StartAsync"/> builds the application's package from them, so a
    /// later change to a source file does not reach the running application.
    /// </para>
    /// </remarks>
    /// <example>
    /// <code>
    /// var options = new ApplicationOptions
    /// {
    ///     Content =
    ///     [
    ///         new ContentDirectory(Path.Combine(AppContext.BaseDirectory, "Content")),
    ///         new ContentFile("Knowledge/Articles/expenses.md", articlePath)
    ///     ]
    /// };
    /// </code>
    /// </example>
    public IReadOnlyList<ContentSource> Content { get; init; } = [];
}
