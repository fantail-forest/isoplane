using System.Reflection;
using System.Text;
using System.Text.RegularExpressions;
using XmlDocMarkdown.Core;

if (args.Length is < 1 or > 2 || (args.Length == 2 && args[1] != "--check"))
{
    Console.Error.WriteLine("Usage: ApiReference <reference-directory> [--check]");
    return 2;
}
var temporary = Directory.CreateTempSubdirectory("isoplane-api-reference-");
try
{
    var generated = Reference.Generate(temporary.FullName);
    var destination = Path.GetFullPath(args[0]);
    var existing = Directory.Exists(Path.Combine(destination, "sdk"))
        ? Directory.EnumerateFiles(Path.Combine(destination, "sdk"), "*.md", SearchOption.AllDirectories)
            .Select(path => Path.GetRelativePath(destination, path)).ToHashSet(StringComparer.Ordinal)
        : [];
    if (File.Exists(Path.Combine(destination, "sdk.md"))) existing.Add("sdk.md");
    var differences = generated.Keys.Union(existing).Where(path =>
        !generated.TryGetValue(path, out var content) || !File.Exists(Path.Combine(destination, path)) ||
        File.ReadAllText(Path.Combine(destination, path)) != content).Order(StringComparer.Ordinal).ToArray();
    if (args.Length == 2)
    {
        if (differences.Length == 0) return 0;
        Console.Error.WriteLine("SDK reference is stale. Run npm run generate:reference in docs/userdocs.");
        foreach (var path in differences) Console.Error.WriteLine(path);
        return 1;
    }
    foreach (var path in existing.Except(generated.Keys)) File.Delete(Path.Combine(destination, path));
    foreach (var (path, content) in generated)
    {
        var file = Path.Combine(destination, path);
        Directory.CreateDirectory(Path.GetDirectoryName(file)!);
        File.WriteAllText(file, content, new UTF8Encoding(false));
    }
    Console.WriteLine($"Generated {generated.Count} SDK reference pages.");
    return 0;
}
finally
{
    temporary.Delete(recursive: true);
}

internal static class Reference
{
    private static readonly Regex Link = new(@"\[([^\]]+)\]\(([^)]+)\)", RegexOptions.CultureInvariant);
    private static readonly Assembly[] Assemblies = [typeof(Isoplane.Request).Assembly, typeof(Isoplane.AspNetCore.IsoplaneHosting).Assembly, typeof(Isoplane.Testing.IsoplaneApplication).Assembly];
    private static readonly HashSet<string> Namespaces = ["Isoplane", "Isoplane.AspNetCore", "Isoplane.Testing"];

    internal static Dictionary<string, string> Generate(string temporary)
    {
        var output = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var assembly in Assemblies)
        {
            var name = assembly.GetName().Name!;
            var directory = Path.Combine(temporary, name);
            XmlDocMarkdownGenerator.Generate(new XmlDocInput { Assembly = assembly }, directory,
                new XmlDocMarkdownSettings { VisibilityLevel = XmlDocVisibilityLevel.Public, SkipCompilerGenerated = true, NewLine = "\n" });
            var types = assembly.GetExportedTypes().Where(type => Namespaces.Contains(type.Namespace ?? "")).ToArray();
            var pages = Directory.EnumerateFiles(directory, "*.md", SearchOption.AllDirectories)
                .Where(path => Included(Path.GetRelativePath(directory, path), name, types)).ToHashSet(StringComparer.Ordinal);
            foreach (var page in pages)
            {
                var text = Filter(File.ReadAllText(page), page, pages, types);
                var relative = Path.Combine("sdk", name, Path.GetRelativePath(directory, page));
                output.Add(relative, "---\nmdx:\n  format: md\n---\n\n" + text.TrimEnd() + "\n");
            }
        }
        output.Add("sdk.md", Overview);
        return output;
    }

    private static bool Included(string path, string assembly, Type[] types)
    {
        if (path == assembly + ".md") return true;
        foreach (var type in types)
        {
            var stem = Path.Combine(type.Namespace!, type.Name.Replace('`', '-'));
            if (path == stem + ".md") return true;
            if (!path.StartsWith(stem + Path.DirectorySeparatorChar, StringComparison.Ordinal)) continue;
            var member = Path.GetFileNameWithoutExtension(path);
            return !type.GetMethods(BindingFlags.Public | BindingFlags.Static | BindingFlags.Instance | BindingFlags.DeclaredOnly)
                .Any(method => method.Name == member && (ReferencesInterop(method.ReturnType) || method.GetParameters().Any(parameter => ReferencesInterop(parameter.ParameterType))));
        }
        return false;
    }

    private static bool ReferencesInterop(Type type) => type.Namespace == "Isoplane.Abi" ||
        (type.HasElementType && ReferencesInterop(type.GetElementType()!)) ||
        (type.IsGenericType && type.GetGenericArguments().Any(ReferencesInterop));

    private static string Filter(string text, string page, HashSet<string> pages, Type[] types)
    {
        var result = new StringBuilder();
        var skipSection = false;
        foreach (var line in text.Split('\n'))
        {
            if (line.StartsWith("## ", StringComparison.Ordinal))
                skipSection = line.EndsWith(" namespace", StringComparison.Ordinal) &&
                    !types.Any(type => line == $"## {type.Namespace} namespace");
            if (skipSection) continue;
            var links = Link.Matches(line);
            if (line.StartsWith('|') && links.Count > 0 && !TargetIncluded(links[0].Groups[2].Value, page, pages)) continue;
            result.Append(Link.Replace(line, match => TargetIncluded(match.Groups[2].Value, page, pages) ? match.Value : match.Groups[1].Value).TrimEnd()).Append('\n');
        }
        var rendered = result.ToString();
        foreach (var type in types)
        {
            var prefix = Path.Combine(type.Namespace!, type.Name.Replace('`', '-')) + Path.DirectorySeparatorChar;
            foreach (var property in type.GetProperties(BindingFlags.Public | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly))
            {
                if (page.EndsWith(prefix + property.Name + ".md", StringComparison.Ordinal) &&
                    property.SetMethod?.ReturnParameter.GetRequiredCustomModifiers().Contains(typeof(System.Runtime.CompilerServices.IsExternalInit)) == true)
                    rendered = rendered.Replace("{ get; set; }", "{ get; init; }", StringComparison.Ordinal);
            }
        }
        return rendered;
    }

    private static bool TargetIncluded(string link, string page, HashSet<string> pages)
    {
        if (Uri.TryCreate(link, UriKind.Absolute, out _) || !link.Split('#')[0].EndsWith(".md", StringComparison.Ordinal)) return true;
        return pages.Contains(Path.GetFullPath(Path.Combine(Path.GetDirectoryName(page)!, link.Split('#')[0])));
    }

    private const string Overview = """
---
title: SDK API reference
mdx:
  format: md
---

# SDK API reference

These pages describe the customer HTTP, ASP.NET and integration-testing APIs in the installed packages. Signatures and member documentation are generated from the compiled assemblies and their XML documentation comments.

- [Isoplane.Sdk](sdk/Isoplane.Sdk/Isoplane.Sdk.md): requests, responses, HTTP handlers, activation, configuration and logging.
- [Isoplane.AspNetCore](sdk/Isoplane.AspNetCore/Isoplane.AspNetCore.md): ASP.NET application hosting over the sandbox's request transport.
- [Isoplane.Testing](sdk/Isoplane.Testing/Isoplane.Testing.md): test-owned application sessions and startup options. Its C# namespace is `Isoplane.Testing`.

<!-- Generated by tools/ApiReference. -->
""" + "\n";
}
