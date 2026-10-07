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
    private sealed record Document(string Path, byte[] Hash, HashSet<(int Line, int Column)> Statements);

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
        using var archive = File.OpenRead(package);
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
        var statements = new Dictionary<DocumentHandle, HashSet<(int, int)>>();
        foreach (var handle in reader.MethodDebugInformation)
        {
            foreach (var point in reader.GetMethodDebugInformation(handle).GetSequencePoints())
            {
                if (point.IsHidden || point.Document.IsNil) continue;
                if (!statements.TryGetValue(point.Document, out var starts)) statements.Add(point.Document, starts = []);
                starts.Add((point.StartLine, point.StartColumn));
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
        bool described = false;
        foreach (var document in documents[FileName(path)])
        {
            if (!FileNames.Equals(document.Path, path) && !SourceLocation.CheckFileHash(path, document.Hash)) continue;
            if (document.Statements.Any(start => start.Line == line && (column <= 1 || start.Column == column))) return true;
            described = true;
        }
        return described ? false : null;
    }

    /// <summary>The name after the last separator of either platform, since symbols may record another platform's paths.</summary>
    private static string FileName(string path) => path[(path.LastIndexOfAny(['/', '\\']) + 1)..];
}
