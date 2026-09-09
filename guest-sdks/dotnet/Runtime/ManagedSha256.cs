using System;
using Org.BouncyCastle.Crypto.Digests;

namespace Wisp.Runtime;

/// <summary>Managed SHA-256 state for the adapted runtime's hash provider.</summary>
public sealed class ManagedSha256
{
    private Sha256Digest? state = new Sha256Digest();

    private Sha256Digest State => state ?? throw new ObjectDisposedException(nameof(ManagedSha256));

    /// <summary>Returns the digest size in bytes; throws after disposal.</summary>
    public int HashSizeInBytes => State.GetDigestSize();

    /// <summary>Appends bytes to the current digest; throws after disposal.</summary>
    public void AppendHashData(ReadOnlySpan<byte> source) => State.BlockUpdate(source);

    /// <summary>Writes the digest and resets state for another message.</summary>
    /// <exception cref="ObjectDisposedException">The provider was disposed.</exception>
    public int FinalizeHashAndReset(Span<byte> destination) => State.DoFinal(destination);

    /// <summary>Discards the current message; throws after disposal.</summary>
    public void Reset() => State.Reset();

    /// <summary>Releases managed state when requested by the runtime's disposal boundary.</summary>
    public void Dispose(bool disposing)
    {
        if (disposing) state = null;
    }
}
