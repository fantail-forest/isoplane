using Microsoft.Build.Framework;
using Wisp.Build;

namespace Wisp.Conformance;

/// <summary>Reports platform compatibility diagnostics without loading application code.</summary>
public sealed class InspectManagedAssemblies : Microsoft.Build.Utilities.Task
{
    /// <summary>Application and dependency assemblies produced by the managed build.</summary>
    [Required]
    public ITaskItem[] Assemblies { get; set; } = [];

    /// <summary>Reads managed metadata and reports compatibility warnings and invalid input errors.</summary>
    public override bool Execute()
    {
        foreach (var path in Assemblies.Select(item => item.GetMetadata("FullPath")).Distinct(StringComparer.Ordinal))
        {
            try
            {
                var assembly = AssemblyClosure.InspectFile(path);
                foreach (var diagnostic in AssemblyClosure.Diagnostics([assembly]))
                    Log.LogWarning(null, diagnostic.Code, null, path, 0, 0, 0, 0, "{0}", diagnostic.Message);
            }
            catch (Exception error) when (error is AssemblyInputException or IOException or UnauthorizedAccessException)
            {
                Log.LogError(null, "WISP1000", null, path, 0, 0, 0, 0, "{0}", error.Message);
            }
        }
        return !Log.HasLoggedErrors;
    }
}
