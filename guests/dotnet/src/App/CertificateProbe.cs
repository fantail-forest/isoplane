#if ISOPLANE_CERTIFICATE_PROBE
using System;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

public sealed class CertificateProbe : IIsoplaneHandler
{
    private const string CertificateDer = "MIIDUTCCAjmgAwIBAgIUE2S/L/NQIM7QCl+zEdc6GsTDPPAwDQYJKoZIhvcNAQELBQAwGDEWMBQGA1UEAwwNV2lzcC1Mb2NhbC1DQTAeFw0yNjA5MDkxNDM0NTVaFw0yNjA5MTAxNDM0NTVaMBkxFzAVBgNVBAMMDnRscy1wcm9iZS50ZXN0MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAr656vdfHlU2Rjg7zQdpQG/TPZHz7EL3J8WXGx1mBZXa2crsiAEyVDRWFA4owoTDoYD65nemo2lZpWBXHmmLTQByWNrzcMaEhu7w2m4ty+zmKBoL9vgTFo2UfhiUmeTw4JtAwGBJE8Qz4MYTBuFcA/Y6SsoGPJC5WgFruHPRWbIt2ylkSNUShpsdKeXGdIZkBHnUrGL+L4S81Iuw3deO+YyTiWAb63TfDp/A2q5WyPR2PT1uRyb2knhRlNomL0xeOFEymKVWg10BXdSoHFk5WRVPvL8KHEInx4+qa7aGkPkNNNF4Bi1VwTMq84GPEWJ0XUgukJLYoDMiCqBHWrfQdRwIDAQABo4GRMIGOMBkGA1UdEQQSMBCCDnRscy1wcm9iZS50ZXN0MAwGA1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgWgMBMGA1UdJQQMMAoGCCsGAQUFBwMBMB0GA1UdDgQWBBTGIG2OaEJyMll7Gm6cVwrzErYZGzAfBgNVHSMEGDAWgBTkc6tf4R3X1ZdEGqC5LL5dMwpIWDANBgkqhkiG9w0BAQsFAAOCAQEAIjMKKVXy4tY0f2xOzpXzp0MhAQlXZ6D+sFRuJh0pJwQ+fDrxvOsg5tog/IY0No354Y7ftOFLRDrpar8DxD5bNTJ18gjv0gYkSYRSXAPnQ+oDYiyHBTt0K44bOUMLTI3yf5jJGna/C7BchItWnm5LpAztluDV7O0ANNxDRcIiaNMEsPB22qIqPF6S47U+XsvsRM/xw2xVYptkrI7hicpm0NzJGsUsUd0YuduICwPqaqBlPkUngdYJCl8he23Wffo52XwhR+8SoqqOcGSHTn3xTlF9h49CbA9ZaeO+ngkarcjteEp4KkbomZh2N2opWtMmKXmBFFepdwfzamtdS6Ff6w==";

    private static X509Certificate2? saved;

    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        byte[] der = Convert.FromBase64String(CertificateDer);
        string result;
        try
        {
            result = request.Path switch
            {
                "/values" => Values(der, false),
                "/capture" => Save(der),
                "/restored" => Restored(),
                "/legacy-values" => Values(der, true),
                "/malformed" => Load(new byte[] { 0x30, 0x01, 0xff }),
                "/empty" => Load(Array.Empty<byte>()),
                "/trailing" => Load(AppendByte(der)),
                "/handle" => Handle(der),
                "/from-handle" => FromHandle(),
                "/subject" => Subject(der),
                "/flags" => Flags(der),
                "/disposed" => Disposed(der),
                _ => throw new InvalidOperationException("Unknown certificate fixture operation.")
            };
        }
        catch (CryptographicException) { result = "CryptographicException"; }
        catch (PlatformNotSupportedException error) { result = error.Message; }
        return Task.FromResult(Response.Text(result));
    }

    private static string Values(byte[] der, bool legacy)
    {
        using var original = legacy ? new X509Certificate2(der) : X509CertificateLoader.LoadCertificate(der);
        using var copy = new X509Certificate2(original);
        original.Dispose();
        if (!copy.RawData.AsSpan().SequenceEqual(der))
            throw new InvalidOperationException("Certificate clone lost the original DER.");
        der[0] ^= 0xff;
        byte[] returned = copy.RawData;
        returned[0] ^= 0xff;
        byte[] raw = copy.GetRawCertData();
        if (!raw.AsSpan().SequenceEqual(Convert.FromBase64String(CertificateDer)))
            throw new InvalidOperationException("Certificate input or output exposed mutable ownership.");
        return Describe(copy);
    }

    private static string Save(byte[] der)
    {
        saved?.Dispose();
        saved = X509CertificateLoader.LoadCertificate(der);
        return "saved";
    }

    private static string Restored()
    {
        using var certificate = saved ?? throw new InvalidOperationException("Retained certificate is absent.");
        saved = null;
        return Describe(certificate);
    }

    private static string Describe(X509Certificate2 certificate)
    {
        byte[] raw = certificate.RawData;
        if (!raw.AsSpan().SequenceEqual(Convert.FromBase64String(CertificateDer)))
            throw new InvalidOperationException("Retained certificate lost the exact original DER.");
        using var hash = SHA256.Create();
        return certificate.SignatureAlgorithm.Value + ":" + certificate.SignatureAlgorithm.FriendlyName + ":" +
            Convert.ToHexString(hash.ComputeHash(raw));
    }

    private static string Load(byte[] der)
    {
        using var certificate = X509CertificateLoader.LoadCertificate(der);
        return Convert.ToHexString(certificate.RawData);
    }

    private static byte[] AppendByte(byte[] der)
    {
        var result = new byte[der.Length + 1];
        der.CopyTo(result, 0);
        return result;
    }

    private static string Handle(byte[] der)
    {
        using var certificate = X509CertificateLoader.LoadCertificate(der);
        return certificate.Handle.ToString();
    }

    private static string FromHandle()
    {
        using var certificate = new X509Certificate2(new IntPtr(1));
        return Convert.ToHexString(certificate.RawData);
    }

    private static string Subject(byte[] der)
    {
        using var certificate = X509CertificateLoader.LoadCertificate(der);
        return certificate.Subject;
    }

    private static string Flags(byte[] der)
    {
        using var certificate = new X509Certificate2(der, (string?)null, X509KeyStorageFlags.EphemeralKeySet);
        return Convert.ToHexString(certificate.RawData);
    }

    private static string Disposed(byte[] der)
    {
        var certificate = X509CertificateLoader.LoadCertificate(der);
        certificate.Dispose();
        certificate.Dispose();
        return Convert.ToHexString(certificate.RawData);
    }
}
#endif
