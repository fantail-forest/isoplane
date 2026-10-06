using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text;

namespace Isoplane.Cli;

/// <summary>A PEM bundle of trusted roots and the SHA-256 the session checks it against.</summary>
internal sealed record TrustedRoots(string Bundle, string Sha256);

/// <summary>
/// The certificate authorities this machine trusts, as .NET reports them,
/// exported for a guest that validates TLS itself. A development session
/// trusts what any other .NET program on the machine trusts.
/// </summary>
internal static class MachineTrust
{
    /// <summary>
    /// Writes the machine's roots beneath <paramref name="directory"/>. Returns
    /// null when .NET reports none, leaving the session on the platform's bundle.
    /// </summary>
    internal static TrustedRoots? Export(string directory)
    {
        var roots = new SortedDictionary<string, string>(StringComparer.Ordinal);
        var distrusted = Certificates(StoreName.Disallowed).Select(certificate => certificate.Thumbprint).ToHashSet(StringComparer.Ordinal);
        foreach (var certificate in Certificates(StoreName.Root).Concat(EnvironmentRoots()))
        {
            if (!distrusted.Contains(certificate.Thumbprint))
                roots[certificate.Thumbprint] = certificate.ExportCertificatePem();
            certificate.Dispose();
        }
        if (roots.Count == 0) return null;
        var pem = Encoding.ASCII.GetBytes(string.Join('\n', roots.Values) + "\n");
        Directory.CreateDirectory(directory);
        var bundle = Path.Combine(directory, "roots.pem");
        if (!File.Exists(bundle) || !File.ReadAllBytes(bundle).AsSpan().SequenceEqual(pem))
        {
            var partial = bundle + "." + Guid.NewGuid().ToString("N") + ".tmp";
            File.WriteAllBytes(partial, pem);
            File.Move(partial, bundle, overwrite: true);
        }
        return new(bundle, Convert.ToHexStringLower(SHA256.HashData(pem)));
    }

    private static IEnumerable<X509Certificate2> Certificates(StoreName name)
    {
        foreach (var location in new[] { StoreLocation.CurrentUser, StoreLocation.LocalMachine })
        {
            using var store = new X509Store(name, location);
            try { store.Open(OpenFlags.ReadOnly | OpenFlags.OpenExistingOnly); }
            // A store this platform or user does not have holds no certificates.
            catch (CryptographicException) { continue; }
            foreach (var certificate in store.Certificates) yield return certificate;
        }
    }

    /// <summary>
    /// Roots named by SSL_CERT_FILE and SSL_CERT_DIR where .NET does not read
    /// them itself. On Linux they already select the machine store's contents.
    /// </summary>
    private static IEnumerable<X509Certificate2> EnvironmentRoots()
    {
        if (OperatingSystem.IsLinux()) return [];
        var files = new List<string>();
        if (Environment.GetEnvironmentVariable("SSL_CERT_FILE") is { Length: > 0 } file) files.Add(file);
        foreach (var directory in (Environment.GetEnvironmentVariable("SSL_CERT_DIR") ?? "").Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
            if (Directory.Exists(directory)) files.AddRange(Directory.GetFiles(directory).Order(StringComparer.Ordinal));
        var roots = new X509Certificate2Collection();
        foreach (var path in files)
        {
            try { roots.ImportFromPemFile(path); }
            catch (Exception error) when (error is CryptographicException or IOException or UnauthorizedAccessException)
            {
                throw new ArgumentException($"Cannot read trusted roots from '{path}': {error.Message}", error);
            }
        }
        return roots;
    }
}
