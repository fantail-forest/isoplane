using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using Isoplane.Native;

namespace Isoplane.Cli;

internal static class ModuleDoctor
{
    internal static int Run(string[] arguments)
    {
        string? module = null;
        ulong? memoryBytes = null;
        var json = false;
        for (var index = 1; index < arguments.Length; index++)
        {
            var argument = arguments[index];
            switch (argument)
            {
                case "--json": json = true; break;
                case "--no-color": break;
                case "--guest-memory-bytes":
                    if (!ulong.TryParse(Value(arguments, ref index), NumberStyles.None, CultureInfo.InvariantCulture, out var bytes))
                        throw new ArgumentException("--guest-memory-bytes requires a positive integer byte count.");
                    memoryBytes = bytes;
                    break;
                default:
                    if (argument.StartsWith('-') || module is not null)
                        throw new ArgumentException($"Unexpected module doctor argument: {argument}");
                    module = Path.GetFullPath(argument);
                    break;
            }
        }
        if (module is null) throw new ArgumentException("Usage: isoplane doctor MODULE [--guest-memory-bytes BYTES] [--json]");
        var policy = ResolvePolicy(memoryBytes);
        var report = ModuleValidator.Validate(File.ReadAllBytes(module), JsonSerializer.SerializeToUtf8Bytes(policy.Settings));
        report["limits_from"] = policy.LimitsFrom;
        report["guest_memory_from"] = policy.MemoryFrom;
        if (json) Console.WriteLine(report.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));
        else Render(module, report);
        return report["rejected"]!.GetValue<bool>() ? 2 : 0;
    }

    private static string Value(string[] arguments, ref int index)
    {
        if (++index == arguments.Length) throw new ArgumentException($"{arguments[index - 1]} requires a value.");
        return arguments[index];
    }

    /// <summary>
    /// The validation policy for a module: numeric budgets and the memory ceiling the flags
    /// select, and otherwise the platform's defaults.
    /// </summary>
    internal static (Dictionary<string, object> Settings, string LimitsFrom, string MemoryFrom) ResolvePolicy(ulong? memoryBytes)
    {
        Dictionary<string, object> settings = [];
        if (memoryBytes is not null) settings["guest_memory_bytes"] = memoryBytes.Value;
        return (settings, "the default", memoryBytes is null ? "limits" : "--guest-memory-bytes");
    }

    private static void Render(string module, JsonObject report)
    {
        Console.WriteLine($"isoplane doctor {module}\n  module {report["bytes"]} bytes  blake3 {report["blake3"]}");
        Console.WriteLine("\nCAPABILITY MANIFEST");
        foreach (var capability in report["capabilities"]!.AsArray())
        {
            Console.WriteLine($"  {capability!["group"]}  {capability["name"]}  {capability["kind"]}  {capability["note"]}");
            if (capability["sensitive"]!.GetValue<bool>()) Console.WriteLine("    sensitive");
            if (capability["init_visible"]!.GetValue<bool>()) Console.WriteLine("    init-visible");
        }
        if (report["pal"]!.AsArray().Count > 0) Console.WriteLine("\nWASI PAL ADAPTER");
        foreach (var entry in report["pal"]!.AsArray())
            Console.WriteLine($"  {entry!["name"]}: " + (entry["delegates_to"] is { } capability
                ? $"delegates to isoplane.{capability}"
                : entry["traps"]!.GetValue<bool>() ? "stub: traps when called" : "returns the empty value"));
        Console.WriteLine("\nSO: THIS CODE CAN");
        foreach (var effect in report["effects"]!.AsArray()) Console.WriteLine($"  • {effect}");
        Console.WriteLine("\nEXPORTS");
        var present = report["exports"]!.AsArray().Select(entry => entry!.GetValue<string>()).ToHashSet(StringComparer.Ordinal);
        foreach (var export in report["required_exports"]!.AsArray())
            Console.WriteLine($"  {(present.Contains(export!.GetValue<string>()) ? "✓" : "✗")} {export}");
        foreach (var export in report["optional_exports"]!.AsArray())
            Console.WriteLine($"  {(present.Contains(export!.GetValue<string>()) ? "✓" : "·")} {export} (optional)");
        Console.WriteLine($"\nVALIDATOR (isoplane-runtime)\n  numeric limits: {report["limits"]} ({report["limits_from"]})");
        Console.WriteLine($"  guest memory ceiling: {report["guest_memory_bytes"]} bytes ({report["guest_memory_from"]})");
        foreach (var violation in report["violations"]!.AsArray())
            Console.WriteLine($"  ✗ {violation!["code"]} {violation["subject"]} — {violation["detail"]}");
        Console.WriteLine(report["rejected"]!.GetValue<bool>() ? "  REJECTED" : "  ✓ no violations");
    }
}
