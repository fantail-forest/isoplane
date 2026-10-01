namespace Isoplane.TestSupport;

/// <summary>Disposable test data requiring native filesystem semantics, including OS certificate storage.</summary>
internal sealed class FileSystemTestDirectory : IDisposable
{
    internal string Path { get; }

    internal FileSystemTestDirectory(string prefix)
    {
        // Repository scratch can be a share lacking Windows ACLs or macOS keychain locking.
        var nativeTemp = OperatingSystem.IsWindows()
            ? System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Temp")
            : OperatingSystem.IsMacOS() ? "/tmp" : System.IO.Path.GetTempPath();
        var path = System.IO.Path.Combine(nativeTemp, prefix + Guid.NewGuid().ToString("N"));
        Path = OperatingSystem.IsWindows()
            ? Directory.CreateDirectory(path).FullName
            : Directory.CreateDirectory(path, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute).FullName;
    }

    public void Dispose() => Directory.Delete(Path, recursive: true);
}
