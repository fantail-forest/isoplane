using System.Formats.Tar;
using System.Reflection.Metadata;
using Mono.Debugging.Client;

namespace Isoplane.Debugging;

/// <summary>
/// The source lines an application package's symbols give a statement to. A guest exists only
/// while a request runs, so these say whether a breakpoint will bind before any guest can bind it.
/// </summary>
internal sealed class ApplicationSymbols
{
    private sealed record Document(string Path, byte[] Hash, HashSet<Statement> Statements);

    /// <summary>Where a statement starts and ends in its source file, one-based, the end exclusive.</summary>
    internal readonly record struct Statement(int Line, int Column, int EndLine, int EndColumn)
    {
        internal bool Contains(int line, int column) =>
            (line > Line || line == Line && column >= Column) && (line < EndLine || line == EndLine && column < EndColumn);
    }

    private readonly ILookup<string, Document> documents;

    private ApplicationSymbols(IEnumerable<Document> documents) =>
        this.documents = documents.ToLookup(document => FileName(document.Path), FileNames);

    /// <summary>Symbols that describe no source file.</summary>
    internal static ApplicationSymbols None { get; } = new([]);

    private static StringComparer FileNames => OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal;

    /// <summary>Reads the portable symbols of every assembly in the package archive at <paramref name="package"/>.</summary>
    /// <exception cref="IOException">The archive cannot be read.</exception>
    /// <exception cref="InvalidDataException">The file is not a package archive.</exception>
    internal static ApplicationSymbols Read(string package)
    {
        var documents = new List<Document>();
        using var archive = new FileStream(package, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
        using var reader = new TarReader(archive);
        while (reader.GetNextEntry() is { } entry)
        {
            if (entry.DataStream is null || !entry.Name.EndsWith(".pdb", StringComparison.OrdinalIgnoreCase)) continue;
            var symbols = new MemoryStream();
            entry.DataStream.CopyTo(symbols);
            symbols.Position = 0;
            try { Add(documents, symbols); }
            catch (BadImageFormatException)
            {
                // A dependency's Windows PDB has no portable sequence points; its breakpoints keep the guest's own answer.
            }
        }
        return new(documents);
    }

    private static void Add(List<Document> documents, Stream symbols)
    {
        using var provider = MetadataReaderProvider.FromPortablePdbStream(symbols);
        var reader = provider.GetMetadataReader();
        var statements = new Dictionary<DocumentHandle, HashSet<Statement>>();
        foreach (var handle in reader.MethodDebugInformation)
        {
            foreach (var point in reader.GetMethodDebugInformation(handle).GetSequencePoints())
            {
                if (point.IsHidden || point.Document.IsNil) continue;
                if (!statements.TryGetValue(point.Document, out var starts)) statements.Add(point.Document, starts = []);
                starts.Add(new(point.StartLine, point.StartColumn, point.EndLine, point.EndColumn));
            }
        }
        foreach (var (handle, starts) in statements)
        {
            var document = reader.GetDocument(handle);
            documents.Add(new(reader.GetString(document.Name), reader.GetBlobBytes(document.Hash), starts));
        }
    }

    /// <summary>
    /// Whether a statement starts on <paramref name="line"/> of the source file at <paramref name="path"/>,
    /// at <paramref name="column"/> when it is past the first; null when the symbols do not describe that file.
    /// </summary>
    /// <remarks>
    /// Matches as the debugger does once a guest has loaded the code: a document is the file when it has
    /// its name and either its path or its content, and a breakpoint binds only where a statement starts.
    /// </remarks>
    internal bool? HasStatement(string path, int line, int column)
    {
        if (StatementsOf(path) is not { } statements) return null;
        return statements.Any(start => start.Line == line && (column <= 1 || start.Column == column));
    }

    /// <summary>
    /// The statements that start on <paramref name="line"/> through <paramref name="endLine"/> of the source
    /// file at <paramref name="path"/>, in source order: the places a breakpoint can be set there. Empty when
    /// the symbols do not describe that file.
    /// </summary>
    internal IReadOnlyList<Statement> StatementsStarting(string path, int line, int endLine) =>
        [.. (StatementsOf(path) ?? []).Where(start => start.Line >= line && start.Line <= endLine).Distinct()
            .OrderBy(start => start.Line).ThenBy(start => start.Column)];

    /// <summary>
    /// The statement a breakpoint at <paramref name="column"/> of <paramref name="line"/> means: the innermost
    /// one starting on that line that contains the position, or else the next to start on the line. Null when
    /// there is none, or when the symbols do not describe the file.
    /// </summary>
    /// <remarks>An editor sends the caret's column, which is rarely where a statement starts.</remarks>
    internal Statement? StatementAt(string path, int line, int column)
    {
        var starting = StatementsStarting(path, line, line);
        var containing = starting.Where(statement => statement.Contains(line, column)).ToArray();
        if (containing.Length > 0) return containing.MaxBy(statement => statement.Column);
        return starting.Where(statement => statement.Column > column).Cast<Statement?>().FirstOrDefault();
    }

    internal ApplicationSymbols WithLiveSources(Dictionary<string, HashSet<Statement>> updated, SourcePaths paths) => new(
        documents.SelectMany(group => group).Where(document => !updated.ContainsKey(paths.ToClient(document.Path)))
            .Concat(updated.Select(document => new Document(document.Key, [], document.Value))));

    /// <summary>Every statement of the documents that are the file at <paramref name="path"/>; null when there is no such document.</summary>
    private HashSet<Statement>? StatementsOf(string path)
    {
        HashSet<Statement>? statements = null;
        foreach (var document in documents[FileName(path)])
        {
            if (!FileNames.Equals(document.Path, path) && !SourceLocation.CheckFileHash(path, document.Hash)) continue;
            (statements ??= []).UnionWith(document.Statements);
        }
        return statements;
    }

    /// <summary>The name after the last separator of either platform, since symbols may record another platform's paths.</summary>
    private static string FileName(string path) => path[(path.LastIndexOfAny(['/', '\\']) + 1)..];
}
