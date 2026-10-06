using System.Diagnostics;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class MachineTrustTests
{
    [Fact]
    public async Task a_root_named_by_SSL_CERT_FILE_is_exported_for_the_session()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-machine-trust-");
        try
        {
            using var key = RSA.Create(2048);
            var request = new CertificateRequest("CN=Isoplane machine trust test", key, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
            request.CertificateExtensions.Add(new X509BasicConstraintsExtension(true, false, 0, true));
            using var authority = request.CreateSelfSigned(DateTimeOffset.UtcNow.AddDays(-1), DateTimeOffset.UtcNow.AddDays(1));
            var file = Path.Combine(directory.FullName, "authority.pem");
            await File.WriteAllTextAsync(file, authority.ExportCertificatePem() + "\n");

            // .NET reads its root store once per process, so the export runs in a child that starts with the variable set.
            var start = new ProcessStartInfo("dotnet") { RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false };
            start.Environment["SSL_CERT_FILE"] = file;
            foreach (var argument in new[] { typeof(OwnedChild).Assembly.Location, "--export-machine-trust", Path.Combine(directory.FullName, "trust") })
                start.ArgumentList.Add(argument);
            using var process = Process.Start(start)!;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
            var errors = process.StandardError.ReadToEndAsync(deadline.Token);
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, "Exporting machine trust must succeed: " + await errors);

            var bundle = JsonSerializer.Deserialize<string>(await output)!;
            var roots = new X509Certificate2Collection();
            roots.ImportFromPemFile(bundle);
            Assert.Contains(roots, root => root.Thumbprint == authority.Thumbprint);
        }
        finally { directory.Delete(recursive: true); }
    }
}
