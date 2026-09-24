using System;
using System.Globalization;
using System.Collections.Generic;
using System.IO;
using System.Text;
using Microsoft.Build.Framework;

/// <summary>Embeds the published assemblies' portable symbols.</summary>
public sealed class IsoplaneMonoDebugSymbols : Microsoft.Build.Utilities.Task
{
    /// <summary>The portable PDB selected for application publication.</summary>
    [Required] public string Symbols { get; set; } = "";
    /// <summary>Portable PDBs selected for referenced assemblies in the same publication.</summary>
    public ITaskItem[] DependencySymbols { get; set; } = Array.Empty<ITaskItem>();
    /// <summary>The generated native header.</summary>
    [Required] public string Output { get; set; } = "";

    /// <summary>Validates the portable symbols before emitting native data.</summary>
    public override bool Execute()
    {
        byte[] bytes;
        if (!ReadSymbols(Symbols, out bytes)) return false;
        var header = new StringBuilder();
        AppendData(header, "isoplane_debug_symbols", bytes);
        var registrations = new StringBuilder();
        registrations.AppendLine("mono_wasm_add_assembly(ISOPLANE_MONO_ASSEMBLY \".pdb\", isoplane_debug_symbols, sizeof(isoplane_debug_symbols));");
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { Path.GetFileName(Symbols) };
        var dependencies = (ITaskItem[])DependencySymbols.Clone();
        Array.Sort(dependencies, (left, right) => StringComparer.OrdinalIgnoreCase.Compare(Path.GetFileName(left.ItemSpec), Path.GetFileName(right.ItemSpec)));
        for (int index = 0; index < dependencies.Length; index++)
        {
            string path = dependencies[index].ItemSpec;
            string name = Path.GetFileName(path);
            if (!names.Add(name))
            {
                Log.LogError("isoplane: debug symbol names must be unique within the publication: {0}", name);
                return false;
            }
            if (!ReadSymbols(path, out bytes)) return false;
            string identifier = "isoplane_debug_dependency_" + index.ToString(CultureInfo.InvariantCulture);
            AppendData(header, identifier, bytes);
            registrations.Append("mono_wasm_add_assembly(").Append(CString(name)).Append(", ").Append(identifier)
                .Append(", sizeof(").Append(identifier).AppendLine("));");
        }
        header.AppendLine("static void isoplane_register_debug_symbols(void) {").Append(registrations).AppendLine("}");
        File.WriteAllText(Output, header.ToString());
        return true;
    }

    private bool ReadSymbols(string path, out byte[] bytes)
    {
        bytes = Array.Empty<byte>();
        if (!File.Exists(path))
        {
            Log.LogError("isoplane: matching application portable PDB is missing: {0}", path);
            return false;
        }
        bytes = File.ReadAllBytes(path);
        if (bytes.Length < 4 || bytes[0] != 'B' || bytes[1] != 'S' || bytes[2] != 'J' || bytes[3] != 'B')
        {
            Log.LogError("isoplane: local debugging requires a portable PDB: {0}", path);
            return false;
        }
        return true;
    }

    private static void AppendData(StringBuilder header, string identifier, byte[] bytes)
    {
        header.Append("static const unsigned char ").Append(identifier).AppendLine("[] = {");
        foreach (byte value in bytes) header.Append(value.ToString(CultureInfo.InvariantCulture)).Append(',');
        header.AppendLine("};");
    }

    private static string CString(string text)
    {
        var literal = new StringBuilder("\"");
        foreach (byte value in Encoding.UTF8.GetBytes(text)) literal.Append('\\').Append(Convert.ToString(value, 8).PadLeft(3, '0'));
        return literal.Append('"').ToString();
    }
}
