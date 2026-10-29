using System.Runtime.Versioning;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class TemporaryFilePermissionsTests
{
    [PosixFact]
    [UnsupportedOSPlatform("windows")]
    public void A_unix_mode_holds_until_disposal_and_reverts_through_an_exception()
    {
        var directory = new FileSystemTestDirectory("isoplane-temporary-mode-");
        try
        {
            var path = Path.Combine(directory.Path, "file");
            File.WriteAllText(path, "");
            var before = File.GetUnixFileMode(path);
            Assert.NotEqual(UnixFileMode.UserRead, before);
            void FailWhileHeld()
            {
                using var mode = TemporaryFilePermissions.UnixMode(path, UnixFileMode.UserRead);
                Assert.Equal(UnixFileMode.UserRead, File.GetUnixFileMode(path));
                throw new InvalidOperationException("a failed test");
            }
            Assert.Throws<InvalidOperationException>(FailWhileHeld);
            Assert.True(File.GetUnixFileMode(path) == before, "The mode must revert when the exception leaves the using block.");
        }
        finally { directory.Dispose(); }
    }
}
