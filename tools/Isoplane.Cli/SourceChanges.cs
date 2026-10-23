using System.Security.Cryptography;
using System.Threading.Channels;

namespace Isoplane.Cli;

internal sealed class SourceChanges : IDisposable
{
    private readonly Func<CancellationToken, Task<SourceInputs>> discover;
    private readonly Func<string, FileSystemWatcher> createWatcher;
    private readonly Dictionary<string, FileSystemWatcher> watchers = new(PathComparer);
    private HashSet<string> files;
    private string[] outputDirectories;
    private Dictionary<string, string> snapshot;
    private int sourceChanged;
    private int discoveryNeeded;

    private static StringComparer PathComparer => SourceInputs.PathComparer;

    private readonly Channel<bool> notifications = Channel.CreateBounded<bool>(new BoundedChannelOptions(1)
    {
        FullMode = BoundedChannelFullMode.DropOldest,
        SingleReader = true
    });

    internal static async Task<SourceChanges> CreateAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        Task<SourceInputs> Discover(CancellationToken token) => SourceInputs.ReadAsync(options, token);
        return new(await Discover(cancellation), Discover);
    }

    internal SourceChanges(SourceInputs inputs, Func<CancellationToken, Task<SourceInputs>> discover,
        Func<string, FileSystemWatcher>? createWatcher = null)
    {
        this.discover = discover;
        this.createWatcher = createWatcher ?? (directory => new FileSystemWatcher(directory));
        files = inputs.Files.ToHashSet(PathComparer);
        outputDirectories = inputs.OutputDirectories;
        try
        {
            UpdateWatchers();
            snapshot = Snapshot();
        }
        catch
        {
            Dispose();
            throw;
        }
    }

    private void UpdateWatchers()
    {
        var directories = files.Select(Path.GetDirectoryName).OfType<string>().Where(Directory.Exists).ToHashSet(PathComparer);
        var projects = files.Where(path => Path.GetExtension(path).Equals(".csproj", StringComparison.OrdinalIgnoreCase))
            .Select(Path.GetDirectoryName).OfType<string>().ToHashSet(PathComparer);
        foreach (var directory in watchers.Keys.Where(directory => !directories.Contains(directory)).ToArray())
        {
            watchers[directory].Dispose();
            watchers.Remove(directory);
        }
        foreach (var directory in directories)
        {
            if (watchers.TryGetValue(directory, out var existing))
            {
                existing.IncludeSubdirectories = projects.Contains(directory);
                continue;
            }
            var watcher = createWatcher(directory);
            watchers.Add(directory, watcher);
            watcher.IncludeSubdirectories = projects.Contains(directory);
            watcher.Changed += (_, e) => Observe(e.FullPath, structural: false);
            watcher.Created += (_, e) => Observe(e.FullPath, structural: true);
            watcher.Deleted += (_, e) => Observe(e.FullPath, structural: true);
            watcher.Renamed += (_, e) => { Observe(e.OldFullPath, structural: true); Observe(e.FullPath, structural: true); };
            watcher.Error += (_, e) =>
            {
                if (e.GetException() is InternalBufferOverflowException)
                {
                    Interlocked.Exchange(ref discoveryNeeded, 1);
                    notifications.Writer.TryWrite(true);
                }
                else
                    notifications.Writer.TryComplete(new IOException($"Could not watch source files in '{watcher.Path}': {e.GetException().Message}", e.GetException()));
            };
            watcher.EnableRaisingEvents = true;
        }
    }

    private void Observe(string path, bool structural)
    {
        if (IsSourceFile(path))
        {
            Interlocked.Exchange(ref sourceChanged, 1);
            if (new[] { ".csproj", ".props", ".targets" }.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase))
                Interlocked.Exchange(ref discoveryNeeded, 1);
            notifications.Writer.TryWrite(true);
        }
        else if (structural && !IsBuildOutput(path))
        {
            Interlocked.Exchange(ref discoveryNeeded, 1);
            notifications.Writer.TryWrite(true);
        }
    }

    internal bool IsSourceFile(string path) => Volatile.Read(ref files).Contains(path);

    private bool IsBuildOutput(string path) => Volatile.Read(ref outputDirectories).Any(directory =>
        PathComparer.Equals(path, directory) || path.StartsWith(Path.TrimEndingDirectorySeparator(directory) + Path.DirectorySeparatorChar,
            OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal));

    private Dictionary<string, string> Snapshot()
    {
        var result = new Dictionary<string, string>(PathComparer);
        foreach (var path in files)
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
                await notifications.Reader.ReadAsync(cancellation);
                var sourceEvent = Interlocked.Exchange(ref sourceChanged, 0) != 0;
                if (Interlocked.Exchange(ref discoveryNeeded, 0) != 0)
                {
                    try
                    {
                        var inputs = await discover(cancellation);
                        Volatile.Write(ref files, inputs.Files.ToHashSet(PathComparer));
                        Volatile.Write(ref outputDirectories, inputs.OutputDirectories);
                        UpdateWatchers();
                    }
                    catch (InvalidOperationException error)
                    {
                        // Keep the last graph available so fixing a malformed project retries its build.
                        Console.Error.WriteLine(error.Message);
                    }
                }
                var current = Snapshot();
                var changed = current.Count != snapshot.Count || current.Any(entry => !snapshot.TryGetValue(entry.Key, out var previous) || previous != entry.Value);
                snapshot = current;
                if (sourceEvent || changed) return;
            }
        }
        catch (ChannelClosedException error) when (error.InnerException is not null)
        {
            throw error.InnerException;
        }
    }

    public void Dispose()
    {
        foreach (var watcher in watchers.Values) watcher.Dispose();
    }
}
