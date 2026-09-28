using System.Formats.Tar;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Isoplane.Cli.Tests;

internal sealed class SessionFixture : IDisposable
{
    private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-native-session-");
    internal string ProjectDirectory => directory.FullName;
    // A minimal staged runtime isolates native session ownership; installed SDK tests exercise Mono.
    internal static byte[] Module => Convert.FromBase64String("AGFzbQEAAAABHAVgAn9/AX9gAn9/AX5gA35/fwF+YAABfmABfwADBwYAAQIDAwQFBAEBAQEHhAEHBm1lbW9yeQIADmlzb3BsYW5lX2FsbG9jAAAPaXNvcGxhbmVfaGFuZGxlAAENaXNvcGxhbmVfd2FrZQACG2lzb3BsYW5lX2luaXRpYWxpemVfcnVudGltZQADE2lzb3BsYW5lX2luaXRpYWxpemUABBBpc29wbGFuZV9nY19oaW50AAUKHQYEAEEACwQAQgELBABCAQsEAEIACwQAQgALAgAL");
    internal byte[] Options { get; }

    internal SessionFixture()
    {
        File.WriteAllText(Path.Combine(ProjectDirectory, "isoplane.toml"), "name='session'\n");
        var project = new LocalProject(ProjectDirectory);
        Directory.CreateDirectory(Path.GetDirectoryName(project.Package)!);
        using (var file = File.Create(project.Package))
        using (var archive = new TarWriter(file))
        {
            Write(archive, "application.json", "{}");
            Write(archive, "runtime.json", JsonSerializer.Serialize(new {
                sdkVersion = "0.0.0-fixture", modules = new Dictionary<string, object> {
                    ["Isoplane.MonoRuntime.wasm"] = new { sha256 = Convert.ToHexStringLower(SHA256.HashData(Module)) }
                }
            }));
        }
        Options = project.SessionOptions(0, []);
    }

    private static void Write(TarWriter archive, string name, string content)
    {
        using var data = new MemoryStream(Encoding.UTF8.GetBytes(content));
        archive.WriteEntry(new PaxTarEntry(TarEntryType.RegularFile, name) { DataStream = data });
    }

    public void Dispose() => directory.Delete(recursive: true);
}
