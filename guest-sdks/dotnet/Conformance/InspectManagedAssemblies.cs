using Microsoft.Build.Framework;
using Isoplane.Build;

namespace Isoplane.Conformance;

/// <summary>Reports platform compatibility diagnostics without loading application code.</summary>
public sealed class InspectManagedAssemblies : Microsoft.Build.Utilities.Task
{
    /// <summary>Application and dependency assemblies produced by the managed build.</summary>
    [Required]
    public ITaskItem[] Assemblies { get; set; } = [];

    /// <summary>Assemblies built by the application and its project references.</summary>
    public ITaskItem[] ApplicationAssemblies { get; set; } = [];

    /// <summary>The application's own assembly, checked for one usable entry.</summary>
    public ITaskItem[] EntryAssembly { get; set; } = [];

    /// <summary>
    /// Assemblies the Isoplane analyzers checked as source while they compiled. Their thread and
    /// synchronous-wait diagnostics were reported there, at the calls, and are not repeated.
    /// </summary>
    public ITaskItem[] SourceAnalyzedAssemblies { get; set; } = [];

    /// <summary>Includes packaged dependencies in advisory diagnostics.</summary>
    public bool AnalyzeDependencies { get; set; }

    /// <summary>Enables advisory diagnostics for native import declarations.</summary>
    public bool WarnOnNativeImports { get; set; }

    /// <summary>
    /// The runtime's list of the native functions it provides, one <c>library!entry</c> per line;
    /// their imports are not reported.
    /// </summary>
    public string ProvidedNativeImportsFile { get; set; } = "";

    /// <summary>Warning codes suppressed by the project.</summary>
    public string[] NoWarn { get; set; } = [];

    /// <summary>Warning codes promoted to errors by the project.</summary>
    public string[] WarningsAsErrors { get; set; } = [];

    /// <summary>Warning codes excluded from blanket promotion to errors.</summary>
    public string[] WarningsNotAsErrors { get; set; } = [];

    /// <summary>Promotes unsuppressed warnings to errors.</summary>
    public bool TreatWarningsAsErrors { get; set; }

    private static bool ContainsCode(IEnumerable<string> codes, string code) => codes
        .SelectMany(value => value.Split(',', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        .Contains(code, StringComparer.OrdinalIgnoreCase);

    /// <summary>Reads managed metadata and reports compatibility warnings and invalid input errors.</summary>
    public override bool Execute()
    {
        var applicationPaths = ApplicationAssemblies.Select(item => item.GetMetadata("FullPath"))
            .ToHashSet(OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal);
        var sourceAnalyzed = SourceAnalyzedAssemblies.Select(item => item.GetMetadata("FullPath")).ToHashSet(applicationPaths.Comparer);
        var provided = ProvidedNativeImportsFile.Length == 0 ? [] : File.ReadAllLines(ProvidedNativeImportsFile)
            .Where(line => line.Length != 0).ToHashSet(StringComparer.Ordinal);
        foreach (var path in Assemblies.Select(item => item.GetMetadata("FullPath")).Distinct(StringComparer.Ordinal))
        {
            try
            {
                var assembly = AssemblyClosure.InspectFile(path);
                if (!AnalyzeDependencies && !applicationPaths.Contains(path)) continue;
                foreach (var diagnostic in AssemblyClosure.Diagnostics([assembly], WarnOnNativeImports, provided))
                {
                    if (ContainsCode(NoWarn, diagnostic.Code)) continue;
                    if (diagnostic.Code is "ISOPLANE1002" or "ISOPLANE1003" && sourceAnalyzed.Contains(path)) continue;
                    if (ContainsCode(WarningsAsErrors, diagnostic.Code)
                        || (TreatWarningsAsErrors && !ContainsCode(WarningsNotAsErrors, diagnostic.Code)))
                        Log.LogError(null, diagnostic.Code, null, path, 0, 0, 0, 0, "{0}", diagnostic.Message);
                    else
                        Log.LogWarning(null, diagnostic.Code, null, path, 0, 0, 0, 0, "{0}", diagnostic.Message);
                }
            }
            catch (Exception error) when (error is AssemblyInputException or IOException or UnauthorizedAccessException)
            {
                Log.LogError(null, BuildDiagnostics.UnreadableAssembly.Code, null, path, 0, 0, 0, 0, "{0}", error.Message);
            }
        }
        foreach (var path in EntryAssembly.Select(item => item.GetMetadata("FullPath")))
        {
            try { ApplicationEntryDiscovery.Discover(path); }
            catch (ApplicationEntryException error)
            {
                Log.LogError(null, ApplicationEntryException.Code, null, path, 0, 0, 0, 0, "{0}", error.Message);
            }
        }
        return !Log.HasLoggedErrors;
    }
}
