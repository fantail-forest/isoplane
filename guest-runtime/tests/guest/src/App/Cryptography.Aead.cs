using System;
using System.Security.Cryptography;
using System.Text;

namespace Acceptance;

/// <summary>
/// The authenticated ciphers and AES-CBC: known answers, and what a
/// tampered input, a wrong length and an invalid padding produce.
/// </summary>
internal sealed partial class Cryptography
{
    private delegate void Seal(ReadOnlySpan<byte> nonce, ReadOnlySpan<byte> plaintext, Span<byte> ciphertext, Span<byte> tag, ReadOnlySpan<byte> associatedData);

    private delegate void Open(ReadOnlySpan<byte> nonce, ReadOnlySpan<byte> ciphertext, ReadOnlySpan<byte> tag, Span<byte> plaintext, ReadOnlySpan<byte> associatedData);

    private enum Tamper
    {
        Tag,
        Ciphertext,
        AssociatedData,
    }

    /// <summary>
    /// One AEAD cipher over fixed material, with the sizes its key, nonce
    /// and tag must not have.
    /// </summary>
    private sealed class AeadCipher : IDisposable
    {
        private readonly IDisposable _instance;
        private readonly Seal _seal;
        private readonly Open _open;
        private readonly byte[] _nonce;
        private readonly byte[] _associatedData;
        private readonly byte[] _plaintext;
        private readonly int _tagBytes;

        public AeadCipher(IDisposable instance, Seal seal, Open open, byte[] nonce, byte[] associatedData, byte[] plaintext, int tagBytes)
        {
            _instance = instance;
            _seal = seal;
            _open = open;
            _nonce = nonce;
            _associatedData = associatedData;
            _plaintext = plaintext;
            _tagBytes = tagBytes;
        }

        public string KnownAnswer()
        {
            byte[] ciphertext = new byte[_plaintext.Length];
            byte[] tag = new byte[_tagBytes];
            _seal(_nonce, _plaintext, ciphertext, tag, _associatedData);
            byte[] decrypted = new byte[_plaintext.Length];
            _open(_nonce, ciphertext, tag, decrypted, _associatedData);
            return Hex(ciphertext) + " " + Hex(tag) + " " + decrypted.AsSpan().SequenceEqual(_plaintext);
        }

        /// <summary>
        /// Flips one bit of one authenticated input and reports how the
        /// decryption fails and whether it left the plaintext buffer cleared.
        /// </summary>
        public string Tampered(Tamper what)
        {
            byte[] ciphertext = new byte[_plaintext.Length];
            byte[] tag = new byte[_tagBytes];
            _seal(_nonce, _plaintext, ciphertext, tag, _associatedData);
            byte[] associatedData = (byte[])_associatedData.Clone();
            switch (what)
            {
                case Tamper.Tag:
                    tag[^1] ^= 0x01;
                    break;
                case Tamper.Ciphertext:
                    ciphertext[0] ^= 0x01;
                    break;
                case Tamper.AssociatedData:
                    associatedData[0] ^= 0x01;
                    break;
            }
            byte[] decrypted = new byte[_plaintext.Length];
            Array.Fill(decrypted, (byte)0xff);
            string failure = Failure(() => _open(_nonce, ciphertext, tag, decrypted, associatedData));
            return failure + " " + Array.TrueForAll(decrypted, b => b == 0);
        }

        /// <summary>
        /// A nonce or tag one byte outside the cipher's range, and an output
        /// buffer shorter than the input, each on an otherwise valid call.
        /// </summary>
        public string InvalidLengths(int shortestNonce, int longestNonce, int shortestTag, int longestTag)
        {
            byte[] ciphertext = new byte[_plaintext.Length];
            byte[] tag = new byte[_tagBytes];
            return "nonce-short=" + Failure(() => _seal(new byte[shortestNonce - 1], _plaintext, ciphertext, tag, _associatedData))
                + " nonce-long=" + Failure(() => _seal(new byte[longestNonce + 1], _plaintext, ciphertext, tag, _associatedData))
                + " tag-short=" + Failure(() => _seal(_nonce, _plaintext, ciphertext, new byte[shortestTag - 1], _associatedData))
                + " tag-long=" + Failure(() => _seal(_nonce, _plaintext, ciphertext, new byte[longestTag + 1], _associatedData))
                + " output-short=" + Failure(() => _seal(_nonce, _plaintext, new byte[_plaintext.Length - 1], tag, _associatedData));
        }

        public void Dispose() => _instance.Dispose();
    }

    private static byte[] Sequence(int first, int count)
    {
        byte[] bytes = new byte[count];
        for (int i = 0; i < count; i++) bytes[i] = (byte)(first + i);
        return bytes;
    }

    /// <summary>AES-128-GCM over arbitrary fixed material with associated data.</summary>
    private static AeadCipher AesGcmCipher()
    {
        var gcm = new AesGcm(Sequence(0x10, 16), 16);
        return new AeadCipher(gcm, gcm.Encrypt, gcm.Decrypt, Sequence(0x20, 12), Encoding.ASCII.GetBytes("wisp"), Encoding.ASCII.GetBytes("wisp acceptance plaintext"), 16);
    }

    /// <summary>RFC 3610 packet vector #1: a 13-byte nonce, 8 bytes of associated data, 23 of plaintext, an 8-byte tag.</summary>
    private static AeadCipher AesCcmCipher()
    {
        var ccm = new AesCcm(Sequence(0xc0, 16));
        return new AeadCipher(ccm, ccm.Encrypt, ccm.Decrypt, Convert.FromHexString("00000003020100a0a1a2a3a4a5"), Sequence(0x00, 8), Sequence(0x08, 23), 8);
    }

    /// <summary>RFC 8439 section 2.8.2.</summary>
    private static AeadCipher ChaCha20Poly1305Cipher()
    {
        var chacha = new ChaCha20Poly1305(Sequence(0x80, 32));
        byte[] plaintext = Encoding.ASCII.GetBytes("Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.");
        return new AeadCipher(chacha, chacha.Encrypt, chacha.Decrypt, Convert.FromHexString("070000004041424344454647"), Convert.FromHexString("50515253c0c1c2c3c4c5c6c7"), plaintext, 16);
    }

    private static string Aead(string algorithm, string probe)
    {
        switch (algorithm)
        {
            case "aes-gcm":
                using (AeadCipher gcm = AesGcmCipher())
                    return probe == "invalid-lengths"
                        ? "key=" + Failure(() => new AesGcm(new byte[15], 16)) + " " + gcm.InvalidLengths(12, 12, 12, 16)
                        : Probe(gcm, probe);
            case "aes-ccm":
                using (AeadCipher ccm = AesCcmCipher())
                    return probe == "invalid-lengths"
                        ? "key=" + Failure(() => new AesCcm(new byte[15])) + " " + ccm.InvalidLengths(7, 13, 4, 16) + " tag-odd=" + Failure(() => new AesCcm(Sequence(0xc0, 16)).Encrypt(new byte[13], new byte[4], new byte[4], new byte[5]))
                        : Probe(ccm, probe);
            case "chacha20-poly1305":
                using (AeadCipher chacha = ChaCha20Poly1305Cipher())
                    return probe == "invalid-lengths"
                        ? "key=" + Failure(() => new ChaCha20Poly1305(new byte[31])) + " " + chacha.InvalidLengths(12, 12, 16, 16)
                        : Probe(chacha, probe);
            default:
                throw new ArgumentException("unknown AEAD " + algorithm);
        }
    }

    private static string Probe(AeadCipher cipher, string probe)
    {
        switch (probe)
        {
            case "known-answer":
                return cipher.KnownAnswer();
            case "tampered-tag":
                return cipher.Tampered(Tamper.Tag);
            case "tampered-ciphertext":
                return cipher.Tampered(Tamper.Ciphertext);
            case "tampered-associated-data":
                return cipher.Tampered(Tamper.AssociatedData);
            default:
                throw new ArgumentException("unknown AEAD probe " + probe);
        }
    }

    /// <summary>
    /// PKCS#7 padding that does not decode, on a ciphertext whose penultimate
    /// block was altered (which flips the final padding byte deterministically),
    /// and a ciphertext that is not whole blocks; the untouched ciphertext
    /// still decrypts.
    /// </summary>
    private static string AesCbcInvalidPadding()
    {
        using Aes aes = Aes.Create();
        aes.Key = Sequence(0x30, 32);
        byte[] iv = Sequence(0x50, 16);
        byte[] plaintext = Encoding.ASCII.GetBytes("wisp acceptance plaintext");
        byte[] encrypted = aes.EncryptCbc(plaintext, iv);
        byte[] tampered = (byte[])encrypted.Clone();
        tampered[15] ^= 0x80;
        byte[] partial = encrypted.AsSpan(0, encrypted.Length - 1).ToArray();
        return "tampered=" + Failure(() => aes.DecryptCbc(tampered, iv))
            + " partial-block=" + Failure(() => aes.DecryptCbc(partial, iv))
            + " intact=" + aes.DecryptCbc(encrypted, iv).AsSpan().SequenceEqual(plaintext);
    }

    private static string AesCbcInvalidLengths()
    {
        using Aes aes = Aes.Create();
        return "key=" + Failure(() => aes.Key = new byte[15])
            + " key-size=" + Failure(() => aes.KeySize = 100)
            + " iv=" + Failure(() => aes.IV = new byte[15])
            + " cbc-iv=" + Failure(() => aes.EncryptCbc(new byte[16], new byte[15]));
    }

    /// <summary>
    /// OAEP rejects an altered ciphertext and one of the wrong length, and
    /// refuses a plaintext the modulus cannot hold.
    /// </summary>
    private static string RsaTamperedCiphertext()
    {
        using RSA rsa = RSA.Create();
        rsa.ImportFromPem(CryptoMaterial.LeafKey);
        byte[] wrapped = rsa.Encrypt(Encoding.ASCII.GetBytes("a secret for the host"), RSAEncryptionPadding.OaepSHA256);
        byte[] tampered = (byte[])wrapped.Clone();
        tampered[10] ^= 0x01;
        byte[] truncated = wrapped.AsSpan(0, wrapped.Length - 1).ToArray();
        return "tampered=" + Failure(() => rsa.Decrypt(tampered, RSAEncryptionPadding.OaepSHA256))
            + " truncated=" + Failure(() => rsa.Decrypt(truncated, RSAEncryptionPadding.OaepSHA256))
            + " too-large=" + Failure(() => rsa.Encrypt(new byte[300], RSAEncryptionPadding.OaepSHA256));
    }

    /// <summary>A signature of the wrong length or with an altered byte verifies false, never throws.</summary>
    private static string EcdsaMalformedSignatures()
    {
        using ECDsa key = ECDsa.Create();
        key.ImportFromPem(CryptoMaterial.EcKey);
        byte[] message = Encoding.ASCII.GetBytes("wisp");
        byte[] signature = key.SignData(message, HashAlgorithmName.SHA256);
        byte[] altered = (byte[])signature.Clone();
        altered[3] ^= 0x01;
        return "empty=" + key.VerifyData(message, Array.Empty<byte>(), HashAlgorithmName.SHA256)
            + " truncated=" + key.VerifyData(message, signature.AsSpan(0, signature.Length - 1).ToArray(), HashAlgorithmName.SHA256)
            + " altered=" + key.VerifyData(message, altered, HashAlgorithmName.SHA256)
            + " intact=" + key.VerifyData(message, signature, HashAlgorithmName.SHA256);
    }
}
