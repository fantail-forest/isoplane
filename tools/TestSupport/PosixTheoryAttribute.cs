using Xunit;

namespace Isoplane.TestSupport;

/// <summary>Exercises POSIX signals on platforms where the CLI registers them.</summary>
public sealed class PosixTheoryAttribute : TheoryAttribute
{
    /// <summary>Windows uses console control events rather than POSIX signals.</summary>
    public PosixTheoryAttribute()
    {
        if (OperatingSystem.IsWindows()) Skip = "POSIX signal delivery is only applicable on Unix.";
    }
}
