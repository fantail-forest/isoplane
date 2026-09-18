using System;
using System.Globalization;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Cryptography through the pack's native OpenSSL backend: digests, MACs and
/// key derivation match their standard vectors, symmetric and asymmetric
/// primitives round-trip and verify fixed signatures, certificates parse and
/// chain, and randomness is the host's entropy.
/// </summary>
internal sealed partial class Cryptography
{
    public Task<string> RunAsync(string[] path) => Task.FromResult(Run(path));

    private static string Run(string[] path)
    {
        switch (path[0])
        {
            case "backend":
                // The AEAD ciphers are only supported with a native backend;
                // the managed adaptation reported none of them.
                return AesGcm.IsSupported + " " + ChaCha20Poly1305.IsSupported + " " + AesCcm.IsSupported;
            case "md5":
                return Hex(MD5.HashData(Encoding.ASCII.GetBytes("abc")));
            case "encrypted-pkcs8":
                return EncryptedPkcs8();
            case "sha256":
                return Hex(SHA256.HashData(Encoding.ASCII.GetBytes(path[1])));
            case "hmac":
                return Hex(HMACSHA256.HashData(Encoding.ASCII.GetBytes("Jefe"), Encoding.ASCII.GetBytes("what do ya want for nothing?")));
            case "hkdf":
            {
                byte[] ikm = new byte[22];
                Array.Fill(ikm, (byte)0x0b);
                byte[] salt = Convert.FromHexString("000102030405060708090a0b0c");
                byte[] info = Convert.FromHexString("f0f1f2f3f4f5f6f7f8f9");
                return Hex(HKDF.DeriveKey(HashAlgorithmName.SHA256, ikm, 42, salt, info));
            }
            case "pbkdf2":
                return Hex(Rfc2898DeriveBytes.Pbkdf2("password", Encoding.ASCII.GetBytes("salt"), 1, HashAlgorithmName.SHA256, 32));
            case "aes-gcm":
            {
                if (!AesGcm.IsSupported) return "AesGcm unsupported";
                using var gcm = new AesGcm(new byte[16], 16);
                byte[] ciphertext = new byte[16];
                byte[] tag = new byte[16];
                gcm.Encrypt(new byte[12], new byte[16], ciphertext, tag);
                byte[] decrypted = new byte[16];
                gcm.Decrypt(new byte[12], ciphertext, tag, decrypted);
                return Hex(ciphertext) + " " + Hex(tag) + " " + Hex(decrypted);
            }
            case "aes-cbc":
            {
                using Aes aes = Aes.Create();
                aes.KeySize = 256;
                aes.GenerateKey();
                aes.GenerateIV();
                byte[] plaintext = Encoding.ASCII.GetBytes("isoplane acceptance plaintext");
                byte[] encrypted = aes.EncryptCbc(plaintext, aes.IV);
                byte[] decrypted = aes.DecryptCbc(encrypted, aes.IV);
                return Encoding.ASCII.GetString(decrypted) + " " + (encrypted.Length % 16 == 0 && !encrypted.AsSpan().SequenceEqual(plaintext));
            }
            case "rsa-verify":
            {
                using RSA rsa = RSA.Create();
                rsa.ImportFromPem(CryptoMaterial.LeafKey);
                byte[] message = Encoding.ASCII.GetBytes("isoplane");
                byte[] signature = rsa.SignData(message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
                bool verified = rsa.VerifyData(message, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
                byte[] pss = rsa.SignData(message, HashAlgorithmName.SHA256, RSASignaturePadding.Pss);
                bool pssVerified = rsa.VerifyData(message, pss, HashAlgorithmName.SHA256, RSASignaturePadding.Pss);
                bool tampered = rsa.VerifyData(Encoding.ASCII.GetBytes("wasp"), signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
                return Hex(signature) + " " + verified + " " + pssVerified + " " + tampered;
            }
            case "rsa-oaep":
            {
                using RSA rsa = RSA.Create();
                rsa.ImportFromPem(CryptoMaterial.LeafKey);
                byte[] secret = Encoding.ASCII.GetBytes("a secret for the host");
                byte[] wrapped = rsa.Encrypt(secret, RSAEncryptionPadding.OaepSHA256);
                return Encoding.ASCII.GetString(rsa.Decrypt(wrapped, RSAEncryptionPadding.OaepSHA256)) + " " + wrapped.Length.ToString(CultureInfo.InvariantCulture);
            }
            case "rsa-generate":
            {
                using RSA rsa = RSA.Create(2048);
                byte[] message = Encoding.ASCII.GetBytes("generated");
                byte[] signature = rsa.SignData(message, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
                return rsa.KeySize + " " + rsa.VerifyData(message, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
            }
            case "ecdsa":
            {
                using ECDsa fixedKey = ECDsa.Create();
                fixedKey.ImportFromPem(CryptoMaterial.EcKey);
                byte[] message = Encoding.ASCII.GetBytes("isoplane");
                byte[] signature = fixedKey.SignData(message, HashAlgorithmName.SHA256);
                using ECDsa publicOnly = ECDsa.Create(fixedKey.ExportParameters(false));
                bool verified = publicOnly.VerifyData(message, signature, HashAlgorithmName.SHA256);
                bool tampered = publicOnly.VerifyData(Encoding.ASCII.GetBytes("wasp"), signature, HashAlgorithmName.SHA256);
                using ECDsa generated = ECDsa.Create(ECCurve.NamedCurves.nistP256);
                bool foreign = generated.VerifyData(message, signature, HashAlgorithmName.SHA256);
                return fixedKey.KeySize + " " + verified + " " + tampered + " " + foreign;
            }
            case "ecdh":
            {
                using ECDiffieHellman alice = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
                using ECDiffieHellman bob = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
                byte[] fromAlice = alice.DeriveKeyMaterial(bob.PublicKey);
                byte[] fromBob = bob.DeriveKeyMaterial(alice.PublicKey);
                return fromAlice.AsSpan().SequenceEqual(fromBob) + " " + fromAlice.Length.ToString(CultureInfo.InvariantCulture);
            }
            case "x509-parse":
            {
                using var leaf = X509Certificate2.CreateFromPem(CryptoMaterial.LeafCertificate);
                using RSA? publicKey = leaf.GetRSAPublicKey();
                bool verified = publicKey != null && publicKey.VerifyData(Encoding.ASCII.GetBytes("isoplane"), Convert.FromHexString(path[1]), HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
                return leaf.Subject + "|" + leaf.Thumbprint + "|" + leaf.SerialNumber + "|" + leaf.SignatureAlgorithm.FriendlyName + "|" + verified;
            }
            case "x509-chain":
            {
                using var leaf = X509Certificate2.CreateFromPem(CryptoMaterial.LeafCertificate);
                using var root = X509Certificate2.CreateFromPem(CryptoMaterial.RootCertificate);
                using var stranger = X509Certificate2.CreateFromPem(CryptoMaterial.StrangerCertificate);
                return Chain(leaf, root) + " " + Chain(leaf, stranger);
            }
            case "x509-sign-verify":
            {
                // The leaf's signature is verified against its issuer's key.
                using var leaf = X509Certificate2.CreateFromPem(CryptoMaterial.LeafCertificate);
                using var root = X509Certificate2.CreateFromPem(CryptoMaterial.RootCertificate);
                using RSA? rootKey = root.GetRSAPublicKey();
                byte[] tbs = leaf.RawDataMemory.Span.ToArray();
                var signed = new System.Formats.Asn1.AsnReader(tbs, System.Formats.Asn1.AsnEncodingRules.DER).ReadSequence();
                byte[] toBeSigned = signed.ReadEncodedValue().ToArray();
                signed.ReadSequence();
                byte[] signature = signed.ReadBitString(out _);
                return (rootKey != null && rootKey.VerifyData(toBeSigned, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1)).ToString();
            }
            case "random":
                return Hex(RandomNumberGenerator.GetBytes(32)) + " " + RandomNumberGenerator.GetInt32(1000, 2000).ToString(CultureInfo.InvariantCulture);
            case "aead":
                return Aead(path[1], path[2]);
            case "aes-cbc-invalid-padding":
                return AesCbcInvalidPadding();
            case "aes-cbc-invalid-lengths":
                return AesCbcInvalidLengths();
            case "rsa-tampered-ciphertext":
                return RsaTamperedCiphertext();
            case "ecdsa-malformed-signatures":
                return EcdsaMalformedSignatures();
            case "rsa-import-export":
                return RsaImportExport();
            case "ecdsa-import-export":
                return EcdsaImportExport();
            case "ecdh-import-export":
                return EcdhImportExport();
            case "malformed-key-imports":
                return MalformedKeyImports();
            case "disposed-keys":
                return DisposedKeys();
            case "disposal-is-per-instance":
                return DisposalIsPerInstance();
            default:
                throw new ArgumentException("unknown crypto case " + path[0]);
        }
    }

    private static string Chain(X509Certificate2 leaf, X509Certificate2 trustedRoot)
    {
        using var chain = new X509Chain();
        chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
        chain.ChainPolicy.CustomTrustStore.Add(trustedRoot);
        chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
        bool built = chain.Build(leaf);
        string status = chain.ChainStatus.Length == 0 ? "ok" : chain.ChainStatus[0].Status.ToString();
        return built + ":" + chain.ChainElements.Count.ToString(CultureInfo.InvariantCulture) + ":" + status;
    }

    private static string Hex(byte[] bytes) => Convert.ToHexString(bytes).ToLowerInvariant();

    /// <summary>
    /// The nearest public type of the exception <paramref name="action"/>
    /// throws, the one a caller can catch (OpenSSL failures surface as an
    /// internal subclass of CryptographicException); the answer a test
    /// asserts on.
    /// </summary>
    /// <summary>
    /// An RSA key exported under a password as DER and as PEM, imported
    /// back and proven by a signature, and the wrong password refused.
    /// </summary>
    private static string EncryptedPkcs8()
    {
        using var key = RSA.Create(2048);
        byte[] data = Encoding.ASCII.GetBytes("encrypted pkcs8");
        byte[] signature = key.SignData(data, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        var parameters = new PbeParameters(PbeEncryptionAlgorithm.Aes256Cbc, HashAlgorithmName.SHA256, 10_000);
        byte[] der = key.ExportEncryptedPkcs8PrivateKey("correct horse", parameters);
        string pem = key.ExportEncryptedPkcs8PrivateKeyPem("correct horse", parameters);
        using var fromDer = RSA.Create();
        fromDer.ImportEncryptedPkcs8PrivateKey("correct horse", der, out _);
        using var fromPem = RSA.Create();
        fromPem.ImportFromEncryptedPem(pem, "correct horse");
        string derOk = fromDer.VerifyData(data, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1) ? "ok" : "mismatch";
        string pemOk = fromPem.VerifyData(data, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1) ? "ok" : "mismatch";
        string wrong;
        try
        {
            using var stranger = RSA.Create();
            stranger.ImportEncryptedPkcs8PrivateKey("wrong", der, out _);
            wrong = "accepted";
        }
        catch (CryptographicException)
        {
            wrong = "CryptographicException";
        }
        return "der=" + derOk + " pem=" + pemOk + " wrong-password=" + wrong;
    }

    private static string Failure(Action action)
    {
        try
        {
            action();
            return "no exception";
        }
        catch (Exception error)
        {
            Type type = error.GetType();
            while (!type.IsPublic && type.BaseType != null) type = type.BaseType;
            return type.Name;
        }
    }
}
