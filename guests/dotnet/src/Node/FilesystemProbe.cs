using System;
using System.IO;
using System.Reflection;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises the managed view of the empty read-only filesystem.</summary>
public static class FilesystemProbe
{
    private static string InitialDirectory = "";

    public static Task InitializeAsync()
    {
        InitialDirectory = Directory.GetCurrentDirectory();
        return Task.CompletedTask;
    }

    public static Task<Response> HandleAsync(string path)
    {
        string result;
        try
        {
            result = path switch
            {
                "/root" => $"{InitialDirectory}|{Directory.Exists("/")}|{string.Join(",", Directory.GetFileSystemEntries("/"))}",
                "/missing" => $"{File.Exists("/etc/passwd")}|{Directory.Exists("/tmp")}|{File.Exists("../../etc/passwd")}",
                "/read-root-file" => File.ReadAllText("/missing"),
                "/resolve" => Path.GetFullPath("../missing"),
                "/enumerate-missing" => string.Join(",", Directory.GetFileSystemEntries("/missing")),
                "/read" => File.ReadAllText("/etc/passwd"),
                "/write" => Write(),
                "/mkdir" => Directory.CreateDirectory("/created").FullName,
                "/delete" => Delete(),
                "/move" => Move(),
                "/symlink" => File.CreateSymbolicLink("/link", "/etc/passwd").FullName,
                "/temp" => $"{Path.GetTempPath()}|{Directory.Exists(Path.GetTempPath())}",
                "/tempfile" => Path.GetTempFileName(),
                "/resource" => Resource(),
                "/assembly" => typeof(Stream).Assembly.GetName().Name ?? "missing assembly",
                _ => "unknown probe",
            };
        }
        catch (Exception error)
        {
            result = $"{error.GetType().Name}:{error.HResult:X8}";
        }
        return Task.FromResult(Response.Text(result));
    }

    private static string Write() { File.WriteAllText("/created", "forbidden"); return "write succeeded"; }
    private static string Delete() { File.Delete("/created"); return "delete succeeded"; }
    private static string Move() { File.Move("/source", "/destination"); return "move succeeded"; }
    private static string Resource()
    {
        using Stream resource = Assembly.GetExecutingAssembly().GetManifestResourceStream("filesystem-resource")
            ?? throw new InvalidOperationException("the packaged resource is missing");
        using var reader = new StreamReader(resource);
        return reader.ReadToEnd();
    }
}
