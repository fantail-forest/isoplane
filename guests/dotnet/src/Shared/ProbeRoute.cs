namespace Isoplane.App;

/// <summary>Splits a request path into the probe its first segment names and the path that probe sees.</summary>
internal static class ProbeRoute
{
    public static (string Probe, string Path) Split(string path)
    {
        if (!path.StartsWith('/')) return ("", path);
        int end = path.IndexOf('/', 1);
        return end < 0 ? (path[1..], "/") : (path[1..end], path[end..]);
    }
}
