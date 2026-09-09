using System;
using Org.BouncyCastle.Crypto.Digests;
using Org.BouncyCastle.Crypto.Macs;
using Org.BouncyCastle.Crypto.Parameters;

namespace Wisp.Runtime;

/// <summary>SHA-256 for the adapted NativeAOT runtime.</summary>
public static class Sha256
{
    /// <summary>Computes a SHA-256 digest of the supplied bytes.</summary>
    /// <exception cref="ArgumentNullException"><paramref name="source"/> is null.</exception>
    public static byte[] HashData(byte[] source)
    {
        ArgumentNullException.ThrowIfNull(source);
        return HashData(source.AsSpan());
    }

    /// <summary>Computes a SHA-256 digest of the supplied bytes.</summary>
    public static byte[] HashData(ReadOnlySpan<byte> source)
    {
        var digest = new Sha256Digest();
        digest.BlockUpdate(source);
        byte[] result = new byte[digest.GetDigestSize()];
        digest.DoFinal(result, 0);
        return result;
    }
}

/// <summary>HMAC-SHA-256 for the adapted NativeAOT runtime.</summary>
public static class HmacSha256
{
    /// <summary>Computes an HMAC-SHA-256 authentication code using the supplied key.</summary>
    /// <exception cref="ArgumentNullException"><paramref name="key"/> or <paramref name="source"/> is null.</exception>
    public static byte[] HashData(byte[] key, byte[] source)
    {
        ArgumentNullException.ThrowIfNull(key);
        ArgumentNullException.ThrowIfNull(source);
        return HashData(key.AsSpan(), source.AsSpan());
    }

    /// <summary>Computes an HMAC-SHA-256 authentication code using the supplied key.</summary>
    public static byte[] HashData(ReadOnlySpan<byte> key, ReadOnlySpan<byte> source)
    {
        var hmac = new HMac(new Sha256Digest());
        hmac.Init(new KeyParameter(key));
        hmac.BlockUpdate(source);
        byte[] result = new byte[hmac.GetMacSize()];
        hmac.DoFinal(result, 0);
        return result;
    }
}
