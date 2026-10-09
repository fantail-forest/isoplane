using System.Collections;
using System.Collections.Immutable;
using System.Reflection;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Text;

namespace Isoplane.HotReload;

internal static class TrackedStatements
{
    internal static async Task<Delegate> CreateAsync(Type provider, Solution baseline, Solution updated,
        ImmutableArray<DocumentId> documents, object[] spans)
    {
        var spanType = provider.GetMethod("Invoke")!.ReturnType.GetGenericArguments()[0].GetGenericArguments()[0];
        var create = typeof(TrackedStatements).GetMethod(nameof(CreateTypedAsync), BindingFlags.Static | BindingFlags.NonPublic)!
            .MakeGenericMethod(spanType);
        return await (Task<Delegate>)create.Invoke(null, [provider, baseline, updated, documents, spans])!;
    }

    private static async Task<Delegate> CreateTypedAsync<T>(Type provider, Solution baseline, Solution updated,
        ImmutableArray<DocumentId> documents, object[] spans)
    {
        var tracked = new Dictionary<DocumentId, ImmutableArray<T>>();
        for (int index = 0; index < documents.Length; index++)
        {
            var originalSpans = ((IEnumerable)spans[index]).Cast<T>().ToArray();
            if (originalSpans.Length == 0) continue;
            var original = baseline.GetDocument(documents[index])!;
            var changed = updated.GetDocument(documents[index]);
            if (changed is null) continue;
            var oldTree = await original.GetSyntaxTreeAsync();
            var newTree = await changed.GetSyntaxTreeAsync();
            if (oldTree is null || newTree is null) continue;
            var oldText = await oldTree.GetTextAsync();
            var newText = await newTree.GetTextAsync();
            var changes = newTree.GetChanges(oldTree).OrderBy(change => change.Span.Start).ToArray();
            var values = ImmutableArray.CreateBuilder<T>();
            foreach (var span in originalSpans)
            {
                var lineSpan = (LinePositionSpan)Property(span!, "LineSpan");
                // Mapped #line documents do not necessarily describe offsets in this syntax tree.
                if (lineSpan.Start.Line >= oldText.Lines.Count || lineSpan.End.Line >= oldText.Lines.Count) continue;
                var oldSpan = oldText.Lines.GetTextSpan(lineSpan);
                var start = Track(oldSpan.Start, changes, false);
                var end = Track(oldSpan.End, changes, true);
                var mapped = newText.Lines.GetLinePositionSpan(TextSpan.FromBounds(start, end));
                values.Add((T)Activator.CreateInstance(typeof(T), Property(span!, "Id"), mapped,
                    Property(span!, "Flags"), typeof(T).GetProperty("UnmappedDocumentId")!.GetValue(span))!);
            }
            tracked.Add(documents[index], values.ToImmutable());
        }
        ValueTask<ImmutableArray<T>> GetSpans(DocumentId? document, string path, CancellationToken cancellation) =>
            new(document is not null && tracked.TryGetValue(document, out var values) ? values : []);
        Func<DocumentId?, string, CancellationToken, ValueTask<ImmutableArray<T>>> callback = GetSpans;
        return Delegate.CreateDelegate(provider, callback.Target, callback.Method);
    }

    private static int Track(int position, TextChange[] changes, bool end)
    {
        int displacement = 0;
        foreach (var change in changes)
        {
            if (change.Span.Start > position) break;
            int length = change.NewText?.Length ?? 0;
            if (change.Span.End <= position) displacement += length - change.Span.Length;
            else return change.Span.Start + displacement + (end ? length : 0);
        }
        return position + displacement;
    }

    private static object Property(object value, string name) => value.GetType().GetProperty(name)?.GetValue(value)
        ?? throw new NotSupportedException($"The SDK's active statement contract has no '{name}' property.");
}
