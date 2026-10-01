using System.Net.Security;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;

namespace Isoplane.Cli;

/// <summary>An explicit PEM trust bundle, scoped to the clients using it.</summary>
internal sealed class CertificateTrust : IDisposable
{
    private readonly X509Certificate2Collection roots = [];

    internal CertificateTrust(string? path)
    {
        if (string.IsNullOrEmpty(path)) return;
        try
        {
            roots.ImportFromPemFile(path);
            if (roots.Count == 0)
                throw new ArgumentException($"The CA bundle '{path}' contains no PEM certificates.");
        }
        catch (Exception error)
        {
            Dispose();
            if (error is CryptographicException)
                throw new ArgumentException($"The CA bundle '{path}' contains an invalid certificate.", error);
            throw;
        }
    }

    internal static CertificateTrust FromEnvironment() => new(Environment.GetEnvironmentVariable("SSL_CERT_FILE"));

    internal static HttpClientHandler CreateHandler(bool allowAutoRedirect) => new OwnedHandler(allowAutoRedirect);

    internal void Configure(HttpClientHandler handler)
    {
        if (roots.Count == 0) return;
        handler.ServerCertificateCustomValidationCallback = (_, certificate, chain, errors) =>
        {
            if (certificate is null || chain is null || (errors & ~SslPolicyErrors.RemoteCertificateChainErrors) != SslPolicyErrors.None)
                return false;
            chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
            chain.ChainPolicy.CustomTrustStore.Clear();
            chain.ChainPolicy.CustomTrustStore.AddRange(roots);
            return chain.Build(certificate);
        };
    }

    public void Dispose()
    {
        foreach (var certificate in roots) certificate.Dispose();
    }

    private sealed class OwnedHandler : HttpClientHandler
    {
        private readonly CertificateTrust trust;

        internal OwnedHandler(bool allowAutoRedirect)
        {
            trust = FromEnvironment();
            AllowAutoRedirect = allowAutoRedirect;
            trust.Configure(this);
        }

        protected override void Dispose(bool disposing)
        {
            try { base.Dispose(disposing); }
            finally { if (disposing) trust.Dispose(); }
        }
    }
}
