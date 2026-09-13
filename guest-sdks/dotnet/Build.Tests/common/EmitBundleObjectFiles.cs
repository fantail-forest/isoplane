using Microsoft.Build.Framework;
using Microsoft.Build.Utilities;

/// <summary>Records workload task invocations while preserving its output item shape.</summary>
public sealed class EmitBundleObjectFiles : Microsoft.Build.Utilities.Task
{
    public ITaskItem[] FilesToBundle { get; set; } = [];
    public string ClangExecutable { get; set; } = "";
    public string BundleRegistrationFunctionName { get; set; } = "";
    public string BundleFile { get; set; } = "";
    public string OutputDirectory { get; set; } = "";
    [Output] public ITaskItem[] BundledResources { get; set; } = [];
    [Output] public string BundleRegistrationFile { get; set; } = "";

    public override bool Execute()
    {
        File.AppendAllText(Path.Combine(OutputDirectory, "emitter.log"), BundleFile + "\n");
        BundledResources = FilesToBundle.Select(source =>
        {
            var result = new TaskItem(source);
            var destination = Path.Combine(OutputDirectory, Path.GetFileName(source.ItemSpec) + ".o");
            File.WriteAllBytes(destination, File.ReadAllBytes(source.ItemSpec));
            result.SetMetadata("DestinationFile", destination);
            return (ITaskItem)result;
        }).ToArray();
        if (!string.IsNullOrEmpty(BundleFile))
        {
            BundleRegistrationFile = Path.Combine(OutputDirectory, BundleFile);
            File.WriteAllText(BundleRegistrationFile, BundleRegistrationFunctionName + "\n" +
                string.Join("\n", FilesToBundle.Select(item => item.ItemSpec + "|" + item.GetMetadata("RegisteredName"))));
        }
        return true;
    }
}
