using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Tomlyn;
using Tomlyn.Model;
using Isoplane.Native;

namespace Isoplane.Cli;

internal sealed class LocalProject
{
    private readonly string directory;
    private readonly TomlTable manifest;

    internal LocalProject(string directory)
    {
        this.directory = Path.GetFullPath(directory);
        var path = Path.Combine(this.directory, "isoplane.toml");
        try { manifest = TomlSerializer.Deserialize<TomlTable>(File.ReadAllText(path)) ?? throw new ArgumentException($"Empty project manifest: {path}"); }
        catch (TomlException error) { throw new ArgumentException($"Cannot read {path}: {error.Message}", error); }
        if (!manifest.TryGetValue("name", out var name) || name is not string text || string.IsNullOrWhiteSpace(text))
            throw new ArgumentException($"{path} must establish a nonempty app name.");
    }

    internal string Module => manifest.TryGetValue("module", out var module) && module is string path && !string.IsNullOrWhiteSpace(path)
        ? Path.GetFullPath(path, directory) : throw new ArgumentException("isoplane.toml must name the module to execute.");

    /// <summary>Where <c>isoplane dev</c> builds an interpreted project's package.</summary>
    internal string Package => Path.Combine(directory, ".isoplane", "package", "application.tar");

    internal byte[] SessionOptions(ushort? port, Dictionary<string, string> configuration, string? debugEndpoint = null, string? debugDirectory = null, string? package = null)
    {
        var development = manifest.TryGetValue("dev", out var dev) && dev is TomlTable table ? table : new TomlTable();
        var trust = development.TryGetValue("trust", out var configuredTrust) && configuredTrust is TomlTable trustTable ? trustTable : new TomlTable();
        if (trust.TryGetValue("bundle", out var bundle) && bundle is string path)
            trust["bundle"] = Path.GetFullPath(path, directory);
        var identity = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes((string)manifest["name"]!)));
        Dictionary<string, object?> settings = new()
        {
            ["manifest"] = manifest,
            ["state_directory"] = debugDirectory ?? Path.Combine(directory, ".isoplane", "local", identity),
            ["runtime_library"] = NativeRuntime.LibraryPath,
            ["compilation_directory"] = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Isoplane", "compiled"),
            ["configuration"] = configuration,
            ["port"] = port,
            ["log_filter"] = Environment.GetEnvironmentVariable("RUST_LOG") ?? "info",
            ["debug_endpoint"] = debugEndpoint,
            ["trust"] = trust,
            ["package"] = package,
        };
        if (development.TryGetValue("init_sandbox", out var sandbox)) settings["init_sandbox"] = sandbox;
        if (development.TryGetValue("allow_endpoints", out var endpoints)) settings["allow_endpoints"] = endpoints;
        return JsonSerializer.SerializeToUtf8Bytes(settings);
    }
}
