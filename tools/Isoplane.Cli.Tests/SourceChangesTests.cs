using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SourceChangesTests
{
    [Theory]
    [InlineData("Application.cs", true)]
    [InlineData("dist/nested/Other.cs", true)]
    [InlineData("dist/nested/customer.wasm", false)]
    [InlineData("dist/nested/customer.wasm.runtime", false)]
    [InlineData("dist/nested/.customer.wasm.publication.tmp", false)]
    [InlineData(".isoplane/publish/build/customer.wasm", false)]
    public void publication_artifacts_do_not_trigger_source_rebuilds(string relativePath, bool expected)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            using var changes = new SourceChanges(directory.FullName);
            changes.SetOutput(Path.Combine(directory.FullName, "dist/nested/customer.wasm"));
            Assert.True(changes.IsSourceFile(Path.Combine(directory.FullName, relativePath)) == expected,
                $"Source edits must trigger rebuilds while publication artifacts must not: {relativePath}.");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
