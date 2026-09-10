using System;
using System.IO;
using System.Security.Cryptography;
using System.Xml;
using System.Text;
using Microsoft.Build.Framework;

/// <summary>Records the exact module and symbols delivered to a local debug session.</summary>
public sealed class WispMonoDebugManifest : Microsoft.Build.Utilities.Task
{
    private const string LocalDebugSection = "wisp.local-debug";

    /// <summary>The final raw guest module.</summary>
    [Required] public string Module { get; set; } = "";
    /// <summary>The copied post-trim portable PDB.</summary>
    [Required] public string Symbols { get; set; } = "";
    /// <summary>The validated session-owned debugger peer.</summary>
    [Required] public string Endpoint { get; set; } = "";
    /// <summary>The output XML manifest.</summary>
    [Required] public string Output { get; set; } = "";

    /// <summary>Writes file names and SHA256 digests for the CLI to verify before launch.</summary>
    public override bool Execute()
    {
        var name = Encoding.UTF8.GetBytes(LocalDebugSection);
        using (var module = File.Open(Module, FileMode.Append, FileAccess.Write))
        {
            module.WriteByte(0);
            module.WriteByte(checked((byte)(name.Length + 1)));
            module.WriteByte(checked((byte)name.Length));
            module.Write(name, 0, name.Length);
        }
        using (var writer = XmlWriter.Create(Output, new XmlWriterSettings { Indent = true }))
        {
            writer.WriteStartElement("LocalDebug");
            writer.WriteAttributeString("endpoint", Endpoint);
            WriteFile(writer, "Module", Module);
            WriteFile(writer, "Symbols", Symbols);
            writer.WriteEndElement();
        }
        return true;
    }

    private static void WriteFile(XmlWriter writer, string element, string path)
    {
        using (var sha = SHA256.Create())
        using (var input = File.OpenRead(path))
        {
            writer.WriteStartElement(element);
            writer.WriteAttributeString("file", Path.GetFileName(path));
            writer.WriteAttributeString("sha256", BitConverter.ToString(sha.ComputeHash(input)).Replace("-", "").ToLowerInvariant());
            writer.WriteEndElement();
        }
    }
}
