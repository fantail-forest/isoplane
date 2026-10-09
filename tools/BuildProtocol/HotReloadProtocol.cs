using System.Text.Json;

namespace Isoplane.Build;

internal sealed record CompilerStart(string SdkDirectory, string Project, Dictionary<string, string> Properties,
    Dictionary<string, string> AssemblyPaths, string[] Capabilities);
internal sealed record ActiveStatement(Guid Module, int MethodToken, int MethodVersion, int IlOffset,
    string Document, int StartLine, int StartColumn, int EndLine, int EndColumn, bool Leaf, bool UpToDate);
internal sealed record SourceUpdate(string Path, string Text);
internal sealed record CompilerEdit(ActiveStatement[] ActiveStatements, Guid[] LoadedModules, string[] Capabilities, SourceUpdate[] Sources);
internal sealed record CompilerCommand(string Action, CompilerEdit? Edit = null);
internal sealed record SourceSpan(int StartLine, int StartColumn, int EndLine, int EndColumn);
internal sealed record ActiveStatementUpdate(int MethodToken, int MethodVersion, int IlOffset, SourceSpan NewSpan);
internal sealed record SourceLineUpdate(int OldLine, int NewLine);
internal sealed record SequencePointUpdate(string FileName, SourceLineUpdate[] Lines);
internal sealed record ModuleUpdate(Guid Module, byte[] Metadata, byte[] Il, byte[] Symbols)
{
    public int[] UpdatedTypes { get; init; } = [];
    public ActiveStatementUpdate[] ActiveStatements { get; init; } = [];
    public SequencePointUpdate[] SequencePoints { get; init; } = [];
}
internal sealed record CompilerReply(ModuleUpdate[] Updates, string[] Diagnostics, bool RestartRequired, string? Error = null)
{
    public bool Pending { get; init; }
    public string[] Documents { get; init; } = [];
}

internal static class HotReloadProtocol
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    internal static async Task WriteAsync<T>(TextWriter writer, T value)
    {
        await writer.WriteLineAsync(JsonSerializer.Serialize(value, Json));
        await writer.FlushAsync();
    }

    internal static async Task<T?> ReadAsync<T>(TextReader reader) where T : class
    {
        string? line = await reader.ReadLineAsync();
        return line is null ? null : JsonSerializer.Deserialize<T>(line, Json)
            ?? throw new IOException("The Hot Reload compiler message was empty.");
    }
}
