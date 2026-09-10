using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Net.Security;
using System.Net.Sockets;
using System.Security.Authentication;
using System.Security.Cryptography.X509Certificates;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// TLS over the delegated sockets with the guest's own OpenSSL: an
/// <see cref="SslStream"/> handshake parks on socket readiness like any
/// other read, bytes flow through it both ways, the server certificate is
/// validated against a root the guest trusts, and an untrusted one fails
/// the ordinary way.
/// </summary>
internal sealed class Tls
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "handshake":
            {
                // The peer's expected name is the leaf's unless the route names one.
                string host = path.Length > 3 ? path[3] : LeafHost;
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                try
                {
                    using var tls = await AuthenticateAsync(client, Trust.Root, host).ConfigureAwait(false);
                    return Describe(tls);
                }
                catch (AuthenticationException error)
                {
                    return "AuthenticationException: " + error.Message;
                }
                catch (IOException error)
                {
                    return "IOException: " + error.Message;
                }
            }
            case "echo":
            {
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var tls = await AuthenticateAsync(client, Trust.Root, LeafHost).ConfigureAwait(false);
                return await RoundTripAsync(tls, Convert.FromHexString(path[3])).ConfigureAwait(false);
            }
            case "two":
            {
                using var first = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var second = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var a = await AuthenticateAsync(first, Trust.Root, LeafHost).ConfigureAwait(false);
                using var b = await AuthenticateAsync(second, Trust.Root, LeafHost).ConfigureAwait(false);
                Task<string> one = RoundTripAsync(a, Convert.FromHexString(path[3]));
                Task<string> other = RoundTripAsync(b, Convert.FromHexString(path[4]));
                await Task.WhenAll(one, other).ConfigureAwait(false);
                return one.Result + "+" + other.Result;
            }
            case "untrusted":
            {
                // Whatever the guest's certificate store holds: nothing.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                try
                {
                    using var tls = await AuthenticateAsync(client, Trust.System, LeafHost).ConfigureAwait(false);
                    return "unexpected " + Describe(tls);
                }
                catch (AuthenticationException error)
                {
                    return "AuthenticationException: " + error.Message;
                }
            }
            case "callback":
            {
                // The application decides: the callback sees the policy errors
                // the chain produced and accepts the peer anyway.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                SslPolicyErrors seen = SslPolicyErrors.None;
                using var tls = new SslStream(new NetworkStream(client, ownsSocket: false), leaveInnerStreamOpen: false);
                await tls.AuthenticateAsClientAsync(new SslClientAuthenticationOptions
                {
                    TargetHost = LeafHost,
                    RemoteCertificateValidationCallback = (_, _, _, errors) => { seen = errors; return true; },
                }).ConfigureAwait(false);
                return "accepted " + seen + " " + Describe(tls);
            }
            case "eof":
            {
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var tls = await AuthenticateAsync(client, Trust.Root, LeafHost).ConfigureAwait(false);
                return await ReadOutcomeAsync(tls, CancellationToken.None).ConfigureAwait(false);
            }
            case "client":
            {
                // The peer requires a certificate; the guest presents the
                // material's client certificate with its private key.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                var options = RootTrustOptions(LeafHost);
                options.ClientCertificates = new X509Certificate2Collection
                {
                    X509Certificate2.CreateFromPem(CryptoMaterial.ClientCertificate, CryptoMaterial.ClientKey),
                };
                using var tls = new SslStream(new NetworkStream(client, ownsSocket: false), leaveInnerStreamOpen: false);
                await tls.AuthenticateAsClientAsync(options).ConfigureAwait(false);
                string echoed = await RoundTripAsync(tls, Convert.FromHexString(path[3])).ConfigureAwait(false);
                return "mutual=" + tls.IsMutuallyAuthenticated + " " + echoed;
            }
            case "client-none":
            {
                // The peer requires a certificate the guest does not offer;
                // TLS 1.3 delivers the refusal after the client's handshake.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                SslStream? tls = null;
                try
                {
                    tls = await AuthenticateAsync(client, Trust.Root, LeafHost).ConfigureAwait(false);
                }
                catch (Exception error) when (error is AuthenticationException or IOException)
                {
                    return "handshake: " + error.GetType().Name;
                }
                using (tls)
                {
                    try
                    {
                        await tls.WriteAsync(new byte[] { 1 }).ConfigureAwait(false);
                        int count = await tls.ReadAsync(new byte[4]).ConfigureAwait(false);
                        return "read: " + count.ToString(CultureInfo.InvariantCulture) + " bytes";
                    }
                    catch (IOException error)
                    {
                        return "read: IOException: " + error.Message;
                    }
                }
            }
            case "alpn":
            {
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                var options = RootTrustOptions(LeafHost);
                options.ApplicationProtocols = new List<SslApplicationProtocol> { new SslApplicationProtocol(path[3]) };
                using var tls = new SslStream(new NetworkStream(client, ownsSocket: false), leaveInnerStreamOpen: false);
                await tls.AuthenticateAsClientAsync(options).ConfigureAwait(false);
                return "alpn=" + tls.NegotiatedApplicationProtocol;
            }
            case "cancel-handshake":
            {
                // The peer never answers; the handshake is cancelled by its
                // token's timer while parked on the socket.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var cancellation = new CancellationTokenSource(int.Parse(path[3], CultureInfo.InvariantCulture));
                using var tls = new SslStream(new NetworkStream(client, ownsSocket: false), leaveInnerStreamOpen: false);
                try
                {
                    await tls.AuthenticateAsClientAsync(RootTrustOptions(LeafHost), cancellation.Token).ConfigureAwait(false);
                    return "unexpected " + Describe(tls);
                }
                catch (OperationCanceledException)
                {
                    return "cancelled";
                }
            }
            case "cancel-read":
            {
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var tls = await AuthenticateAsync(client, Trust.Root, LeafHost).ConfigureAwait(false);
                using var cancellation = new CancellationTokenSource(int.Parse(path[3], CultureInfo.InvariantCulture));
                return await ReadOutcomeAsync(tls, cancellation.Token).ConfigureAwait(false);
            }
            case "shutdown":
            {
                // A clean close: the peer sees close_notify before the socket
                // goes away.
                using var client = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var tls = await AuthenticateAsync(client, Trust.Root, LeafHost).ConfigureAwait(false);
                await tls.ShutdownAsync().ConfigureAwait(false);
                return "shut down";
            }
            default:
                throw new ArgumentException("unknown tls behaviour " + path[0]);
        }
    }

    private const string LeafHost = "leaf.acceptance.test";

    private enum Trust
    {
        /// <summary>Only the material's root, as a custom root trust.</summary>
        Root,
        /// <summary>Whatever the guest's certificate store holds: nothing.</summary>
        System,
    }

    private static async Task<Socket> ConnectAsync(string host, string port)
    {
        var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
        await socket.ConnectAsync(host, int.Parse(port, CultureInfo.InvariantCulture)).ConfigureAwait(false);
        return socket;
    }

    private static SslClientAuthenticationOptions RootTrustOptions(string host)
    {
        // A private root publishes no revocation lists; a policy of its own
        // would otherwise default to an online check.
        var policy = new X509ChainPolicy { TrustMode = X509ChainTrustMode.CustomRootTrust, RevocationMode = X509RevocationMode.NoCheck };
        policy.CustomTrustStore.Add(X509CertificateLoader.LoadCertificate(PemBytes(CryptoMaterial.RootCertificate)));
        return new SslClientAuthenticationOptions { TargetHost = host, CertificateChainPolicy = policy };
    }

    private static async Task<SslStream> AuthenticateAsync(Socket socket, Trust trust, string host)
    {
        var tls = new SslStream(new NetworkStream(socket, ownsSocket: false), leaveInnerStreamOpen: false);
        var options = trust == Trust.Root ? RootTrustOptions(host) : new SslClientAuthenticationOptions { TargetHost = host };
        try
        {
            await tls.AuthenticateAsClientAsync(options).ConfigureAwait(false);
        }
        catch
        {
            tls.Dispose();
            throw;
        }
        return tls;
    }

    private static async Task<string> RoundTripAsync(SslStream tls, byte[] payload)
    {
        await tls.WriteAsync(payload).ConfigureAwait(false);
        await tls.FlushAsync().ConfigureAwait(false);
        byte[] echoed = new byte[payload.Length];
        int received = 0;
        while (received < echoed.Length)
        {
            int count = await tls.ReadAsync(new Memory<byte>(echoed, received, echoed.Length - received)).ConfigureAwait(false);
            if (count == 0) return "eof after " + received.ToString(CultureInfo.InvariantCulture);
            received += count;
        }
        return Convert.ToHexString(echoed);
    }

    /// <summary>One read and what it produced: bytes, EOF, a cancellation or an I/O failure.</summary>
    private static async Task<string> ReadOutcomeAsync(SslStream tls, CancellationToken cancellation)
    {
        try
        {
            int count = await tls.ReadAsync(new byte[64], cancellation).ConfigureAwait(false);
            return count == 0 ? "eof" : "received " + count.ToString(CultureInfo.InvariantCulture);
        }
        catch (OperationCanceledException)
        {
            return "cancelled";
        }
        catch (IOException error)
        {
            return "IOException: " + error.Message;
        }
    }

    private static string Describe(SslStream tls)
    {
        string subject = tls.RemoteCertificate is X509Certificate certificate ? certificate.Subject : "none";
        return "protocol=" + tls.SslProtocol + " authenticated=" + tls.IsAuthenticated + " encrypted=" + tls.IsEncrypted
            + " cipher=" + tls.NegotiatedCipherSuite + " peer=" + subject;
    }

    private static byte[] PemBytes(string pem)
    {
        string body = pem.Replace("-----BEGIN CERTIFICATE-----", "").Replace("-----END CERTIFICATE-----", "").Replace("\r", "").Replace("\n", "");
        return Convert.FromBase64String(body);
    }
}
