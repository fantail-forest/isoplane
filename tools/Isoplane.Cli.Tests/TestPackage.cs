using System.IO.Compression;
using System.Xml.Linq;

namespace Isoplane.Cli.Tests;

/// <summary>A NuGet package a test writes for itself, holding only what the test needs NuGet or <c>dotnet new</c> to read.</summary>
internal static class TestPackage
{
    /// <summary>
    /// Writes <paramref name="id"/> at <paramref name="version"/> into <paramref name="feed"/> with the
    /// <paramref name="files"/> given by path, and returns the package file. <paramref name="type"/> is
    /// the package type, such as <c>Template</c>, or null for an ordinary dependency.
    /// </summary>
    internal static string Write(string feed, string id, string version, string? type, Dictionary<string, string> files)
    {
        var path = Path.Combine(feed, $"{id}.{version}.nupkg");
        using var package = ZipFile.Open(path, ZipArchiveMode.Create);
        XNamespace nuspec = "http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd";
        var manifest = new XDocument(new XElement(nuspec + "package", new XElement(nuspec + "metadata",
            new XElement(nuspec + "id", id), new XElement(nuspec + "version", version), new XElement(nuspec + "authors", "test"), new XElement(nuspec + "description", "A test package."),
            type is null ? null : new XElement(nuspec + "packageTypes", new XElement(nuspec + "packageType", new XAttribute("name", type)))))).ToString();
        foreach (var (file, content) in files.Append(new($"{id}.nuspec", manifest)))
        {
            using var writer = new StreamWriter(package.CreateEntry(file).Open());
            writer.Write(content);
        }
        return path;
    }
}
