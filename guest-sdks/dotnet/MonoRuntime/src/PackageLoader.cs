using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.Loader;
using System.Text.Json;
using System.Text.Json.Serialization;
using Isoplane.Build;
using Isoplane.AspNetCore;
using Isoplane;
using Microsoft.AspNetCore.Builder;

namespace Isoplane.MonoRuntime;

/// <summary>
/// The runtime's fixed application loader. It loads the application package
/// the host serves beneath <see cref="Root"/> and returns the router for the
/// factory the package's manifest names. It runs during initialization, so the
/// loaded assemblies are part of the captured template rather than of each
/// instance.
/// </summary>
public static class PackageLoader
{
    /// <summary>
    /// Where the host serves the package's <c>application.json</c> and
    /// <c>assemblies/</c>, laid out as in the managed bundle.
    /// </summary>
    public const string Root = "/.isoplane/package";

    /// <summary>
    /// Resolves package assemblies on demand in the default context and returns the
    /// router the SDK generates for the declared factory, not yet booted.
    /// Throws when the manifest selects another execution mode, when the
    /// runtime already supplies a package assembly's name, or when the factory
    /// is not a public static parameterless method returning
    /// <see cref="IIsoplaneHandler"/>, <see cref="IIsoplaneRouter"/>, or an ASP.NET <c>WebApplication</c>.
    /// </summary>
    public static IIsoplaneRouter Load()
    {
        ManagedApplication application = ReadManifest();
        AssemblyLoadContext context = AssemblyLoadContext.Default;
        Dictionary<string, string> assemblies = Index(context, Path.Combine(Root, "assemblies"));
        context.Resolving += (loading, name) =>
            assemblies.TryGetValue(Key(name), out string? path) ? loading.LoadFromAssemblyPath(path) : null;
        return CreateRouter(context.LoadFromAssemblyName(new AssemblyName(application.Assembly)), application.Factory);
    }

    private static ManagedApplication ReadManifest()
    {
        using FileStream manifest = File.OpenRead(Path.Combine(Root, "application.json"));
        try
        {
            return JsonSerializer.Deserialize(manifest, new ManifestJson(new JsonSerializerOptions(BuildJson.Options)).ManagedApplication)
                ?? throw Refuse(new PackageManifestException("the application manifest is empty"));
        }
        catch (JsonException error)
        {
            throw Refuse(new PackageManifestException("the application manifest is invalid: " + error.Message));
        }
    }

    /// <summary>
    /// Package assemblies by the key the managed bundle files them under,
    /// <c>[culture/]Name</c>, each checked to define the assembly its key names.
    /// </summary>
    private static Dictionary<string, string> Index(AssemblyLoadContext context, string directory)
    {
        var assemblies = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (string path in Directory.EnumerateFiles(directory, "*.dll", SearchOption.AllDirectories))
        {
            string key = Path.ChangeExtension(Path.GetRelativePath(directory, path), null).Replace(Path.DirectorySeparatorChar, '/');
            RefuseRuntimeAssembly(context, key);
            AssemblyName defined = AssemblyName.GetAssemblyName(path);
            if (!string.Equals(Key(defined), key, StringComparison.OrdinalIgnoreCase))
                throw Refuse(new PackageAssemblyConflictException($"package file '{key}.dll' defines assembly '{Key(defined)}'"));
            assemblies.Add(key, path);
        }
        return assemblies;
    }

    private static string Key(AssemblyName name) =>
        string.IsNullOrEmpty(name.CultureName) ? name.Name ?? "" : name.CultureName + "/" + name.Name;

    private static void RefuseRuntimeAssembly(AssemblyLoadContext context, string key)
    {
        int separator = key.LastIndexOf('/');
        var name = new AssemblyName(key[(separator + 1)..]) { CultureName = separator < 0 ? "" : key[..separator] };
        try
        {
            context.LoadFromAssemblyName(name);
        }
        catch (FileNotFoundException)
        {
            // Package resolution is not installed yet, so only runtime assemblies can bind.
            return;
        }
        throw Refuse(new PackageAssemblyConflictException($"package assembly '{key}' is supplied by the runtime"));
    }

    private static IIsoplaneRouter CreateRouter(Assembly entry, string factory)
    {
        int separator = factory.LastIndexOf('.');
        string typeName = separator > 0 ? factory[..separator] : "";
        Type? type = entry.GetExportedTypes().SingleOrDefault(candidate => candidate.FullName?.Replace('+', '.') == typeName);
        MethodInfo? method = type?.GetMethod(factory[(separator + 1)..], BindingFlags.Public | BindingFlags.Static, Type.EmptyTypes);
        if (method is null || method.IsGenericMethodDefinition)
            throw Refuse(new PackageFactoryException($"'{factory}' is not a public static parameterless method of '{entry.GetName().Name}'"));
        if (typeof(IIsoplaneRouter).IsAssignableFrom(method.ReturnType))
        {
            Func<IIsoplaneRouter?> create = method.CreateDelegate<Func<IIsoplaneRouter?>>();
            return create() ?? throw Refuse(new PackageFactoryException($"'{factory}' returned no router"));
        }
        if (typeof(IIsoplaneHandler).IsAssignableFrom(method.ReturnType))
        {
            Func<IIsoplaneHandler?> create = method.CreateDelegate<Func<IIsoplaneHandler?>>();
            return new HandlerFactoryRouter(() => create()
                ?? throw Refuse(new PackageFactoryException($"'{factory}' returned no handler")));
        }
        // Compared by name first: binding the type loads ASP.NET, which only
        // an ASP.NET package carries.
        if (method.ReturnType.FullName == WebApplications.TypeName)
            return WebApplications.Router(method, factory);
        throw Refuse(new PackageFactoryException($"'{factory}' returns '{method.ReturnType.FullName}', not IIsoplaneHandler, IIsoplaneRouter or WebApplication"));
    }

    /// <summary>
    /// The export boundary reports only an exception's type, so the context
    /// of a refusal goes to the guest log first.
    /// </summary>
    internal static Exception Refuse(Exception refusal)
    {
        Console.Error.WriteLine("application package refused: " + refusal.Message);
        return refusal;
    }
}

/// <summary>
/// Reads the manifest without the reflection serializer, whose caches would
/// otherwise be initialized into every application's template.
/// </summary>
[JsonSerializable(typeof(ManagedApplication))]
internal sealed partial class ManifestJson : JsonSerializerContext;

/// <summary>ASP.NET applications, kept apart so only they bind ASP.NET.</summary>
internal static class WebApplications
{
    internal const string TypeName = "Microsoft.AspNetCore.Builder.WebApplication";

    [MethodImpl(MethodImplOptions.NoInlining)]
    internal static IIsoplaneRouter Router(MethodInfo method, string factory)
    {
        if (method.ReturnType != typeof(WebApplication))
            throw PackageLoader.Refuse(new PackageFactoryException($"'{factory}' returns a WebApplication the runtime's ASP.NET adapter does not bind"));
        return new WebApplicationRouter(method.CreateDelegate<Func<WebApplication>>());
    }
}

internal sealed class PackageManifestException(string message) : Exception(message);

internal sealed class PackageAssemblyConflictException(string message) : Exception(message);

internal sealed class PackageFactoryException(string message) : Exception(message);
