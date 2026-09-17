using Microsoft.Build.Framework;

/// <summary>Records analysis invocations without scanning the runtime in target scheduling tests.</summary>
public sealed class ManagedToNativeGenerator : Microsoft.Build.Utilities.Task
{
    public string[] Assemblies { get; set; } = [];
    public string RuntimeIcallTableFile { get; set; } = "";
    public string IcallOutputPath { get; set; } = "";
    public string[] PInvokeModules { get; set; } = [];
    public string PInvokeSymbolInventoryFile { get; set; } = "";
    public bool RequirePInvokeSymbolInventory { get; set; }
    public string PInvokeOutputPath { get; set; } = "";
    public string InterpToNativeOutputPath { get; set; } = "";
    public bool IsLibraryMode { get; set; }
    [Output] public string[] FileWrites { get; set; } = [];

    public override bool Execute()
    {
        var directory = Path.GetDirectoryName(IcallOutputPath)!;
        File.AppendAllText(Path.Combine(directory, "generator.log"), "analysis\n");
        if (File.Exists(Path.Combine(directory, "fail")))
        {
            Log.LogError("Requested analysis failure.");
            return false;
        }
        FileWrites = [IcallOutputPath, PInvokeOutputPath, InterpToNativeOutputPath];
        foreach (var path in FileWrites)
            if (!File.Exists(path)) File.WriteAllText(path, "unchanged generated header");
        return true;
    }
}
