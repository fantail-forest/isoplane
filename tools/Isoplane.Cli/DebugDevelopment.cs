using System.Security.Cryptography;
using System.Text.Json;
using System.Xml.Linq;
using Isoplane.Native;

namespace Isoplane.Cli;

internal static class DebugDevelopment
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var native = await Development.RuntimeProfileAsync(options, cancellation) == GuestRuntimeProfile.NativeAot;
        var ordinaryModule = new LocalProject(options.Directory).Module;
        var root = native ? Environment.GetEnvironmentVariable(NativeDebugAdapter.SessionDirectoryVariable) : null;
        var session = Path.Combine(root ?? Path.Combine(options.Directory, ".isoplane", "debug"), Guid.NewGuid().ToString("N"));
        try
        {
            if (OperatingSystem.IsWindows())
                Directory.CreateDirectory(session);
            else
                Directory.CreateDirectory(session, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            await using var relay = native ? null : new DebugRelay(cancellation);
            var endpoint = relay?.Endpoint.ToString() ?? "native";
            var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
            List<string> publication = ["publish", project, "-c", options.Configuration, "-o", session, "--disable-build-servers",
                "-p:IsoplaneLocalDebug=true", "-p:IsoplaneDebugEndpoint=" + endpoint];
            var result = await Processes.RunAsync("dotnet", publication, options.Directory, cancellation, redirectOutput: options.Json);
            if (result != 0)
                return result;
            var artifacts = ReadArtifacts(session, endpoint);
            if (Path.GetFullPath(artifacts.Module) == Path.GetFullPath(ordinaryModule))
                throw new InvalidOperationException("A debug session must not replace the ordinary application module.");
            if (options.Json)
                Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint, module = artifacts.Module, symbols = artifacts.Symbols, runtime = native ? "nativeaot" : "mono" }));
            else if (native)
                Console.WriteLine("NativeAOT module published for the native debugger. This session serves one HTTP request.");
            else
                Console.WriteLine($"Debugger target: {endpoint}. Connect your editor through isoplane debug-adapter; this session serves one HTTP request.");
            var projectInputs = new LocalProject(options.Directory);
            await using var host = new LocalSession(await File.ReadAllBytesAsync(artifacts.Module, cancellation),
                projectInputs.SessionOptions(options.Port, [], native ? null : endpoint, Path.Combine(session, "runtime"), nativeDebug: native),
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

    internal static async Task RunHostAsync(LocalSession host, DebugRelay? relay, CancellationToken cancellation)
    {
        using var stop = cancellation.Register(host.Stop);
        try
        {
            if (relay is not null && await Task.WhenAny(host.Completion, relay.Completion) == relay.Completion)
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
