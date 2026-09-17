using System.Security.Cryptography;
using System.Text.Json;
using System.Xml.Linq;
using Wisp.Native;

namespace Wisp.Cli;

internal static class DebugDevelopment
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        if (await Development.RuntimeProfileAsync(options, cancellation) != GuestRuntimeProfile.Mono)
            throw new ArgumentException("Debugging currently requires an explicitly selected interpreted project configuration; NativeAOT debugging is not supported.");
        var ordinaryModule = new LocalProject(options.Directory).Module;
        var session = Path.Combine(options.Directory, ".wisp", "debug", Guid.NewGuid().ToString("N"));
        try
        {
            if (OperatingSystem.IsWindows())
                Directory.CreateDirectory(session);
            else
                Directory.CreateDirectory(session, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            await using var relay = new DebugRelay(cancellation);
            var endpoint = relay.Endpoint.ToString();
            var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
            List<string> publication = ["publish", project, "-c", options.Configuration, "-o", session, "--disable-build-servers",
                "-p:WispLocalDebug=true", "-p:WispDebugEndpoint=" + endpoint];
            var result = await Processes.RunAsync("dotnet", publication, options.Directory, cancellation, redirectOutput: options.Json);
            if (result != 0)
                return result;
            var artifacts = ReadArtifacts(session, endpoint);
            if (Path.GetFullPath(artifacts.Module) == Path.GetFullPath(ordinaryModule))
                throw new InvalidOperationException("A debug session must not replace the ordinary application module.");
            if (options.Json)
                Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint, module = artifacts.Module, symbols = artifacts.Symbols }));
            else
                Console.WriteLine($"Attach an external Mono debugger to {endpoint}; this session serves one HTTP request.");
            var projectInputs = new LocalProject(options.Directory);
            await using var host = new LocalSession(await File.ReadAllBytesAsync(artifacts.Module, cancellation),
                projectInputs.SessionOptions(options.Port, [], endpoint, Path.Combine(session, "runtime")),
                message => LocalEvents.Write(message, options.Json));
            await RunHostAsync(host, relay, cancellation);
            return 0;
        }
        finally
        {
            try
            {
                if (Directory.Exists(session))
                    Directory.Delete(session, recursive: true);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Console.Error.WriteLine($"isoplane: could not remove debug session {session}: {error.Message}");
            }
        }
    }

    internal static async Task RunHostAsync(LocalSession host, DebugRelay relay, CancellationToken cancellation)
    {
        using var stop = cancellation.Register(host.Stop);
        try
        {
            if (await Task.WhenAny(host.Completion, relay.Completion) == relay.Completion)
                await relay.Completion;
            await host.Completion;
        }
        finally
        {
            host.Stop();
            await host.Completion;
        }
    }

    private static DebugArtifacts ReadArtifacts(string directory, string endpoint)
    {
        var manifests = Directory.GetFiles(directory, "*.debug.xml");
        if (manifests.Length != 1)
            throw new InvalidOperationException("Debug publication must produce one module/symbol manifest.");
        XElement? root;
        try { root = XDocument.Load(manifests[0]).Root; }
        catch (System.Xml.XmlException error)
        {
            throw new InvalidOperationException("Debug publication produced an invalid module/symbol manifest.", error);
        }
        if (root is null || root.Name != "LocalDebug" || (string?)root.Attribute("endpoint") != endpoint)
            throw new InvalidOperationException("Debug publication does not identify this session's debugger endpoint.");
        return new(ReadArtifact(directory, root.Element("Module")), ReadArtifact(directory, root.Element("Symbols")));
    }

    private static string ReadArtifact(string directory, XElement? element)
    {
        var name = (string?)element?.Attribute("file");
        var expected = (string?)element?.Attribute("sha256");
        if (string.IsNullOrWhiteSpace(name) || Path.GetFileName(name) != name || string.IsNullOrWhiteSpace(expected))
            throw new InvalidOperationException("Debug publication must identify each session artifact and its hash.");
        var path = Path.Combine(directory, name);
        using var contents = File.OpenRead(path);
        var actual = Convert.ToHexString(SHA256.HashData(contents));
        if (!string.Equals(actual, expected, StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException($"Debug artifact {name} does not match the published module/symbol pair.");
        return path;
    }

    private sealed record DebugArtifacts(string Module, string Symbols);
}
