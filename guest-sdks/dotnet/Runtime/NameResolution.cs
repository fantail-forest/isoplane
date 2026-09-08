using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;
using Wisp.Abi;

namespace Wisp.Runtime;

/// <summary>The SDK entrypoint used by the adapted NativeAOT DNS implementation.</summary>
public static class NameResolution
{
    /// <summary>Resolves addresses through host-owned DNS, releasing the lookup on cancellation.</summary>
    public static Task<IPAddress[]> GetHostAddressesAsync(string name, AddressFamily family, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(name);
        if (cancellationToken.IsCancellationRequested)
            return Task.FromCanceled<IPAddress[]>(cancellationToken);
        if (IPAddress.TryParse(name, out IPAddress? address))
        {
            if (address.Equals(IPAddress.Any) || address.Equals(IPAddress.IPv6Any))
                throw new ArgumentException("An unspecified address cannot be resolved.", nameof(name));
            return Task.FromResult(family == AddressFamily.Unspecified || address.AddressFamily == family
                ? new[] { address } : Array.Empty<IPAddress>());
        }
        return ResolveAsync(name, family, cancellationToken);
    }

    private static async Task<IPAddress[]> ResolveAsync(string name, AddressFamily family, CancellationToken cancellationToken)
    {
        var lookup = NativeDns.Start(name);
        try
        {
            await Wait(null!, lookup.Pollable, true, cancellationToken).ConfigureAwait(false);
            var addresses = new List<IPAddress>();
            while (NativeDns.Next(lookup.Stream) is IPAddress address)
            {
                if (family == AddressFamily.Unspecified || family == address.AddressFamily)
                    addresses.Add(address);
            }
            return addresses.ToArray();
        }
        finally { NativeDns.Drop(lookup.Stream); }
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "WispWaitPollable")]
    private static extern Task Wait(Thread thread, int handle, bool ownsPollable, CancellationToken cancellationToken);
}
