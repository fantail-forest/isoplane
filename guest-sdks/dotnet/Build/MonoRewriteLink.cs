using System;
using System.IO;
using System.Text;
using Microsoft.Build.Framework;

/// <summary>Adapts the pinned Mono response file to wasi-sdk's raw core link.</summary>
public sealed class WispMonoRewriteLink : Microsoft.Build.Utilities.Task
{
    /// <summary>The upstream linker response file.</summary>
    [Required] public string Input { get; set; }
    /// <summary>The separate output file; upstream inputs stay immutable.</summary>
    [Required] public string Output { get; set; }
    /// <summary>The project directory against which upstream response paths resolve.</summary>
    [Required] public string ProjectDirectory { get; set; }

    /// <summary>Removes only the unsupported pthread archive flag from the pinned target.</summary>
    public override bool Execute()
    {
        var output = new StringBuilder();
        var removed = 0;
        foreach (var original in File.ReadAllLines(Input))
        {
            var line = original.Trim();
            if (line.StartsWith("\"@", StringComparison.Ordinal) && line.EndsWith("\"", StringComparison.Ordinal))
            {
                var path = line.Substring(2, line.Length - 3);
                line = File.ReadAllText(Path.GetFullPath(Path.Combine(ProjectDirectory, path)));
            }
            const string obsolete = ",-lwasi-emulated-pthread";
            if (line.Contains(obsolete))
            {
                removed += (line.Length - line.Replace(obsolete, "").Length) / obsolete.Length;
                line = line.Replace(obsolete, "");
            }
            output.AppendLine(line);
        }
        if (removed != 2)
        {
            Log.LogError("wisp: unexpected pinned Mono native response shape; expected two wasi-emulated-pthread flags (target and runtime response file).");
            return false;
        }
        File.WriteAllText(Output, output.ToString());
        return true;
    }
}
