using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Isoplane.Analyzers;
using Isoplane.Build;
using Isoplane.Cli;
using Microsoft.CodeAnalysis.Diagnostics;
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
        Console.Error.WriteLine("SDK reference is stale. Run pnpm run generate:reference in docs/userdocs.");
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
    // Doc comments link to the published guides, which is what an editor's tooltip needs. On the site
    // those become links between files, so that the build fails on one whose guide has gone.
    private static readonly Regex GuideLink = new(@"\]\(https://isoplane\.dev/docs/([^)#]+)(#[^)]*)?\)", RegexOptions.CultureInvariant);
    private static readonly Regex ErrorCode = new(@"ISOPLANE\d{4}", RegexOptions.CultureInvariant);
    private static readonly Regex ErrorCodeSection = new(@"^### (ISOPLANE\d{4})$", RegexOptions.CultureInvariant | RegexOptions.Multiline);
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
                var relative = Path.Combine("sdk", name, Path.GetRelativePath(directory, Published(page, pages)));
                var guides = string.Concat(Enumerable.Repeat("../", relative.Split(Path.DirectorySeparatorChar).Length));
                text = GuideLink.Replace(text, match => $"]({guides}{match.Groups[1].Value}.md{match.Groups[2].Value})");
                output.Add(relative, "---\nmdx:\n  format: md\n---\n\n" + text.TrimEnd() + "\n");
            }
        }
        output.Add("sdk.md", Overview);
        output.Add("project-settings.md", ProjectSettings.Replace("{settings}", Settings()));
        output.Add("error-codes.md", ErrorCodes());
        output.Add("project-templates.md", ProjectTemplates.Replace("{templates}", Templates()));
        output.Add("launch-settings.md", LaunchSettings
            .Replace("{profile}", Keys(LaunchProfileKeys.Profile.Select(key => (key.Name, key.Description))))
            .Replace("{limits}", Keys(LaunchProfileKeys.LimitOverrides.Select(key => (key.Name, key.Description)))));
        output.Add("vscode-launch.md", DebugConfiguration.Replace("{launch}", DebugAttributes("launch")).Replace("{attach}", DebugAttributes("attach")));
        return output;
    }

    /// <summary>
    /// The error code page. Its explanations are the template's; the codes, with the severity and title of
    /// each, are the ones the analyzers and the build declare. A code the template does not explain, or an
    /// explanation of a code nothing declares, fails generation.
    /// </summary>
    private static string ErrorCodes()
    {
        var declared = typeof(BlockingCallAnalyzer).Assembly.GetExportedTypes()
            .Where(type => !type.IsAbstract && typeof(DiagnosticAnalyzer).IsAssignableFrom(type))
            .SelectMany(type => ((DiagnosticAnalyzer)Activator.CreateInstance(type)!).SupportedDiagnostics)
            .Select(rule => (Code: rule.Id, Severity: rule.DefaultSeverity.ToString(), Title: rule.Title.ToString()))
            .Concat(BuildDiagnostics.All.Select(rule => (Code: rule.Code, Severity: rule.IsError ? "Error" : "Warning", Title: rule.Title)))
            .DistinctBy(rule => rule.Code).OrderBy(rule => rule.Code, StringComparer.Ordinal)
            .Select(rule => rule with { Title = rule.Title.Replace("<", "&lt;") }).ToArray();
        using var reader = new StreamReader(Resource("ErrorCodes.md"));
        var page = reader.ReadToEnd();
        var explained = ErrorCodeSection.Matches(page).Select(section => section.Groups[1].Value).ToArray();
        var codes = declared.Select(rule => rule.Code).ToArray();
        if (codes.Except(explained).Concat(explained.Except(codes)).Order(StringComparer.Ordinal).ToArray() is { Length: > 0 } unmatched)
            throw new InvalidOperationException("ErrorCodes.md and the declared diagnostics differ on: " + string.Join(", ", unmatched));
        foreach (var rule in declared)
            page = page.Replace($"### {rule.Code}\n", $"### {rule.Code}\n\n**{rule.Severity}.** {rule.Title}.\n");
        return page.Replace("{codes}", "| Code | Severity | Meaning |\n| --- | --- | --- |\n" + string.Join("\n",
            declared.Select(rule => $"| [`{rule.Code}`](#{rule.Code.ToLowerInvariant()}) | {rule.Severity} | {rule.Title} |")));
    }

    /// <summary>One section for each template the SDK ships, from the definition file inside the template.</summary>
    private static string Templates()
    {
        var sections = new SortedDictionary<string, string>(StringComparer.Ordinal);
        var resources = typeof(Reference).Assembly.GetManifestResourceNames();
        foreach (var resource in resources.Where(name => name.StartsWith("Templates/", StringComparison.Ordinal)))
        {
            using var definition = JsonDocument.Parse(Resource(resource));
            var template = definition.RootElement;
            var shortName = template.GetProperty("shortName").GetString()!;
            var source = template.GetProperty("sourceName").GetString()!;
            // A template added beside an application is named after it, as its source name is.
            var name = source.EndsWith(".Tests", StringComparison.Ordinal) ? "MyApp.Tests" : "MyApp";
            var section = new StringBuilder($"### `{shortName}`\n\n**{template.GetProperty("name").GetString()}.** {template.GetProperty("description").GetString()}\n\n");
            section.Append($"```bash\ndotnet new {shortName} --name {name}\n```\n\nIn a new `{name}` directory, it creates:\n\n");
            section.AppendJoin('\n', template.GetProperty("primaryOutputs").EnumerateArray()
                .Select(output => $"- `{output.GetProperty("path").GetString()!.Replace(source, name)}`")).Append('\n');
            var options = template.GetProperty("symbols").EnumerateObject()
                .Where(symbol => symbol.Value.GetProperty("type").GetString() == "parameter").ToArray();
            var names = OptionNames(resources, "TemplateOptions/" + resource["Templates/".Length..]);
            if (options.Length != 0)
                section.Append("\n| Option | Value | Description |\n| --- | --- | --- |\n").AppendJoin('\n', options.Select(option =>
                    $"| `--{names.GetValueOrDefault(option.Name, option.Name)}` | {option.Value.GetProperty("datatype").GetString()} | {option.Value.GetProperty("description").GetString()} |")).Append('\n');
            sections.Add(shortName, section.ToString());
        }
        return string.Join("\n", sections.Values).TrimEnd();
    }

    /// <summary>The command-line names a template gives its options; an option without one is spelled as its symbol.</summary>
    private static Dictionary<string, string> OptionNames(string[] resources, string resource)
    {
        if (!resources.Contains(resource)) return [];
        using var host = JsonDocument.Parse(Resource(resource));
        return host.RootElement.GetProperty("symbolInfo").EnumerateObject()
            .Where(symbol => symbol.Value.TryGetProperty("longName", out _))
            .ToDictionary(symbol => symbol.Name, symbol => symbol.Value.GetProperty("longName").GetString()!);
    }

    private static Stream Resource(string name) => typeof(Reference).Assembly.GetManifestResourceStream(name)
        ?? throw new InvalidOperationException($"{name} is not embedded in this tool.");

    private static string Keys(IEnumerable<(string Name, string Description)> keys) =>
        "| Key | Description |\n| --- | --- |\n" + string.Join("\n", keys.Select(key => $"| `{key.Name}` | {key.Description} |"));

    /// <summary>The attributes the debugger extension's manifest declares for one kind of debug configuration.</summary>
    private static string DebugAttributes(string request)
    {
        using var manifest = JsonDocument.Parse(Resource("vscode-isoplane.package.json"));
        var attributes = manifest.RootElement.GetProperty("contributes").GetProperty("debuggers")[0]
            .GetProperty("configurationAttributes").GetProperty(request);
        var required = attributes.TryGetProperty("required", out var names)
            ? names.EnumerateArray().Select(name => name.GetString()).ToHashSet() : [];
        return "| Attribute | Value | Default | Description |\n| --- | --- | --- | --- |\n" + string.Join("\n",
            attributes.GetProperty("properties").EnumerateObject().Select(attribute =>
            {
                var value = attribute.Value.TryGetProperty("enum", out var choices)
                    ? string.Join(" or ", choices.EnumerateArray().Select(choice => $"`{choice.GetString()}`"))
                    : attribute.Value.GetProperty("type").GetString();
                var initial = required.Contains(attribute.Name) ? "required"
                    : attribute.Value.TryGetProperty("default", out var preset) ? $"`{preset.GetRawText().Trim('"')}`" : "";
                return $"| `{attribute.Name}` | {value} | {initial} | {attribute.Value.GetProperty("description").GetString()} |";
            }));
    }

    /// <summary>
    /// One section for each property the SDK's build file gives a default, described by the comment above it there.
    /// </summary>
    private static string Settings()
    {
        using var targets = Resource("Isoplane.Conformance.targets");
        var settings = new StringBuilder();
        foreach (var property in XDocument.Load(targets).Root!.Elements("PropertyGroup").First().Elements())
        {
            var name = property.Name.LocalName;
            if (property.PreviousNode is not XComment comment)
                throw new InvalidOperationException($"{name} has no comment describing it in Isoplane.Conformance.targets.");
            var description = string.Join(' ', comment.Value.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
            description = ErrorCode.Replace(description, code => $"[`{code.Value}`](error-codes.md#{code.Value.ToLowerInvariant()})");
            settings.Append($"### `{name}`\n\nDefault: `{property.Value}`.\n\n{description}\n\n");
        }
        return settings.ToString().TrimEnd();
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
            if (line.StartsWith('|') && links.Count > 0 && Target(links[0].Groups[2].Value, page) is { } first && !pages.Contains(first)) continue;
            result.Append(Link.Replace(line, match => Rendered(match, page, pages)).TrimEnd()).Append('\n');
        }
        var rendered = result.ToString();
        foreach (var type in types)
        {
            var prefix = Path.Combine(type.Namespace!, type.Name.Replace('`', '-')) + Path.DirectorySeparatorChar;
            foreach (var property in type.GetProperties(BindingFlags.Public | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly))
            {
                if (property.SetMethod?.ReturnParameter.GetRequiredCustomModifiers().Contains(typeof(System.Runtime.CompilerServices.IsExternalInit)) != true) continue;
                if (page.EndsWith(prefix + property.Name + ".md", StringComparison.Ordinal))
                    rendered = rendered.Replace("{ get; set; }", "{ get; init; }", StringComparison.Ordinal);
                // The type page's member table separates these tokens with no-break spaces.
                var row = $"[{property.Name}]({type.Name}/{property.Name}.md)\u00A0{{\u00A0get;\u00A0";
                if (page.EndsWith(prefix.TrimEnd(Path.DirectorySeparatorChar) + ".md", StringComparison.Ordinal))
                    rendered = rendered.Replace(row + "set;", row + "init;", StringComparison.Ordinal);
            }
        }
        return rendered;
    }

    private static string Rendered(Match link, string page, HashSet<string> pages)
    {
        var (text, destination) = (link.Groups[1].Value, link.Groups[2].Value);
        if (Target(destination, page) is not { } target) return link.Value;
        if (!pages.Contains(target)) return text;
        return $"[{text}]({destination.Replace(Path.GetFileName(target), Path.GetFileName(Published(target, pages)), StringComparison.Ordinal)})";
    }

    private static string? Target(string link, string page)
    {
        var path = link.Split('#')[0];
        if (Uri.TryCreate(link, UriKind.Absolute, out _) || !path.EndsWith(".md", StringComparison.Ordinal)) return null;
        return Path.GetFullPath(Path.Combine(Path.GetDirectoryName(page)!, path));
    }

    // xmldocmd writes a type's constructors to <Type>/<Type>.md. Docusaurus serves a page named after
    // its directory as that directory's index, which is the address <Type>.md already has.
    private static string Published(string page, HashSet<string> pages)
    {
        var directory = Path.GetDirectoryName(page)!;
        var constructors = Path.GetFileNameWithoutExtension(page) == Path.GetFileName(directory) && pages.Contains(directory + ".md");
        return constructors ? Path.Combine(directory, "-ctor.md") : page;
    }

    private const string Overview = """
---
title: SDK API reference
sidebar_position: 6
mdx:
  format: md
---

# SDK API reference

The types and members of the Isoplane packages an application uses. Signatures and descriptions are generated from the compiled assemblies and their documentation comments, so they match the version you have installed.

## Which package you need

An application is written in one of two ways, and tested with a third package.

- **An ASP.NET application** has a `Program.cs`. It calls [`IsoplaneHosting.CreateBuilder`](sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/IsoplaneHosting/CreateBuilder.md) from [Isoplane.AspNetCore](sdk/Isoplane.AspNetCore/Isoplane.AspNetCore.md) and is otherwise ordinary ASP.NET. From [Isoplane.Sdk](sdk/Isoplane.Sdk/Isoplane.Sdk.md) it needs only [`Initialization`](sdk/Isoplane.Sdk/Isoplane/Initialization.md) and [`Activation`](sdk/Isoplane.Sdk/Isoplane/Activation.md), and those only to prepare state ahead of time. Start with [Build a small API with ASP.NET Core and EF Core](../build-an-api.md) or [Bring an existing ASP.NET application](../onboard-an-application.md).
- **An application without ASP.NET** is one static method marked [`[HttpHandler]`](sdk/Isoplane.Sdk/Isoplane/HttpHandlerAttribute.md) that takes a [`Request`](sdk/Isoplane.Sdk/Isoplane/Request.md) and returns a [`Response`](sdk/Isoplane.Sdk/Isoplane/Response.md). Everything it uses is in [Isoplane.Sdk](sdk/Isoplane.Sdk/Isoplane.Sdk.md). Start with [Build a small lightweight web service](../build-a-web-service.md) or [Handle requests without ASP.NET](../handle-requests-without-aspnet.md).
- **A test project** uses [Isoplane.Testing](sdk/Isoplane.Testing/Isoplane.Testing.md) to start either kind of application in the sandbox and send it requests: [`IsoplaneApplication.StartAsync`](sdk/Isoplane.Testing/Isoplane.Testing/IsoplaneApplication/StartAsync.md). Start with [Test an application](../test-an-application.md).

## Words these pages use

- **Instance.** One running copy of your application. Isoplane runs as many as the traffic needs and may discard one between requests, so memory is not a place to keep anything that must last.
- **Template.** Isoplane starts your application once ahead of time and saves its memory. That saved memory is the template, and every instance starts as a copy of it. Work done before the template is saved is done once; state created then is identical in every instance.
- **Initialization.** The start-up work that runs before the template is saved: [`Initialization.Initializing`](sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md), or an [`[Initializer]`](sdk/Isoplane.Sdk/Isoplane/InitializerAttribute.md) method.
- **Activation.** The moment an instance starts from the template, before its first request: [`Activation.Activated`](sdk/Isoplane.Sdk/Isoplane/Activation/Activated.md), or an [`[Activated]`](sdk/Isoplane.Sdk/Isoplane/ActivatedAttribute.md) method. State that must differ between instances is created here.
- **Sandbox.** The isolated environment your application runs in, on your machine and when deployed. It reaches the network and its settings only through Isoplane.

[The application lifecycle](../application-lifecycle.md) explains these in full.

<!-- Generated by tools/ApiReference. -->
""" + "\n";

    private const string ProjectSettings = """
---
sidebar_position: 3
mdx:
  format: md
---

# Project settings

The MSBuild properties an application's project can set to change how the Isoplane SDK builds it. Names, defaults and descriptions are generated from the SDK's build files, so they match the version you have installed.

## Setting a property

Add the property to a `<PropertyGroup>` in the application's project file, the `.csproj`:

```xml
<PropertyGroup>
  <IsoplaneAnalyzeDependencies>true</IsoplaneAnalyzeDependencies>
</PropertyGroup>
```

A project created from an Isoplane template already has these properties in its project file, commented out.

To set a property for every project in a repository, put the same `<PropertyGroup>` in a `Directory.Build.props` file at the repository root. To set it for one build only, pass it on the command line:

```bash
dotnet build -p:IsoplaneAnalyzeDependencies=true
```

## Properties

{settings}

## Standard .NET properties

The warnings these properties produce also follow the ordinary .NET properties `NoWarn`, `WarningsAsErrors`, `TreatWarningsAsErrors` and `WarningsNotAsErrors`. [Control compatibility warnings](../run-and-rebuild.md#control-compatibility-warnings) shows how to use them, and [Error codes](error-codes.md) explains each warning.

<!-- Generated by tools/ApiReference. -->
""" + "\n";

    private const string LaunchSettings = """
---
sidebar_position: 4
mdx:
  format: md
---

# launchSettings.json

`Properties/launchSettings.json` is the ordinary .NET file of launch profiles. `dotnet run` and `isoplane dev` read one profile from it when they start a local session. This page lists the keys Isoplane reads. It ignores every other key, such as `launchBrowser`. The key tables are generated from the list the Isoplane CLI parses, so they match the version you have installed.

[Supply local development settings](../local-settings.md) shows the file in use.

## Which profile is used

A local session uses the first profile whose `commandName` is `Project`. Select another with `dotnet run --launch-profile NAME` or `isoplane dev --launch-profile NAME`, or run without a profile with `--no-launch-profile`.

## Profile keys

{profile}

## Keys of `isoplaneLimitsOverride`

Each key is optional, and each value is a positive whole number. A local session otherwise runs with the largest limits the platform offers. [Execution limits](../execution-limits.md) explains what each limit measures.

{limits}

## Example

```json
{
  "profiles": {
    "http": {
      "commandName": "Project",
      "applicationUrl": "http://localhost:5080",
      "environmentVariables": {
        "ASPNETCORE_ENVIRONMENT": "Development",
        "Greeting__Default": "world"
      },
      "isoplaneLimitsOverride": {
        "memoryBytes": 134217728,
        "occupancyMsPerHttpDispatch": 1000
      }
    }
  }
}
```

## Comments

`dotnet run` does not accept comments in this file. A project created from an Isoplane template carries its notes in a property named `//`, which nothing reads.

<!-- Generated by tools/ApiReference. -->
""" + "\n";

    private const string DebugConfiguration = """
---
sidebar_position: 5
mdx:
  format: md
---

# launch.json for VS Code

`.vscode/launch.json` holds the debug configurations Visual Studio Code offers when you press F5. The Isoplane Debugger extension adds the `isoplane` configuration type. This page lists the attributes of that type. The tables are generated from the extension's manifest.

[Debug an application](../debug-an-application.md) shows how to use these configurations.

## Launch

A configuration with `"request": "launch"` builds the application, starts it in a local session and attaches the debugger.

{launch}

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "Debug local Isoplane",
      "type": "isoplane",
      "request": "launch",
      "project": "${workspaceFolder}",
      "configuration": "Debug"
    }
  ]
}
```

## Attach

A configuration with `"request": "attach"` attaches the debugger to a local debug session that is already running.

{attach}

<!-- Generated by tools/ApiReference. -->
""" + "\n";

    private const string ProjectTemplates = """
---
sidebar_position: 1
mdx:
  format: md
---

# Project templates

The `dotnet new` templates that create an Isoplane application or add tests to one. `isoplane setup` installs them. The names, descriptions and options on this page are generated from the templates themselves, so they match the version you have installed.

Every file in a generated project is commented, and an application's `README.md` says what each file is for and how to make common changes. That README is the guide to a template's contents. This page helps you choose a template and create a project from it.

## Choosing a template

- **Starting something new?** Use [`isoplane-handler`](#isoplane-handler). It has no web framework: one static method receives every request. [Build a small lightweight web service](../build-a-web-service.md) walks through it.
- **Want ASP.NET Core with a database?** Use [`isoplane-webapi`](#isoplane-webapi). [Build a small API with ASP.NET Core and EF Core](../build-an-api.md) walks through it.
- **Bringing an existing ASP.NET application over, or want ASP.NET Core and nothing else?** Use [`isoplane-web`](#isoplane-web), as [Bring an existing ASP.NET application](../onboard-an-application.md) and the [Quickstart](../quickstart.md) do.
- **Giving an AI agent tools to call?** Use [`isoplane-mcpserver`](#isoplane-mcpserver), a Model Context Protocol server over HTTP.
- **Have an application without sandbox tests?** Add them with [`isoplane-tests`](#isoplane-tests), created beside the application. [Test an application](../test-an-application.md) walks through it. `isoplane-handler` and `isoplane-webapi` already include a test project.

## Creating a project

`--name` names the project and the directory it is created in. Without it, the project takes the name of the current directory and is created there.

## Templates

{templates}

<!-- Generated by tools/ApiReference. -->
""" + "\n";
}
