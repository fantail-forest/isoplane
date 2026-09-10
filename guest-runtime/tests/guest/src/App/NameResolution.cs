using System;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Name resolution through the source-built asynchronous WASI path: forward
/// lookups await the host's resolver, literals never leave the guest, failures
/// map to the ordinary socket errors, the host name comes from libc, and the
/// lookups that would have to block fail explicitly rather than by trap.
/// </summary>
internal sealed class NameResolution
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "resolve":
                return Join(await Dns.GetHostAddressesAsync(path[1]).ConfigureAwait(false));
            case "resolve4":
                return Join(await Dns.GetHostAddressesAsync(path[1], AddressFamily.InterNetwork).ConfigureAwait(false));
            case "resolve6":
                return Join(await Dns.GetHostAddressesAsync(path[1], AddressFamily.InterNetworkV6).ConfigureAwait(false));
            case "entry":
            {
                IPHostEntry entry = await Dns.GetHostEntryAsync(path[1]).ConfigureAwait(false);
                return entry.HostName + "|" + entry.Aliases.Length.ToString(CultureInfo.InvariantCulture) + "|" + Join(entry.AddressList);
            }
            case "literal":
                return Join(await Dns.GetHostAddressesAsync(path[1]).ConfigureAwait(false));
            case "error":
                return await ExpectSocketError(() => Dns.GetHostAddressesAsync(path[1])).ConfigureAwait(false);
            case "cancel":
            {
                using var cancellation = new CancellationTokenSource();
                cancellation.CancelAfter(int.Parse(path[2], CultureInfo.InvariantCulture));
                try
                {
                    await Dns.GetHostAddressesAsync(path[1], cancellation.Token).ConfigureAwait(false);
                    return "resolved";
                }
                catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
                {
                    return "cancelled";
                }
            }
            case "reverse":
                return await ExpectUnsupported(() => Dns.GetHostEntryAsync(IPAddress.Parse(path[1]))).ConfigureAwait(false);
            case "hostname":
                return Dns.GetHostName();
            case "sync":
                return await ExpectUnsupported(() => Task.FromResult(Dns.GetHostAddresses(path[1]))).ConfigureAwait(false);
            case "legacy":
            {
                // The Begin/End pair over the asynchronous path completes through its callback.
                var completed = new TaskCompletionSource<IPAddress[]>();
                try
                {
                    Dns.BeginGetHostAddresses(path[1], result =>
                    {
                        try { completed.SetResult(Dns.EndGetHostAddresses(result)); }
                        catch (Exception error) { completed.SetException(error); }
                    }, null);
                    return Join(await completed.Task.ConfigureAwait(false));
                }
                catch (PlatformNotSupportedException error)
                {
                    // Where the legacy pair breaks is the diagnosis.
                    return "PlatformNotSupportedException at " + error.StackTrace;
                }
            }
            default:
                throw new ArgumentException("unknown dns case " + path[0]);
        }
    }

    private static string Join(IPAddress[] addresses) =>
        addresses.Length == 0 ? "none" : string.Join(",", addresses.Select(address => address.ToString()));

    private static async Task<string> ExpectSocketError(Func<Task> operation)
    {
        try
        {
            await operation().ConfigureAwait(false);
            return "no error";
        }
        catch (SocketException error)
        {
            return error.SocketErrorCode.ToString();
        }
    }

    private static async Task<string> ExpectUnsupported(Func<Task> operation)
    {
        try
        {
            await operation().ConfigureAwait(false);
            return "supported";
        }
        catch (PlatformNotSupportedException)
        {
            return "PlatformNotSupportedException";
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }
}
