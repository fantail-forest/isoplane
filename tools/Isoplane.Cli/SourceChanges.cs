using System.Threading.Channels;
using System.Security.Cryptography;

namespace Isoplane.Cli;

internal sealed class SourceChanges : IDisposable
{
    private readonly FileSystemWatcher watcher;
    private Dictionary<string, string> snapshot;
    private string? output;

    internal void SetOutput(string module) => Volatile.Write(ref output, Path.GetFullPath(module));
    private readonly Channel<bool> changes = Channel.CreateBounded<bool>(new BoundedChannelOptions(1)
    {
        FullMode = BoundedChannelFullMode.DropOldest,
        SingleReader = true
    });

    internal SourceChanges(string directory) : this(new FileSystemWatcher(directory)) { }

    internal SourceChanges(FileSystemWatcher watcher)
    {
        this.watcher = watcher;
        watcher.IncludeSubdirectories = true;
        watcher.Changed += (_, e) => Observe(e.FullPath);
        watcher.Created += (_, e) => Observe(e.FullPath);
        watcher.Deleted += (_, e) => Observe(e.FullPath);
        watcher.Renamed += (_, e) => { Observe(e.OldFullPath); Observe(e.FullPath); };
        watcher.Error += (_, e) =>
        {
            if (e.GetException() is InternalBufferOverflowException)
                changes.Writer.TryWrite(false);
            else
                changes.Writer.TryComplete(e.GetException());
        };
        watcher.EnableRaisingEvents = true;
        snapshot = Snapshot();
    }

    private void Observe(string path)
    {
        if (IsSourceFile(path))
            changes.Writer.TryWrite(true);
    }

    internal bool IsSourceFile(string path)
    {
        var module = Volatile.Read(ref output);
        var fullPath = Path.GetFullPath(path);
        if (fullPath == module || fullPath == module + ".runtime" || Directory.Exists(path))
            return false;
        var relative = Path.GetRelativePath(watcher.Path, path);
        return !relative.Split(Path.DirectorySeparatorChar).Any(Excluded);
    }

    private static bool Excluded(string part) => part.StartsWith('.') || part is "bin" or "obj" or "out" or "node_modules" or "target";

    private IEnumerable<string> SourceFiles(string directory)
    {
        foreach (var path in Directory.EnumerateFileSystemEntries(directory))
        {
            if (Excluded(Path.GetFileName(path))) continue;
            if (Directory.Exists(path))
            {
                if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0) continue;
                foreach (var file in SourceFiles(path)) yield return file;
            }
            else if (IsSourceFile(path)) yield return path;
        }
    }

    private Dictionary<string, string> Snapshot()
    {
        var result = new Dictionary<string, string>();
        foreach (var path in SourceFiles(watcher.Path))
        {
            try
            {
                using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                result.Add(path, Convert.ToHexString(SHA256.HashData(stream)));
            }
            catch (FileNotFoundException) { /* A removal is represented by the missing entry. */ }
            catch (DirectoryNotFoundException) { /* A removed parent also removes this entry. */ }
        }
        return result;
    }

    internal async Task WaitAsync(CancellationToken cancellation)
    {
        try
        {
            while (true)
            {
                var sourceEvent = await changes.Reader.ReadAsync(cancellation);
                var current = Snapshot();
                var changed = current.Count != snapshot.Count || current.Any(entry => !snapshot.TryGetValue(entry.Key, out var previous) || previous != entry.Value);
                snapshot = current;
                if (sourceEvent || changed) return;
            }
        }
        catch (ChannelClosedException error) when (error.InnerException is not null)
        {
            throw new IOException($"Could not watch source files in '{watcher.Path}': {error.InnerException.Message}", error.InnerException);
        }
    }

    public void Dispose() => watcher.Dispose();
}
