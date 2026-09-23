using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.Loader;
using System.Text.Json;
using System.Text.Json.Serialization;
using Isoplane.Build;
using Isoplane.Durable;

namespace Isoplane.MonoRuntime;

/// <summary>
/// The runtime's fixed application factory. It loads the application package
/// the host serves beneath <see cref="Root"/> and returns the handler the
/// package's manifest names. It runs during initialization, so the loaded
/// assemblies are part of the captured template rather than of each instance.
/// </summary>
public static class PackageLoader
{
    /// <summary>
    /// Where the host serves the package's <c>application.json</c> and
    /// <c>assemblies/</c>, laid out as in the managed bundle.
    /// </summary>
    public const string Root = "/.isoplane/package";

    /// <summary>
    /// Loads every package assembly into the default context and invokes the
    /// declared factory. Throws when the manifest selects another execution
    /// mode, when the runtime already supplies a package assembly's name, or
    /// when the factory is not a public static parameterless method returning
    /// <see cref="IIsoplaneHandler"/>.
    /// </summary>
    public static IIsoplaneHandler Load()
    {
        ManagedApplication application = ReadManifest();
        Dictionary<string, string> assemblies = Index(Path.Combine(Root, "assemblies"));
        AssemblyLoadContext context = AssemblyLoadContext.Default;
        context.Resolving += (loading, name) =>
            assemblies.TryGetValue(Key(name), out string? path) ? loading.LoadFromAssemblyPath(path) : null;
        foreach (KeyValuePair<string, string> assembly in assemblies.OrderBy(entry => entry.Key, StringComparer.Ordinal))
            RequireFromPackage(context, assembly.Key, assembly.Value);
        return CreateHandler(context.LoadFromAssemblyName(new AssemblyName(application.Assembly)), application.Factory);
    }

    private static ManagedApplication ReadManifest()
    {
        using FileStream manifest = File.OpenRead(Path.Combine(Root, "application.json"));
        ManagedApplication application = JsonSerializer.Deserialize(manifest, new ManifestJson(new JsonSerializerOptions(BuildJson.Options)).ManagedApplication)
            ?? throw Refuse(new PackageManifestException("the application manifest is empty"));
        if (application.Execution != GuestExecution.Interpreted)
            throw Refuse(new PackageExecutionException($"the package targets {application.Execution} execution, not this interpreter"));
        return application;
    }

    /// <summary>
    /// Package assemblies by the key the managed bundle files them under,
    /// <c>[culture/]Name</c>, each checked to define the assembly its key names.
    /// </summary>
    private static Dictionary<string, string> Index(string directory)
    {
        var assemblies = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (string path in Directory.EnumerateFiles(directory, "*.dll", SearchOption.AllDirectories))
        {
            string key = Path.ChangeExtension(Path.GetRelativePath(directory, path), null).Replace(Path.DirectorySeparatorChar, '/');
            AssemblyName defined = AssemblyName.GetAssemblyName(path);
            if (!string.Equals(Key(defined), key, StringComparison.OrdinalIgnoreCase))
                throw Refuse(new PackageAssemblyConflictException($"package file '{key}.dll' defines assembly '{Key(defined)}'"));
            assemblies.Add(key, path);
        }
        return assemblies;
    }

    private static string Key(AssemblyName name) =>
        string.IsNullOrEmpty(name.CultureName) ? name.Name ?? "" : name.CultureName + "/" + name.Name;

    /// <summary>
    /// Loads a package assembly by name, as its dependents will, and refuses
    /// the package when that name binds to the runtime's assembly instead.
    /// </summary>
    private static void RequireFromPackage(AssemblyLoadContext context, string key, string path)
    {
        int separator = key.LastIndexOf('/');
        var name = new AssemblyName(key[(separator + 1)..]) { CultureName = separator < 0 ? "" : key[..separator] };
        Assembly loaded = context.LoadFromAssemblyName(name);
        if (!string.Equals(loaded.Location, path, StringComparison.Ordinal))
            throw Refuse(new PackageAssemblyConflictException($"package assembly '{key}' is supplied by the runtime"));
    }

    private static IIsoplaneHandler CreateHandler(Assembly entry, string factory)
    {
        int separator = factory.LastIndexOf('.');
        string typeName = separator > 0 ? factory[..separator] : "";
        Type? type = entry.GetExportedTypes().SingleOrDefault(candidate => candidate.FullName?.Replace('+', '.') == typeName);
        MethodInfo? method = type?.GetMethod(factory[(separator + 1)..], BindingFlags.Public | BindingFlags.Static, Type.EmptyTypes);
        if (method is null || method.IsGenericMethodDefinition || !typeof(IIsoplaneHandler).IsAssignableFrom(method.ReturnType))
            throw Refuse(new PackageFactoryException($"'{factory}' is not a public static parameterless method of '{entry.GetName().Name}' returning IIsoplaneHandler"));
        return method.CreateDelegate<Func<IIsoplaneHandler>>()()
            ?? throw Refuse(new PackageFactoryException($"'{factory}' returned no handler"));
    }

    /// <summary>
    /// The export boundary reports only an exception's type, so the context
    /// of a refusal goes to the guest log first.
    /// </summary>
    private static Exception Refuse(Exception refusal)
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

internal sealed class PackageManifestException(string message) : Exception(message);

internal sealed class PackageExecutionException(string message) : Exception(message);

internal sealed class PackageAssemblyConflictException(string message) : Exception(message);

internal sealed class PackageFactoryException(string message) : Exception(message);
