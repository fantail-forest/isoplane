using System.Globalization;
using Isoplane.Build;

namespace Isoplane.Testing;

/// <summary>
/// Files a test gives the application it starts, in <see cref="ApplicationOptions.Content"/>. A source is
/// either a <see cref="ContentDirectory"/> or a <see cref="ContentFile"/>.
/// </summary>
/// <remarks>
/// The application reads them as it reads the files its own project bundles, such as those in
/// <c>wwwroot</c> or declared as <c>Content</c>. They are read-only, at paths beneath its content root.
/// </remarks>
public abstract record ContentSource
{
    private protected ContentSource() { }

    /// <summary>
    /// The directory <c>isoplane dev --content</c> reads this source's files from, laid out as the
    /// application's content root. A source that is not laid out that way copies its files into the empty
    /// directory <paramref name="copyDirectory"/> creates.
    /// </summary>
    internal abstract string ContentRoot(Func<string> copyDirectory);

    private protected static void Copy(string source, string destination)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
        File.Copy(source, destination, overwrite: true);
        // File.Copy gives the copy a read-only source's attribute, and a read-only copy can be neither replaced nor, on Windows, deleted.
        File.SetAttributes(destination, File.GetAttributes(destination) & ~FileAttributes.ReadOnly);
    }
}

/// <summary>
/// Every file in a directory on the test machine, each given to the application at its path relative to
/// that directory.
/// </summary>
/// <param name="Directory">
/// The directory to read, including its subdirectories. A relative path is resolved against the current
/// directory, which varies between test runners, so build an absolute one from
/// <c>AppContext.BaseDirectory</c>.
/// </param>
/// <remarks>
/// Lay the directory out as the application's content root, so that a file at
/// <c>Templates/welcome.html</c> beneath it is the application's <c>Templates/welcome.html</c>. To have the files beside the test assembly, include them
/// in the test project with <c>CopyToOutputDirectory</c>. The directory is read where it is while
/// <see cref="IsoplaneApplication.StartAsync"/> builds the application's package, and is not copied.
/// </remarks>
/// <example>
/// The files in the test project's <c>Content</c> directory, copied to its output:
/// <code>
/// new ContentDirectory(Path.Combine(AppContext.BaseDirectory, "Content"))
/// </code>
/// </example>
public sealed record ContentDirectory(string Directory) : ContentSource
{
    internal override string ContentRoot(Func<string> copyDirectory)
    {
        var directory = Path.GetFullPath(Directory);
        return System.IO.Directory.Exists(directory) ? directory
            : throw new ArgumentException($"Content directory does not exist: {directory}");
    }
}

/// <summary>One file on the test machine, given to the application at a path the test chooses.</summary>
/// <example>
/// A file the test wrote, which the application reads as <c>Knowledge/Articles/expenses.md</c>:
/// <code>
/// new ContentFile("Knowledge/Articles/expenses.md", articlePath)
/// </code>
/// </example>
public sealed record ContentFile : ContentSource
{
    private readonly string path = "";

    /// <summary>Gives the application the file at <paramref name="source"/> as <paramref name="path"/>.</summary>
    /// <param name="path">The value of <see cref="Path"/>.</param>
    /// <param name="source">The value of <see cref="Source"/>.</param>
    /// <exception cref="ContentPathException"><paramref name="path"/> is not a path beneath the content root.</exception>
    public ContentFile(string path, string source)
    {
        Path = path;
        Source = source;
    }

    /// <summary>
    /// Where the application reads the file, relative to its content root, with <c>/</c> between
    /// directories, such as <c>Templates/welcome.html</c>.
    /// </summary>
    /// <exception cref="ContentPathException">
    /// The value is not a path beneath the content root. It is empty or absolute, or it has a <c>..</c>,
    /// <c>.</c> or empty segment, a backslash or a colon.
    /// </exception>
    public string Path
    {
        get => path;
        init => path = AssetPaths.IsRelativeFile(value ?? "") ? value! : throw new ContentPathException(value ?? "");
    }

    /// <summary>
    /// The file to read. A relative path is resolved against the current directory, which varies between
    /// test runners, so build an absolute one.
    /// </summary>
    public string Source { get; init; }

    internal override string ContentRoot(Func<string> copyDirectory)
    {
        var source = System.IO.Path.GetFullPath(Source);
        if (!File.Exists(source))
            throw new ArgumentException($"Content file does not exist: {source}");
        var root = copyDirectory();
        Copy(source, System.IO.Path.Combine(root, Path));
        return root;
    }
}

/// <summary>
/// A <see cref="ContentFile"/> was given a path that is not beneath the application's content root.
/// </summary>
/// <remarks>
/// A test's content cannot be placed outside the content root, so the path must be relative and name
/// each directory on the way to the file.
/// </remarks>
public sealed class ContentPathException : ArgumentException
{
    internal ContentPathException(string path)
        : base($"Content path '{path}' is not beneath the application's content root; give a relative path with '/' between directories and no '..' segment, such as Templates/welcome.html.")
    {
        Path = path;
    }

    /// <summary>The path that was refused.</summary>
    public string Path { get; }
}

/// <summary>
/// A test's content as the directories <c>isoplane dev --content</c> reads, in the test's order, with the
/// copies made for sources that have no directory of their own.
/// </summary>
internal sealed class StagedContent
{
    private readonly List<string> directories = [];
    private string? copies;
    private int copied;

    private StagedContent() { }

    internal IReadOnlyList<string> Directories => directories;

    /// <exception cref="ArgumentException">A source's directory or file does not exist.</exception>
    internal static StagedContent Create(IReadOnlyList<ContentSource> sources)
    {
        var staged = new StagedContent();
        try
        {
            foreach (var source in sources)
                staged.directories.Add(source.ContentRoot(staged.CopyDirectory));
            return staged;
        }
        catch
        {
            staged.Delete();
            throw;
        }
    }

    /// <summary>Removes the copies. The sources' own directories and files are left as they are.</summary>
    internal void Delete()
    {
        if (copies is not null)
            Directory.Delete(copies, recursive: true);
        copies = null;
    }

    // A directory per source, so that on a case-insensitive file system two sources' paths that differ
    // only in case stay two files, as they are in the application's package.
    private string CopyDirectory()
    {
        copies ??= Directory.CreateTempSubdirectory("isoplane-content-").FullName;
        return Directory.CreateDirectory(Path.Combine(copies, (copied++).ToString(CultureInfo.InvariantCulture))).FullName;
    }
}
