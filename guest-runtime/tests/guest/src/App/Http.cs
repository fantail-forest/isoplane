using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Outbound HTTP through the runtime's own <see cref="SocketsHttpHandler"/>
/// over the delegated sockets: connections and reads park on socket
/// readiness, bodies stream both ways in guest memory, TLS is the guest's
/// <c>SslStream</c> with a root the application supplies, and every failure
/// reaches the ordinary <see cref="HttpRequestException"/>.
/// </summary>
internal sealed class Http
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "get":
                return await GetAsync(new HttpClient(), "http", path[1], path[2], "/hello").ConfigureAwait(false);
            case "https":
                // Only the application's root: the guest has no system store.
                return await GetAsync(new HttpClient(RootTrustHandler()), "https", path[1], path[2], "/hello").ConfigureAwait(false);
            case "redirect":
                return await GetAsync(new HttpClient(new HttpClientHandler { AllowAutoRedirect = false }), "http", path[1], path[2], "/redirect").ConfigureAwait(false);
            case "follow":
                return await GetAsync(new HttpClient(), "http", path[1], path[2], "/relative").ConfigureAwait(false);
            case "hops":
                return await GetAsync(new HttpClient(new HttpClientHandler { MaxAutomaticRedirections = 3 }), "http", path[1], path[2], "/hops/" + path[3]).ConfigureAwait(false);
            case "replay":
            {
                // A 307 replays the method and the body.
                byte[] payload = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                using var client = new HttpClient();
                using var content = new ByteArrayContent(payload);
                content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
                using var response = await client.PostAsync(Url("http", path[1], path[2], "/replay"), content).ConfigureAwait(false);
                byte[] echoed = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                return Describe(response) + " replayed=" + (echoed.AsSpan().SequenceEqual(payload) ? "same" : "different");
            }
            case "gzip":
            case "deflate":
            {
                using var client = new HttpClient(new HttpClientHandler { AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate });
                using var response = await client.GetAsync(Url("http", path[1], path[2], "/" + path[0] + "/" + path[3])).ConfigureAwait(false);
                byte[] body = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                byte[] expected = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                return Describe(response) + " content-encoding=" + string.Join(",", response.Content.Headers.ContentEncoding)
                    + " body=" + (body.AsSpan().SequenceEqual(expected) ? "decoded" : "raw " + body.Length.ToString(CultureInfo.InvariantCulture));
            }
            case "accept-encoding":
            {
                using var client = new HttpClient(new HttpClientHandler { AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate });
                return await client.GetStringAsync(Url("http", path[1], path[2], "/accept-encoding")).ConfigureAwait(false);
            }
            case "reuse":
            {
                // Sequential requests on one client share one pooled connection.
                using var client = new HttpClient();
                string joined = "";
                for (int i = 0; i < 3; i++)
                {
                    joined += (i == 0 ? "" : "|") + await client.GetStringAsync(Url("http", path[1], path[2], "/hello")).ConfigureAwait(false);
                }
                return joined;
            }
            case "stale":
            {
                // The peer closes the idle connection after answering; the
                // pool must notice and open another rather than fail.
                using var client = new HttpClient();
                string first = await client.GetStringAsync(Url("http", path[1], path[2], "/stale")).ConfigureAwait(false);
                string second = await client.GetStringAsync(Url("http", path[1], path[2], "/stale")).ConfigureAwait(false);
                return first + "|" + second;
            }
            case "headers":
            {
                using var client = new HttpClient();
                using var request = new HttpRequestMessage(HttpMethod.Get, Url("http", path[1], path[2], "/headers"));
                request.Headers.Add("X-Wisp", "ping");
                using var response = await client.SendAsync(request).ConfigureAwait(false);
                string body = await response.Content.ReadAsStringAsync().ConfigureAwait(false);
                return Describe(response) + " body=" + body + " x-answer=" + string.Join(",", response.Headers.GetValues("X-Answer"));
            }
            case "stream":
            {
                using var client = new HttpClient();
                using var response = await client.GetAsync(Url("http", path[1], path[2], "/stream/" + path[3]), HttpCompletionOption.ResponseHeadersRead).ConfigureAwait(false);
                using var stream = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
                using var digest = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
                byte[] buffer = new byte[8192];
                long length = 0;
                int reads = 0;
                while (true)
                {
                    int count = await stream.ReadAsync(buffer).ConfigureAwait(false);
                    if (count == 0) break;
                    digest.AppendData(buffer, 0, count);
                    length += count;
                    reads++;
                }
                return Describe(response) + " length=" + length.ToString(CultureInfo.InvariantCulture)
                    + " reads=" + reads.ToString(CultureInfo.InvariantCulture)
                    + " sha256=" + Convert.ToHexString(digest.GetHashAndReset());
            }
            case "post":
            {
                byte[] payload = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                using var client = new HttpClient();
                using var content = new StreamContent(new MemoryStream(payload));
                content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
                using var response = await client.PostAsync(Url("http", path[1], path[2], "/echo"), content).ConfigureAwait(false);
                byte[] echoed = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                return Describe(response) + " sent=" + Convert.ToHexString(SHA256.HashData(payload))
                    + " echoed=" + Convert.ToHexString(SHA256.HashData(echoed));
            }
            case "two":
            {
                using var client = new HttpClient();
                Task<string> hello = client.GetStringAsync(Url("http", path[1], path[2], "/hello"));
                Task<string> world = client.GetStringAsync(Url("http", path[1], path[2], "/world"));
                await Task.WhenAll(hello, world).ConfigureAwait(false);
                return hello.Result + "+" + world.Result;
            }
            case "timeout":
            {
                using var client = new HttpClient { Timeout = TimeSpan.FromMilliseconds(int.Parse(path[3], CultureInfo.InvariantCulture)) };
                try
                {
                    using var response = await client.GetAsync(Url("http", path[1], path[2], "/stall")).ConfigureAwait(false);
                    return "unexpected " + Describe(response);
                }
                catch (TaskCanceledException)
                {
                    return "TaskCanceledException";
                }
            }
            case "cancel":
            {
                using var client = new HttpClient();
                using var cancellation = new CancellationTokenSource();
                cancellation.CancelAfter(int.Parse(path[3], CultureInfo.InvariantCulture));
                try
                {
                    using var response = await client.GetAsync(Url("http", path[1], path[2], "/stall"), cancellation.Token).ConfigureAwait(false);
                    return "unexpected " + Describe(response);
                }
                catch (OperationCanceledException)
                {
                    return "OperationCanceledException";
                }
            }
            default:
                throw new ArgumentException("unknown http behaviour " + path[0]);
        }
    }

    /// <summary>A handler trusting only the material's root, as an application with a private CA configures it.</summary>
    private static SocketsHttpHandler RootTrustHandler()
    {
        var policy = new X509ChainPolicy { TrustMode = X509ChainTrustMode.CustomRootTrust, RevocationMode = X509RevocationMode.NoCheck };
        policy.CustomTrustStore.Add(X509CertificateLoader.LoadCertificate(PemBytes(CryptoMaterial.RootCertificate)));
        var handler = new SocketsHttpHandler();
        handler.SslOptions.CertificateChainPolicy = policy;
        return handler;
    }

    private static async Task<string> GetAsync(HttpClient client, string scheme, string host, string port, string route)
    {
        using (client)
        {
            try
            {
                using var response = await client.GetAsync(Url(scheme, host, port, route)).ConfigureAwait(false);
                string body = await response.Content.ReadAsStringAsync().ConfigureAwait(false);
                string location = response.Headers.Location is null ? "" : " location=" + response.Headers.Location;
                return Describe(response) + location + " body=" + body;
            }
            catch (HttpRequestException error)
            {
                return "HttpRequestException: " + error.Message + (error.InnerException is null ? "" : " <- " + error.InnerException.GetType().Name);
            }
        }
    }

    private static string Url(string scheme, string host, string port, string route)
        => scheme + "://" + host + ":" + port + route;

    private static string Describe(HttpResponseMessage response)
    {
        string contentType = response.Content.Headers.ContentType is null ? "" : response.Content.Headers.ContentType.MediaType ?? "";
        return "status=" + ((int)response.StatusCode).ToString(CultureInfo.InvariantCulture) + " content-type=" + contentType;
    }

    private static byte[] Pattern(int length)
    {
        byte[] bytes = new byte[length];
        for (int i = 0; i < bytes.Length; i++) bytes[i] = (byte)(i % 251);
        return bytes;
    }

    private static byte[] PemBytes(string pem)
    {
        string body = pem.Replace("-----BEGIN CERTIFICATE-----", "").Replace("-----END CERTIFICATE-----", "").Replace("\r", "").Replace("\n", "");
        return Convert.FromBase64String(body);
    }
}
