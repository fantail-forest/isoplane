using System.Runtime.Versioning;
using System.Security.AccessControl;

namespace Isoplane.TestSupport;

/// <summary>
/// Permissions granted on a path for as long as the object lives. Disposing it, including through a
/// failed assertion or an unhandled exception, restores what the path had, so a test leaves nothing it
/// cannot delete. Hold it with <c>using</c>.
/// </summary>
internal sealed class TemporaryFilePermissions : IDisposable
{
    private readonly string path;
    private readonly UnixFileMode? originalUnixMode;
    private readonly FileSystemAccessRule? windowsRule;

    private TemporaryFilePermissions(string path, UnixFileMode? originalUnixMode, FileSystemAccessRule? windowsRule)
    {
        this.path = path;
        this.originalUnixMode = originalUnixMode;
        this.windowsRule = windowsRule;
    }

    /// <summary>Sets <paramref name="path"/>'s mode, as chmod would, until disposal.</summary>
    [UnsupportedOSPlatform("windows")]
    internal static TemporaryFilePermissions UnixMode(string path, UnixFileMode mode)
    {
        var original = File.GetUnixFileMode(path);
        File.SetUnixFileMode(path, mode);
        return new(path, original, null);
    }

    /// <summary>Adds <paramref name="rule"/> to <paramref name="path"/>'s access control list until disposal.</summary>
    [SupportedOSPlatform("windows")]
    internal static TemporaryFilePermissions WindowsAccessRule(string path, FileSystemAccessRule rule)
    {
        var file = new FileInfo(path);
        var security = file.GetAccessControl();
        security.AddAccessRule(rule);
        file.SetAccessControl(security);
        return new(path, null, rule);
    }

    public void Dispose()
    {
        if (OperatingSystem.IsWindows())
        {
            if (windowsRule is null) return;
            var file = new FileInfo(path);
            var security = file.GetAccessControl();
            security.RemoveAccessRule(windowsRule);
            file.SetAccessControl(security);
        }
        else if (originalUnixMode is { } mode) File.SetUnixFileMode(path, mode);
    }
}
