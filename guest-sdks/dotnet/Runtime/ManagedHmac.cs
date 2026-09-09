using System;
using Org.BouncyCastle.Crypto.Digests;
using Org.BouncyCastle.Crypto.Macs;
using Org.BouncyCastle.Crypto.Parameters;

namespace Wisp.Runtime;

/// <summary>Managed SHA-256 HMAC state for the adapted runtime's hash provider.</summary>
public sealed class ManagedHmac
{
    private HMac? state;

    /// <summary>Initializes SHA-256 HMAC with a copy of the supplied key.</summary>
    public ManagedHmac(ReadOnlySpan<byte> key)
    {
        state = new HMac(new Sha256Digest());
        state.Init(new KeyParameter(key));
    }

    private HMac State => state ?? throw new ObjectDisposedException(nameof(ManagedHmac));

    /// <summary>Returns the digest size in bytes; throws after disposal.</summary>
    public int HashSizeInBytes => State.GetMacSize();

    /// <summary>Appends bytes to the current digest; throws after disposal.</summary>
    public void AppendHashData(ReadOnlySpan<byte> source) => State.BlockUpdate(source);

    /// <summary>Writes the digest and resets keyed state for another message.</summary>
    /// <exception cref="ObjectDisposedException">The provider was disposed.</exception>
    public int FinalizeHashAndReset(Span<byte> destination) => State.DoFinal(destination);

    /// <summary>Discards the current message and retains its key; throws after disposal.</summary>
    public void Reset() => State.Reset();

    /// <summary>Releases managed state when requested by the runtime's disposal boundary.</summary>
    public void Dispose(bool disposing)
    {
        if (disposing) state = null;
    }
}
