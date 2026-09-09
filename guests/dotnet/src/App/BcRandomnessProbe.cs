#if WISP_BC_RANDOMNESS_PROBE
using System;
using System.Threading.Tasks;
using Org.BouncyCastle.Crypto.Digests;
using Org.BouncyCastle.Crypto.Prng;
using Org.BouncyCastle.Security;
using Wisp.Durable;

namespace Wisp.App;

internal sealed class BcRandomnessProbe : IWispHandler
{
    private static readonly SecureRandom Retained = new SecureRandom();

    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        byte[] bytes = new byte[32];
        switch (request.Path)
        {
            case "/warm":
                Retained.NextBytes(bytes);
                Org.BouncyCastle.Math.BigInteger.Arbitrary(256);
                return Task.FromResult(Response.Text("warm"));
            case "/default":
                Retained.NextBytes(bytes);
                break;
            case "/arbitrary":
                bytes = Org.BouncyCastle.Math.BigInteger.Arbitrary(256).ToByteArrayUnsigned();
                break;
            case "/explicit":
                var generator = new DigestRandomGenerator(new Sha256Digest());
                generator.AddSeedMaterial(new byte[] { 1, 2, 3 });
                new SecureRandom(generator).NextBytes(bytes);
                break;
            default:
                throw new ArgumentException("Unknown BC RNG probe path.", nameof(request));
        }
        return Task.FromResult(Response.Text(Convert.ToHexString(bytes)));
    }
}
#endif
