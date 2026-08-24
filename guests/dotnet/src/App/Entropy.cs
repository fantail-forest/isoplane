#if WISP_ENTROPY_PROBE
using System;
using System.Collections.Generic;
using System.Security.Cryptography;
using System.Text;

namespace Wisp.App;

/// <summary>
/// The §4.4 clone-uniqueness probe. Build with
/// <c>-p:WispEntropyProbe=true</c>, then run
/// <c>check.sh --entropy-clone</c>.
///
/// Host entropy is drawn per call, but template memory can retain returned
/// values or a guest PRNG's state. This probe compares restored instances to
/// observe which values repeat. The risk depends on where
/// the STATE lives. .NET's `Random.Shared` draws a seed once and then advances
/// a Xoshiro256** state on the GC heap; the object-header hash code does the
/// same. Both live in linear memory, and linear memory is exactly what a
/// template image is. So the question a probe has to answer is not "does this
/// call an import?" but "do two instances restored from ONE post-startup image
/// produce the SAME values?".
///
/// Each line below is a different answer to that, which is why they are
/// reported separately rather than hashed together.
/// </summary>
internal static class EntropyProbe
{
    /// <summary>
    /// What §4.4 explicitly WANTS a pre-initializer to do: "module
    /// initializers, static constructors, DI-container construction, config
    /// parsing". All of those hash strings, which is why §4.4 escrows a
    /// per-deploy Marvin seed — "without it, nothing that hashes a string
    /// (every dictionary, every DI container) could pre-run".
    ///
    /// Called from <c>wisp_initialize</c> under the probe only. Its whole
    /// purpose is to move the lazily-seeded RNG state from "drawn after the
    /// template was frozen" to "baked into the template", so
    /// <c>check.sh --entropy-clone</c> can show what that costs.
    /// </summary>
    internal static void WarmLikePreInit()
    {
        // NOTE the first thing this taught us: a small
        // `Dictionary<string, string>` does NOT seed Marvin. .NET uses
        // `NonRandomizedStringEqualityComparer` until a bucket exceeds the
        // collision threshold, so §4.4's "every dictionary … could [not]
        // pre-run without the seed" is too strong — most dictionaries never
        // touch the randomized path at all. Forcing it takes an explicit
        // `GetHashCode`, which is what a real config-parsing or DI cctor does
        // via `HashCode.Combine`, record equality, or a HashSet<string>.
        var config = new Dictionary<string, string>
        {
            ["region"] = "nz-akl",
            ["tier"] = "standard",
        };
        GC.KeepAlive(config["region"]);

        // Seeds the Marvin string-hash state (§4.4's escrowed seed).
        GC.KeepAlive("wisp-config".GetHashCode());

        // Seeds the object-header hash path: ThreadSafeRandom -> Xoshiro.
        GC.KeepAlive(new object().GetHashCode());

        // Seeds Random.Shared's Xoshiro256** state.
        GC.KeepAlive(Random.Shared.Next());
    }

    internal static string Sample()
    {
        var sb = new StringBuilder();

        // Per-field try/catch, because "which of these five is unsupported"
        // is itself a §12 measurement and one throw must not hide the rest.
        Field(sb, "rng", static () =>
        {
            // §2.5.5 says RandomNumberGenerator routes to a host import.
            Span<byte> b = stackalloc byte[8];
            RandomNumberGenerator.Fill(b);
            return Convert.ToHexString(b);
        });

        // v4 GUID — drawn per call, per §4.4's "no shared entropy streams".
        Field(sb, "guid", static () => Guid.NewGuid().ToString("N"));

        // Seeded ONCE, then advanced from heap state. This is the field that
        // detects a shared stream across clones of one template.
        Field(sb, "shared", static () => Random.Shared.Next().ToString());

        // The Marvin string-hash seed §4.4 escrows, observed indirectly.
        Field(sb, "strhash", static () => "wisp".GetHashCode().ToString());

        // Object-header hash code: assigned lazily from `ThreadSafeRandom`,
        // which is the path §4.1 of the findings traced to `_Exit`.
        Field(sb, "objhash", static () => new object().GetHashCode().ToString());

        return sb.ToString();
    }

    private static void Field(StringBuilder sb, string name, Func<string> f)
    {
        if (sb.Length > 0) sb.Append(' ');
        sb.Append(name).Append('=');
        try
        {
            sb.Append(f());
        }
        catch (Exception e)
        {
            // §12.2/§12.3: an edge-profile gap shows up here as a type name.
            sb.Append("THREW:").Append(e.GetType().Name);
        }
    }
}
#endif
