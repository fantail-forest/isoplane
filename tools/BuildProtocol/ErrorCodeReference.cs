namespace Isoplane.Build;

/// <summary>
/// Where the customer error code reference explains each <c>ISOPLANE</c> code: the help link of every
/// diagnostic the analyzers and the build report, which editors show on the code.
/// </summary>
internal static class ErrorCodeReference
{
    internal static string Link(string code) => "https://isoplane.dev/docs/reference/error-codes#" + code.ToLowerInvariant();
}
