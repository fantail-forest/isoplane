using System.Diagnostics;
using Isoplane.TestSupport;
using System.Net;
using System.Net.Security;
using System.Net.Sockets;
using System.Security.Authentication;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CertificateTrustTests
{
    [Theory]
    [InlineData("trusted")]
    [InlineData("untrusted")]
    [InlineData("wrong-host")]
    [InlineData("expired")]
    [InlineData("wrong-usage")]
    public async Task An_explicit_bundle_preserves_certificate_and_hostname_validation(string scenario)
    {
        if (OperatingSystem.IsMacOS())
        {
            await VerifyInNativeTemporaryDirectory(scenario);
            return;
        }
        await VerifyBundleAsync(scenario);
    }

    internal static async Task VerifyBundleAsync(string scenario)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-ca-bundle-");
        try
        {
            using var authority = Authority();
            using var unrelated = scenario == "untrusted" ? Authority() : null;
            using var certificate = ServerCertificate(authority, scenario);
            var path = Path.Combine(directory.FullName, "roots.pem");
            await File.WriteAllTextAsync(path, (unrelated ?? authority).ExportCertificatePem());
            using var trust = new CertificateTrust(path);
            using var handler = new HttpClientHandler { UseProxy = false };
            trust.Configure(handler);
            using var client = new HttpClient(handler);
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
            using var listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            var address = new Uri($"https://127.0.0.1:{((IPEndPoint)listener.LocalEndpoint).Port}/");
            var rejected = scenario != "trusted";
            var serving = Serve(listener, certificate, rejected, deadline.Token);
            try
            {
                if (rejected)
                    await Assert.ThrowsAsync<HttpRequestException>(() => client.GetStringAsync(address, deadline.Token));
                else
                    Assert.Equal("ok", await client.GetStringAsync(address, deadline.Token));
            }
            finally
            {
                deadline.Cancel();
                listener.Stop();
                await serving;
            }
        }
        finally { directory.Delete(true); }
    }

    [Theory]
    [InlineData("")]
    [InlineData("-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----")]
    public void An_empty_or_invalid_explicit_bundle_is_refused(string contents)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-ca-bundle-");
        try
        {
            var path = Path.Combine(directory.FullName, "roots.pem");
            File.WriteAllText(path, contents);
            Assert.Throws<ArgumentException>(() => new CertificateTrust(path));
        }
        finally { directory.Delete(true); }
    }

    private static async Task VerifyInNativeTemporaryDirectory(string scenario)
    {
        using var directory = new FileSystemTestDirectory("isoplane-tls-keys-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        var start = new ProcessStartInfo("dotnet") { RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false };
        start.ArgumentList.Add(typeof(CertificateTrustTests).Assembly.Location);
        start.ArgumentList.Add("--verify-ca-bundle");
        start.ArgumentList.Add(scenario);
        // .NET creates disposable Apple keychains under TMPDIR, without a per-certificate path option.
        start.Environment["TMPDIR"] = directory.Path;
        using var child = Process.Start(start) ?? throw new InvalidOperationException("Cannot start the TLS certificate fixture.");
        var output = child.StandardOutput.ReadToEndAsync();
        var errors = child.StandardError.ReadToEndAsync();
        try
        {
            await child.WaitForExitAsync(deadline.Token);
            Assert.True(child.ExitCode == 0, $"Certificate scenario '{scenario}' failed: " + await output + await errors);
        }
        finally
        {
            if (!child.HasExited) child.Kill(entireProcessTree: true);
            await child.WaitForExitAsync();
        }
    }

    private static X509Certificate2 Authority()
    {
        using var key = RSA.Create(2048);
        var request = new CertificateRequest("CN=Isoplane test CA", key, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        request.CertificateExtensions.Add(new X509BasicConstraintsExtension(true, false, 0, true));
        request.CertificateExtensions.Add(new X509KeyUsageExtension(X509KeyUsageFlags.KeyCertSign, true));
        return request.CreateSelfSigned(DateTimeOffset.UtcNow.AddDays(-1), DateTimeOffset.UtcNow.AddDays(7));
    }

    private static X509Certificate2 ServerCertificate(X509Certificate2 authority, string scenario)
    {
        using var key = RSA.Create(2048);
        var request = new CertificateRequest("CN=Isoplane TLS test", key, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        var names = new SubjectAlternativeNameBuilder();
        if (scenario == "wrong-host") names.AddDnsName("wrong.example.test");
        else names.AddIpAddress(IPAddress.Loopback);
        request.CertificateExtensions.Add(names.Build());
        request.CertificateExtensions.Add(new X509BasicConstraintsExtension(false, false, 0, true));
        request.CertificateExtensions.Add(new X509EnhancedKeyUsageExtension(new OidCollection
        {
            new(scenario == "wrong-usage" ? "1.3.6.1.5.5.7.3.2" : "1.3.6.1.5.5.7.3.1")
        }, true));
        using var issued = request.Create(authority, DateTimeOffset.UtcNow.AddHours(-2),
            DateTimeOffset.UtcNow.AddHours(scenario == "expired" ? -1 : 1), RandomNumberGenerator.GetBytes(16));
        if (!OperatingSystem.IsWindows()) return issued.CopyWithPrivateKey(key);
        using var withKey = issued.CopyWithPrivateKey(key);
        // Schannel needs an imported key; disposing the certificate removes its temporary key container.
        return X509CertificateLoader.LoadPkcs12(withKey.Export(X509ContentType.Pfx), null);
    }

    private static async Task Serve(TcpListener listener, X509Certificate2 certificate, bool rejected, CancellationToken cancellation)
    {
        using var connection = await listener.AcceptTcpClientAsync(cancellation);
        await using var stream = new SslStream(connection.GetStream());
        try
        {
            await stream.AuthenticateAsServerAsync(new SslServerAuthenticationOptions { ServerCertificate = certificate }, cancellation);
            using var reader = new StreamReader(stream, leaveOpen: true);
            while (await reader.ReadLineAsync(cancellation) is { Length: > 0 }) { }
            await stream.WriteAsync("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"u8.ToArray(), cancellation);
        }
        catch (Exception error) when (rejected && error is AuthenticationException or IOException or OperationCanceledException)
        {
            // A rejected handshake can surface as a TLS alert, EOF, or cancellation after the client refuses it.
            return;
        }
    }
}
