using System;
using System.Security.Cryptography;
using Org.BouncyCastle.Security.Certificates;
using Org.BouncyCastle.X509;

namespace Wisp.Runtime;

/// <summary>Owned DER and signature values for the adapted certificate runtime; performs no trust validation.</summary>
public sealed class CertificateValue : IDisposable
{
    private byte[]? der;
    private X509Certificate? certificate;

    private CertificateValue(byte[] der, X509Certificate certificate)
    {
        this.der = der;
        this.certificate = certificate;
    }

    /// <summary>Copies and parses exactly one DER certificate.</summary>
    /// <exception cref="CryptographicException">The input is not exactly one DER certificate.</exception>
    public static CertificateValue Parse(ReadOnlySpan<byte> input)
    {
        byte[] der = input.ToArray();
        try
        {
            var certificate = new X509CertificateParser().ReadCertificate(der);
            if (certificate is null || !certificate.GetEncoded().AsSpan().SequenceEqual(der))
                throw new CryptographicException("Expected exactly one DER certificate.");
            return new CertificateValue(der, certificate);
        }
        catch (CertificateException error)
        {
            throw new CryptographicException("Invalid DER certificate.", error);
        }
    }

    /// <summary>Copies and parses DER bytes from the runtime's certificate-copy boundary.</summary>
    /// <exception cref="ArgumentNullException">The input is null.</exception>
    /// <exception cref="CryptographicException">The input is not exactly one DER certificate.</exception>
    public static CertificateValue ParseArray(byte[] input)
    {
        ArgumentNullException.ThrowIfNull(input);
        return Parse(input);
    }

    /// <summary>Returns independent original DER bytes; throws after disposal.</summary>
    public byte[] RawData
    {
        get
        {
            ObjectDisposedException.ThrowIf(der is null, this);
            return (byte[])der.Clone();
        }
    }

    /// <summary>Returns the parsed signature algorithm OID; throws after disposal.</summary>
    public string SignatureAlgorithm
    {
        get
        {
            ObjectDisposedException.ThrowIf(certificate is null, this);
            return certificate.SigAlgOid;
        }
    }

    /// <summary>Releases this certificate's retained values.</summary>
    public void Dispose()
    {
        der = null;
        certificate = null;
    }
}
