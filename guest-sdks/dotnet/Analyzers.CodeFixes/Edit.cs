using System;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Editing;
using Microsoft.CodeAnalysis.Simplification;

namespace Isoplane.Analyzers;

internal static class Edit
{
    /// <summary>
    /// Applies <paramref name="change"/> to <paramref name="document"/> and imports the namespaces of the
    /// types it wrote, so a fix reads <c>Task</c> where the file had no such import.
    /// </summary>
    internal static async Task<Document> ApplyAsync(Document document, Action<DocumentEditor> change, CancellationToken cancellation)
    {
        var editor = await DocumentEditor.CreateAsync(document, cancellation).ConfigureAwait(false);
        change(editor);
        // The generator writes a type by its full name and marks it for simplifying; the import makes the short name possible.
        return await ImportAdder.AddImportsAsync(editor.GetChangedDocument(), Simplifier.Annotation, cancellationToken: cancellation).ConfigureAwait(false);
    }
}
