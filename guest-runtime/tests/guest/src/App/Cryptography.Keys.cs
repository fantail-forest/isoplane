using System;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text;

namespace Acceptance;

/// <summary>
/// Key material: the import and export formats round-trip, malformed input
/// is refused, and a disposed key is unusable without affecting any other.
/// </summary>
internal sealed partial class Cryptography
{
    private static readonly byte[] Message = Encoding.ASCII.GetBytes("isoplane");

    /// <summary>
    /// Each format of the leaf key: a private import signs and verifies and
    /// carries the same modulus; a public import verifies and cannot export a
    /// private key.
    /// </summary>
    private static string RsaImportExport()
    {
        using RSA original = RSA.Create();
        original.ImportFromPem(CryptoMaterial.LeafKey);
        byte[] signature = original.SignData(Message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        byte[] modulus = original.ExportParameters(false).Modulus!;

        string Private(Action<RSA> import)
        {
            using RSA key = RSA.Create();
            import(key);
            bool verifies = key.VerifyData(Message, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
            bool signs = original.VerifyData(Message, key.SignData(Message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1), HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
            return verifies + "/" + signs + "/" + key.ExportParameters(false).Modulus.AsSpan().SequenceEqual(modulus);
        }

        string Public(Action<RSA> import)
        {
            using RSA key = RSA.Create();
            import(key);
            bool verifies = key.VerifyData(Message, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
            return verifies + "/" + Failure(() => key.ExportPkcs8PrivateKey());
        }

        byte[] pkcs8 = original.ExportPkcs8PrivateKey();
        byte[] pkcs1 = original.ExportRSAPrivateKey();
        byte[] spki = original.ExportSubjectPublicKeyInfo();
        byte[] rsaPublic = original.ExportRSAPublicKey();
        string pkcs8Pem = original.ExportPkcs8PrivateKeyPem();
        string pkcs1Pem = original.ExportRSAPrivateKeyPem();
        string spkiPem = original.ExportSubjectPublicKeyInfoPem();
        string rsaPublicPem = original.ExportRSAPublicKeyPem();
        return "pkcs8=" + Private(key => key.ImportPkcs8PrivateKey(pkcs8, out _))
            + " pkcs1=" + Private(key => key.ImportRSAPrivateKey(pkcs1, out _))
            + " spki=" + Public(key => key.ImportSubjectPublicKeyInfo(spki, out _))
            + " rsa-public=" + Public(key => key.ImportRSAPublicKey(rsaPublic, out _))
            + " pkcs8-pem=" + Private(key => key.ImportFromPem(pkcs8Pem))
            + " pkcs1-pem=" + Private(key => key.ImportFromPem(pkcs1Pem))
            + " spki-pem=" + Public(key => key.ImportFromPem(spkiPem))
            + " rsa-public-pem=" + Public(key => key.ImportFromPem(rsaPublicPem))
            + " parameters=" + Private(key => key.ImportParameters(original.ExportParameters(true)));
    }

    /// <summary>
    /// Each format of the fixed P-256 key: a private import signs and
    /// verifies and carries the same public point; a public import verifies
    /// and cannot export a private key.
    /// </summary>
    private static string EcdsaImportExport()
    {
        using ECDsa original = ECDsa.Create();
        original.ImportFromPem(CryptoMaterial.EcKey);
        byte[] signature = original.SignData(Message, HashAlgorithmName.SHA256);
        byte[] x = original.ExportParameters(false).Q.X!;

        string Private(Action<ECDsa> import)
        {
            using ECDsa key = ECDsa.Create();
            import(key);
            bool verifies = key.VerifyData(Message, signature, HashAlgorithmName.SHA256);
            bool signs = original.VerifyData(Message, key.SignData(Message, HashAlgorithmName.SHA256), HashAlgorithmName.SHA256);
            return verifies + "/" + signs + "/" + key.ExportParameters(false).Q.X.AsSpan().SequenceEqual(x);
        }

        string Public(Action<ECDsa> import)
        {
            using ECDsa key = ECDsa.Create();
            import(key);
            bool verifies = key.VerifyData(Message, signature, HashAlgorithmName.SHA256);
            return verifies + "/" + Failure(() => key.ExportPkcs8PrivateKey());
        }

        byte[] pkcs8 = original.ExportPkcs8PrivateKey();
        byte[] ecPrivate = original.ExportECPrivateKey();
        byte[] spki = original.ExportSubjectPublicKeyInfo();
        string pkcs8Pem = original.ExportPkcs8PrivateKeyPem();
        string ecPrivatePem = original.ExportECPrivateKeyPem();
        string spkiPem = original.ExportSubjectPublicKeyInfoPem();
        return "pkcs8=" + Private(key => key.ImportPkcs8PrivateKey(pkcs8, out _))
            + " ec-private=" + Private(key => key.ImportECPrivateKey(ecPrivate, out _))
            + " spki=" + Public(key => key.ImportSubjectPublicKeyInfo(spki, out _))
            + " pkcs8-pem=" + Private(key => key.ImportFromPem(pkcs8Pem))
            + " ec-private-pem=" + Private(key => key.ImportFromPem(ecPrivatePem))
            + " spki-pem=" + Public(key => key.ImportFromPem(spkiPem))
            + " parameters=" + Private(key => key.ImportParameters(original.ExportParameters(true)));
    }

    /// <summary>
    /// A generated P-256 agreement key: a private import derives the shared
    /// secret from both sides; a public import lets the peer derive it and
    /// cannot export a private key.
    /// </summary>
    private static string EcdhImportExport()
    {
        using ECDiffieHellman alice = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
        using ECDiffieHellman bob = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
        byte[] secret = alice.DeriveKeyMaterial(bob.PublicKey);

        string Private(Action<ECDiffieHellman> import)
        {
            using ECDiffieHellman key = ECDiffieHellman.Create();
            import(key);
            return key.DeriveKeyMaterial(bob.PublicKey).AsSpan().SequenceEqual(secret) + "/" + bob.DeriveKeyMaterial(key.PublicKey).AsSpan().SequenceEqual(secret);
        }

        string Public(Action<ECDiffieHellman> import)
        {
            using ECDiffieHellman key = ECDiffieHellman.Create();
            import(key);
            return bob.DeriveKeyMaterial(key.PublicKey).AsSpan().SequenceEqual(secret) + "/" + Failure(() => key.ExportPkcs8PrivateKey());
        }

        byte[] pkcs8 = alice.ExportPkcs8PrivateKey();
        byte[] ecPrivate = alice.ExportECPrivateKey();
        byte[] spki = alice.ExportSubjectPublicKeyInfo();
        byte[] publicKeySpki = alice.PublicKey.ExportSubjectPublicKeyInfo();
        string pkcs8Pem = alice.ExportPkcs8PrivateKeyPem();
        string spkiPem = alice.ExportSubjectPublicKeyInfoPem();
        return "pkcs8=" + Private(key => key.ImportPkcs8PrivateKey(pkcs8, out _))
            + " ec-private=" + Private(key => key.ImportECPrivateKey(ecPrivate, out _))
            + " spki=" + Public(key => key.ImportSubjectPublicKeyInfo(spki, out _))
            + " public-key-spki=" + Public(key => key.ImportSubjectPublicKeyInfo(publicKeySpki, out _))
            + " pkcs8-pem=" + Private(key => key.ImportFromPem(pkcs8Pem))
            + " spki-pem=" + Public(key => key.ImportFromPem(spkiPem))
            + " parameters=" + Private(key => key.ImportParameters(alice.ExportParameters(true)));
    }

    /// <summary>
    /// Bytes that are not a key, a key truncated, a key of the other
    /// algorithm, a public key where a private one is expected, text that is
    /// not PEM, and a certificate PEM where a key PEM is expected.
    /// </summary>
    private static string MalformedKeyImports()
    {
        using RSA leaf = RSA.Create();
        leaf.ImportFromPem(CryptoMaterial.LeafKey);
        byte[] rsaPkcs8 = leaf.ExportPkcs8PrivateKey();
        byte[] rsaSpki = leaf.ExportSubjectPublicKeyInfo();
        using ECDsa ec = ECDsa.Create();
        ec.ImportFromPem(CryptoMaterial.EcKey);
        byte[] ecPkcs8 = ec.ExportPkcs8PrivateKey();
        byte[] garbage = Encoding.ASCII.GetBytes("not a key at all");
        byte[] truncated = rsaPkcs8.AsSpan(0, rsaPkcs8.Length - 32).ToArray();
        return "garbage-pkcs8=" + Rsa(key => key.ImportPkcs8PrivateKey(garbage, out _))
            + " garbage-spki=" + Rsa(key => key.ImportSubjectPublicKeyInfo(garbage, out _))
            + " truncated-pkcs8=" + Rsa(key => key.ImportPkcs8PrivateKey(truncated, out _))
            + " spki-as-pkcs8=" + Rsa(key => key.ImportPkcs8PrivateKey(rsaSpki, out _))
            + " ec-key-as-rsa=" + Rsa(key => key.ImportPkcs8PrivateKey(ecPkcs8, out _))
            + " rsa-key-as-ec=" + Ec(key => key.ImportPkcs8PrivateKey(rsaPkcs8, out _))
            + " rsa-pem-as-ec=" + Ec(key => key.ImportFromPem(CryptoMaterial.LeafKey))
            + " not-pem=" + Rsa(key => key.ImportFromPem("not a key at all"))
            + " certificate-pem=" + Rsa(key => key.ImportFromPem(CryptoMaterial.LeafCertificate));
    }

    private static string Rsa(Action<RSA> action) => Failure(() =>
    {
        using RSA key = RSA.Create();
        action(key);
    });

    private static string Ec(Action<ECDsa> action) => Failure(() =>
    {
        using ECDsa key = ECDsa.Create();
        action(key);
    });

    /// <summary>
    /// A disposed key refuses to export, sign or agree; disposing twice is
    /// allowed.
    /// </summary>
    private static string DisposedKeys()
    {
        RSA rsa = RSA.Create();
        rsa.ImportFromPem(CryptoMaterial.LeafKey);
        rsa.Dispose();
        rsa.Dispose();
        ECDsa ecdsa = ECDsa.Create();
        ecdsa.ImportFromPem(CryptoMaterial.EcKey);
        ecdsa.Dispose();
        ECDiffieHellman ecdh = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
        using ECDiffieHellman peer = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
        ecdh.Dispose();
        return "rsa-pkcs8=" + Failure(() => rsa.ExportPkcs8PrivateKey())
            + " rsa-spki=" + Failure(() => rsa.ExportSubjectPublicKeyInfo())
            + " rsa-pem=" + Failure(() => rsa.ExportRSAPrivateKeyPem())
            + " rsa-sign=" + Failure(() => rsa.SignData(Message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1))
            + " ecdsa-pkcs8=" + Failure(() => ecdsa.ExportPkcs8PrivateKey())
            + " ecdsa-spki=" + Failure(() => ecdsa.ExportSubjectPublicKeyInfo())
            + " ecdsa-sign=" + Failure(() => ecdsa.SignData(Message, HashAlgorithmName.SHA256))
            + " ecdh-pkcs8=" + Failure(() => ecdh.ExportPkcs8PrivateKey())
            + " ecdh-derive=" + Failure(() => ecdh.DeriveKeyMaterial(peer.PublicKey));
    }

    /// <summary>
    /// Disposing a key leaves a key built from its parameters, bytes exported
    /// from it, and the certificate it was taken from all usable.
    /// </summary>
    private static string DisposalIsPerInstance()
    {
        RSA original = RSA.Create();
        original.ImportFromPem(CryptoMaterial.LeafKey);
        using RSA copy = RSA.Create(original.ExportParameters(true));
        byte[] exported = original.ExportPkcs8PrivateKey();
        original.Dispose();

        bool Signs(RSA key) => key.VerifyData(Message, key.SignData(Message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1), HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);

        using RSA fromBytes = RSA.Create();
        fromBytes.ImportPkcs8PrivateKey(exported, out _);
        using var leaf = X509Certificate2.CreateFromPem(CryptoMaterial.LeafCertificate, CryptoMaterial.LeafKey);
        RSA? first = leaf.GetRSAPrivateKey();
        first?.Dispose();
        using RSA? second = leaf.GetRSAPrivateKey();
        return "copy=" + Signs(copy) + " exported=" + Signs(fromBytes) + " certificate=" + (second != null && Signs(second));
    }
}
