using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class ApplicationSymbolsTests : IDisposable
{
    private const string Source = """
        public static class Example
        {
            public static int Run(int value)
            {
                // A comment has no statement.
                int doubled = value * 2; int tripled = value * 3;
                return doubled + tripled;
            }
        }
        """;

    private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-application-symbols-");
    private string Package => Path.Combine(directory.FullName, "application.tar");
    private string File(string name) => Path.Combine(directory.FullName, name);

    public void Dispose() => directory.Delete(recursive: true);

    [Fact]
    public void a_line_is_verified_only_where_a_statement_starts()
    {
        BuiltApplication.Write(Package, File("Example.cs"), Source);
        var symbols = ApplicationSymbols.Read(Package);
        Assert.Equal(true, symbols.HasStatement(File("Example.cs"), 6, 1));
        Assert.Equal(true, symbols.HasStatement(File("Example.cs"), 7, 1));
        Assert.Equal(false, symbols.HasStatement(File("Example.cs"), 5, 1));
        Assert.Equal(false, symbols.HasStatement(File("Example.cs"), 40, 1));
    }

    [Fact]
    public void a_column_selects_the_statement_that_starts_there()
    {
        BuiltApplication.Write(Package, File("Example.cs"), Source);
        var symbols = ApplicationSymbols.Read(Package);
        int second = Source.Split('\n')[5].IndexOf("int tripled", StringComparison.Ordinal) + 1;
        Assert.Equal(true, symbols.HasStatement(File("Example.cs"), 6, second));
        Assert.Equal(false, symbols.HasStatement(File("Example.cs"), 6, second + 1));
    }

    [Fact]
    public void a_file_the_application_was_not_built_from_is_not_described()
    {
        BuiltApplication.Write(Package, File("Example.cs"), Source);
        var symbols = ApplicationSymbols.Read(Package);
        Assert.Null(symbols.HasStatement(File("Other.cs"), 6, 1));
        Directory.CreateDirectory(File("elsewhere"));
        System.IO.File.WriteAllText(Path.Combine(File("elsewhere"), "Example.cs"), "// Another file of the same name.");
        Assert.Null(symbols.HasStatement(Path.Combine(File("elsewhere"), "Example.cs"), 6, 1));
    }

    [Fact]
    public void a_copy_of_a_source_file_is_recognised_by_its_content()
    {
        BuiltApplication.Write(Package, Path.Combine(Path.GetTempPath(), "build-machine", "Example.cs"), Source);
        // The compiler records a hash of the file's bytes; the fixture compiles the text as UTF-8 with its preamble.
        System.IO.File.WriteAllText(File("Example.cs"), Source, System.Text.Encoding.UTF8);
        var symbols = ApplicationSymbols.Read(Package);
        Assert.Equal(true, symbols.HasStatement(File("Example.cs"), 6, 1));
    }

    [Fact]
    public void a_file_that_is_not_a_package_is_refused()
    {
        System.IO.File.WriteAllText(Package, "not a package archive");
        Assert.ThrowsAny<Exception>(() => ApplicationSymbols.Read(Package));
        Assert.Throws<FileNotFoundException>(() => ApplicationSymbols.Read(File("missing.tar")));
    }
}
