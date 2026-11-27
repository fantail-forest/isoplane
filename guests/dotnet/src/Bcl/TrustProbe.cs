using System;
using System.Collections.Generic;
using System.Globalization;
using System.Security.Cryptography.X509Certificates;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

internal static class TrustProbe
{
    private static string _atBoot = "";

    /// <summary>
    /// Lists the machine root store during initialization so the template carries the store's
    /// cache of the bundle served at preparation.
    /// </summary>
    public static Task InitializeAsync()
    {
        _atBoot = Roots();
        return Task.CompletedTask;
    }

    public static Task<Response> HandleAsync(string path)
    {
        string text = path switch
        {
            "/at-boot" => _atBoot,
            "/roots" => Roots(),
            _ => throw new ArgumentException("Unknown trust probe path.", nameof(path)),
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
