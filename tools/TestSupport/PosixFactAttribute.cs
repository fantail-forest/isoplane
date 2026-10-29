using Xunit;

namespace Isoplane.TestSupport;

/// <summary>Exercises Unix file modes and signals on the platforms that have them.</summary>
public sealed class PosixFactAttribute : FactAttribute
{
    /// <summary>Windows has access control lists and console control events instead.</summary>
    public PosixFactAttribute()
    {
        if (OperatingSystem.IsWindows()) Skip = "Unix file modes and POSIX signals are only applicable on Unix.";
    }
}
