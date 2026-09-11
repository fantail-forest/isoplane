#if WISP_TRUST_PROBE
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Security.Cryptography.X509Certificates;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>
/// Lists the machine root store while the application is constructed, which
/// pre-initialisation does, so the template carries the store's cache of
/// the bundle served at preparation. Answers <c>/at-boot</c> with that
/// listing and <c>/roots</c> with what the store lists now, each as
/// <c>roots=N thumbprint,thumbprint,…</c> sorted.
/// </summary>
internal sealed class TrustProbe : IWispHandler
{
    private readonly string _atBoot = Roots();

    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        string text = request.Path switch
        {
            "/at-boot" => _atBoot,
            "/roots" => Roots(),
            _ => throw new ArgumentException("Unknown trust probe path.", nameof(request)),
        };
        return Task.FromResult(Response.Text(text));
    }

    private static string Roots()
    {
        using var store = new X509Store(StoreName.Root, StoreLocation.LocalMachine);
        store.Open(OpenFlags.ReadOnly);
        var thumbprints = new List<string>();
        foreach (X509Certificate2 certificate in store.Certificates) thumbprints.Add(certificate.Thumbprint);
        thumbprints.Sort(StringComparer.Ordinal);
        return "roots=" + thumbprints.Count.ToString(CultureInfo.InvariantCulture) + " " + string.Join(",", thumbprints);
    }
}
#endif
