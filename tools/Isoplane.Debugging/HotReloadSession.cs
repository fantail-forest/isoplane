using Isoplane.Build;
using Mono.Debugger.Soft;
using SourceUpdate = Isoplane.Build.SourceUpdate;

namespace Isoplane.Debugging;

/// <summary>Owns the compiler baseline and applies edits to one guest invocation.</summary>
internal sealed class HotReloadSession : IAsyncDisposable
{
    private readonly HotReloadBaseline baseline;
    private readonly HotReloadCompiler compiler;

    internal string[] Documents => compiler.Documents;

    private HotReloadSession(HotReloadBaseline baseline, HotReloadCompiler compiler)
    {
        this.baseline = baseline;
        this.compiler = compiler;
    }

    internal static async Task<HotReloadSession> StartAsync(DebugProject project, string package, CancellationToken cancellation)
    {
        var baseline = HotReloadBaseline.Read(package);
        try
        {
            var compiler = await HotReloadCompiler.StartAsync(project.Path, project.Properties, baseline.Assemblies, [], cancellation);
            return new(baseline, compiler);
        }
        catch
        {
            baseline.Dispose();
            throw;
        }
    }

    internal async Task<CompilerReply> ApplyAsync(VirtualMachine machine, SourceUpdate[] sources, SourcePaths paths, CancellationToken cancellation)
    {
        var comparison = OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal;
        foreach (var source in sources)
            if (!Documents.Contains(source.Path, comparison))
                throw new ArgumentException($"Source '{source.Path}' is not part of the paused project's compilation.");
        var modules = machine.RootDomain.GetAssemblies()
            .Where(assembly => baseline.Assemblies.ContainsKey(assembly.GetName().Name ?? ""))
            .Select(assembly => assembly.ManifestModule).ToDictionary(module => module.ModuleVersionId);
        var active = new List<ActiveStatement>();
        foreach (var thread in machine.GetThreads())
        {
            var frames = thread.GetFrames();
            for (int index = 0; index < frames.Length; index++)
            {
                var frame = frames[index];
                if (!baseline.Assemblies.ContainsKey(frame.Method.DeclaringType.Assembly.GetName().Name ?? "")) continue;
                var location = frame.Location;
                if (frame.ILOffset < 0 || string.IsNullOrEmpty(location.SourceFile) || location.LineNumber is <= 0 or 0xfeefee) continue;
                active.Add(new(frame.Method.DeclaringType.Assembly.ManifestModule.ModuleVersionId,
                    frame.Method.MetadataToken, frame.MethodVersion, frame.ILOffset, paths.ToClient(location.SourceFile),
                    location.LineNumber - 1, location.ColumnNumber - 1, location.EndLineNumber - 1, location.EndColumnNumber - 1,
                    index == 0, frame.IsCurrentMethodVersion));
            }
        }
        var edit = new CompilerEdit([.. active], [.. modules.Keys],
            machine.GetEnCCapabilities().Split(' ', StringSplitOptions.RemoveEmptyEntries), sources);
        var reply = await compiler.SendAsync(new("update", edit), cancellation);
        if (!reply.Pending) return reply;
        if (reply.RestartRequired || reply.Diagnostics.Length != 0)
        {
            await compiler.SendAsync(new("discard"), cancellation);
            return reply;
        }
        if (reply.Updates.Any(update => !modules.ContainsKey(update.Module)))
        {
            await compiler.SendAsync(new("discard"), cancellation);
            throw new IOException("The compiler emitted an update for a module that is not loaded in this guest.");
        }
        try
        {
            foreach (var update in reply.Updates)
                modules[update.Module].ApplyChanges(update.Metadata, update.Il, update.Symbols,
                    update.SequencePoints.Select(sequence =>
                    {
                        var changes = new Mono.Debugger.Soft.SourceUpdate(sequence.FileName);
                        changes.LineUpdates.AddRange(sequence.Lines.Select(line => Tuple.Create(line.OldLine, line.NewLine)));
                        return changes;
                    }));
            await compiler.SendAsync(new("commit"), cancellation);
        }
        catch (Exception error)
        {
            throw new HotReloadApplicationException("The guest could not complete Hot Reload. Restart debugging to restore a consistent compiler and runtime baseline.", error);
        }
        return reply;
    }

    public async ValueTask DisposeAsync()
    {
        try { await compiler.DisposeAsync(); }
        finally { baseline.Dispose(); }
    }
}

internal sealed class HotReloadApplicationException(string message, Exception inner) : Exception(message, inner);
