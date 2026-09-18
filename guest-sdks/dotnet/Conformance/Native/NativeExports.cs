using System.ComponentModel;
using System.Diagnostics;
using Microsoft.Build.Framework;

namespace Isoplane.Conformance;

/// <summary>Inventories exports from the exact native inputs selected for guest linking.</summary>
public sealed class NativeExports : Microsoft.Build.Utilities.Task
{
    /// <summary>The selected WASI SDK's llvm-nm executable.</summary>
    [Required]
    public string Tool { get; set; } = "";

    /// <summary>Selected archives or objects; Library metadata names an optional direct P/Invoke module.</summary>
    [Required]
    public ITaskItem[] Inputs { get; set; } = [];

    /// <summary>Destination for the sorted native symbol inventory.</summary>
    [Required]
    public string SymbolsFile { get; set; } = "";

    /// <summary>Optional destination for export-specific NativeAOT direct P/Invoke entries.</summary>
    public string DirectPInvokeFile { get; set; } = "";

    /// <summary>The selected WASI clang used to resolve its implicit runtime archives.</summary>
    public string Compiler { get; set; } = "";

    /// <summary>The compiler target used by the guest link.</summary>
    public string TargetTriple { get; set; } = "";

    /// <summary>The sysroot selected by the guest's native link.</summary>
    public string SysRoot { get; set; } = "";

    /// <summary>Archive filenames resolved by that compiler and added explicitly to the link.</summary>
    public string[] SystemLibraries { get; set; } = [];

    /// <summary>Includes the compiler's selected builtins archive in the inventory and link.</summary>
    public bool IncludeCompilerBuiltins { get; set; }

    /// <summary>Resolved implicit archives, which the caller must also pass to the native linker.</summary>
    [Output]
    public ITaskItem[] ResolvedLibraries { get; private set; } = [];

    /// <summary>Scans every input on each invocation so replaced archives cannot reuse a stale inventory.</summary>
    public override bool Execute()
    {
        try
        {
            if (Inputs.Length == 0)
                throw new InvalidDataException("No native link inputs were supplied for the export inventory.");
            var symbols = new SortedSet<string>(StringComparer.Ordinal);
            var libraries = new SortedSet<string>(StringComparer.Ordinal) { "*" };
            var resolved = SystemLibraries.Select(name => ResolveLibrary("--print-file-name=" + name)).ToList();
            if (IncludeCompilerBuiltins) resolved.Add(ResolveLibrary("--print-libgcc-file-name"));
            ResolvedLibraries = resolved.Select(path => (ITaskItem)new Microsoft.Build.Utilities.TaskItem(path)).ToArray();
            foreach (var input in Inputs.Concat(ResolvedLibraries))
            {
                var path = input.GetMetadata("FullPath");
                if (!File.Exists(path))
                    throw new FileNotFoundException("A selected native link input is missing.", path);
                var exports = ReadExports(path);
                symbols.UnionWith(exports);
                var library = input.GetMetadata("Library");
                if (library.Length != 0)
                {
                    if (library.Contains('!') || library.Any(char.IsWhiteSpace))
                        throw new InvalidDataException($"Invalid direct P/Invoke module name: {library}");
                    libraries.Add(library);
                }
            }
            if (symbols.Count == 0)
                throw new InvalidDataException("The selected native link inputs export no symbols.");
            WriteIfChanged(SymbolsFile, symbols);
            if (DirectPInvokeFile.Length != 0)
            {
                var direct = libraries.SelectMany(library => symbols.Select(symbol => library + "!" + symbol));
                WriteIfChanged(DirectPInvokeFile, direct);
            }
            return true;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or Win32Exception)
        {
            Log.LogError("Native export inventory failed: {0}", error.Message);
            return false;
        }
    }

    private string[] ReadExports(string path)
    {
        var output = Run(Tool, ["--defined-only", "--extern-only", "--format=posix", "--no-sort", "--print-file-name", path]);
        return output.Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => ParseSymbol(line.TrimEnd('\r'), path)).OfType<string>().ToArray();
    }

    private string ResolveLibrary(string option)
    {
        if (Compiler.Length == 0 || TargetTriple.Length == 0)
            throw new InvalidDataException("Resolving implicit native libraries requires the selected compiler and target.");
        var arguments = new List<string> { "--target=" + TargetTriple };
        if (SysRoot.Length != 0) arguments.Add("--sysroot=" + SysRoot);
        arguments.Add(option);
        var path = Run(Compiler, arguments).Trim();
        if (!Path.IsPathFullyQualified(path) || !File.Exists(path))
            throw new InvalidDataException($"The selected compiler could not resolve {option}: {path}");
        return path;
    }

    private static string Run(string executable, IEnumerable<string> arguments)
    {
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new IOException($"Could not start {executable}.");
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        process.WaitForExit();
        var diagnostics = error.GetAwaiter().GetResult();
        if (process.ExitCode != 0)
            throw new InvalidDataException($"{executable} failed: {diagnostics}");
        return output.GetAwaiter().GetResult();
    }

    private static string? ParseSymbol(string line, string path)
    {
        var separator = line.LastIndexOf(": ", StringComparison.Ordinal);
        if (!(line.StartsWith(path + ":", StringComparison.Ordinal) || line.StartsWith(path + "[", StringComparison.Ordinal)) || separator < path.Length)
            throw new InvalidDataException($"Unexpected llvm-nm output for {path}: {line}");
        var fields = line[(separator + 2)..].Split(' ', StringSplitOptions.RemoveEmptyEntries);
        if (fields.Length != 4 || fields[1].Length != 1)
            throw new InvalidDataException($"Unexpected llvm-nm symbol record in {path}: {line}");
        if (fields[1] is not ("T" or "W")) return null;
        var symbol = fields[0];
        if (symbol.Any(char.IsWhiteSpace) || symbol.Contains('!'))
            throw new InvalidDataException($"Unsupported native symbol in {path}: {symbol}");
        return symbol;
    }

    private static void WriteIfChanged(string path, IEnumerable<string> lines)
    {
        var contents = string.Join("\n", lines) + "\n";
        if (File.Exists(path) && File.ReadAllText(path) == contents) return;
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        File.WriteAllText(path, contents);
    }
}
