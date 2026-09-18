namespace Isoplane.Build;

internal static class AssetPaths
{
    internal static bool IsRelativeFile(string path) => path.Length != 0 &&
        !path.Contains('\\') && !path.Contains(':') && !path.Contains('\0') &&
        path.Split('/').All(segment => segment.Length != 0 && segment is not "." and not "..");

    internal static bool TryAdd(HashSet<string> files, string path)
    {
        if (!IsRelativeFile(path) || files.Contains(path) || files.Any(existing =>
            existing.StartsWith(path + "/", StringComparison.Ordinal) || path.StartsWith(existing + "/", StringComparison.Ordinal))) return false;
        return files.Add(path);
    }
}
