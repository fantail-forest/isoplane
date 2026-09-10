using System;
using System.Globalization;
using System.IO;
using System.Text;
using Microsoft.Build.Framework;

/// <summary>Embeds the published application's portable symbols and owned debugger endpoint.</summary>
public sealed class WispMonoDebugSymbols : Microsoft.Build.Utilities.Task
{
    /// <summary>The matching post-trim portable PDB.</summary>
    [Required] public string Symbols { get; set; } = "";
    /// <summary>The session-owned IPv4 loopback peer.</summary>
    [Required] public string Endpoint { get; set; } = "";
    /// <summary>The generated native header.</summary>
    [Required] public string Output { get; set; } = "";

    /// <summary>Validates the local endpoint and portable symbols before emitting native data.</summary>
    public override bool Execute()
    {
        const string prefix = "127.0.0.1:";
        int port;
        if (!Endpoint.StartsWith(prefix, StringComparison.Ordinal)
            || !int.TryParse(Endpoint.Substring(prefix.Length), NumberStyles.None, CultureInfo.InvariantCulture, out port)
            || port < 1 || port > 65535)
        {
            Log.LogError("wisp: local debugging requires the session-owned endpoint 127.0.0.1:<port>, with port 1..65535.");
            return false;
        }
        if (!File.Exists(Symbols))
        {
            Log.LogError("wisp: matching post-trim portable PDB is missing: {0}", Symbols);
            return false;
        }
        var bytes = File.ReadAllBytes(Symbols);
        if (bytes.Length < 4 || bytes[0] != 'B' || bytes[1] != 'S' || bytes[2] != 'J' || bytes[3] != 'B')
        {
            Log.LogError("wisp: local debugging requires a portable PDB: {0}", Symbols);
            return false;
        }
        var header = new StringBuilder();
        header.Append("#define WISP_DEBUG_OPTIONS \"--debugger-agent=transport=dt_socket,address=");
        header.Append(Endpoint).AppendLine(",server=n,suspend=y\"");
        header.AppendLine("static const unsigned char wisp_debug_symbols[] = {");
        foreach (var value in bytes) header.Append(value.ToString(CultureInfo.InvariantCulture)).Append(',');
        header.AppendLine("};");
        File.WriteAllText(Output, header.ToString());
        return true;
    }
}
