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
/// reaches the ordinary <see cref="HttpRequestException"/>. A leading
/// <c>h2</c>, <c>h3</c> or <c>higher</c> segment selects the HTTP version
/// the client asks for and how strictly; every behaviour then runs under it
/// and reports the version the response came on.
/// </summary>
internal sealed class Http
{
    /// <summary>The version a run asks for and its policy; the client's defaults when unset.</summary>
    private readonly record struct Selection(Version? Version, HttpVersionPolicy? Policy)
    {
        public static readonly Selection Default = new(null, null);

        public bool IsExplicit => Version is not null;
    }

    public async Task<string> RunAsync(string[] path)
    {
        Selection selection = path[0] switch
        {
            "h2" => new Selection(HttpVersion.Version20, HttpVersionPolicy.RequestVersionExact),
            "h3" => new Selection(HttpVersion.Version30, HttpVersionPolicy.RequestVersionExact),
            "higher" => new Selection(HttpVersion.Version11, HttpVersionPolicy.RequestVersionOrHigher),
            _ => Selection.Default,
        };
        if (selection.IsExplicit) path = path[1..];
        switch (path[0])
        {
            case "get":
                return await GetAsync(Client(selection), selection, "http", path[1], path[2], "/hello").ConfigureAwait(false);
            case "https":
                // Only the application's root: the guest has no system store.
                return await GetAsync(Client(selection, RootTrustHandler()), selection, "https", path[1], path[2], "/hello").ConfigureAwait(false);
            case "redirect":
                return await GetAsync(Client(selection, new HttpClientHandler { AllowAutoRedirect = false }), selection, "http", path[1], path[2], "/redirect").ConfigureAwait(false);
            case "follow":
                return await GetAsync(Client(selection), selection, "http", path[1], path[2], "/relative").ConfigureAwait(false);
            case "hops":
                return await GetAsync(Client(selection, new HttpClientHandler { MaxAutomaticRedirections = 3 }), selection, "http", path[1], path[2], "/hops/" + path[3]).ConfigureAwait(false);
            case "replay":
            {
                // A 307 replays the method and the body.
                byte[] payload = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                using var client = Client(selection);
                using var content = new ByteArrayContent(payload);
                content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
                using var response = await client.PostAsync(Url("http", path[1], path[2], "/replay"), content).ConfigureAwait(false);
                byte[] echoed = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                return Describe(response, selection) + " replayed=" + (echoed.AsSpan().SequenceEqual(payload) ? "same" : "different");
            }
            case "br":
            case "gzip":
            case "deflate":
            {
                using var client = Client(selection, new HttpClientHandler { AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate | DecompressionMethods.Brotli });
                using var response = await client.GetAsync(Url("http", path[1], path[2], "/" + path[0] + "/" + path[3])).ConfigureAwait(false);
                byte[] body = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                byte[] expected = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                return Describe(response, selection) + " content-encoding=" + string.Join(",", response.Content.Headers.ContentEncoding)
                    + " body=" + (body.AsSpan().SequenceEqual(expected) ? "decoded" : "raw " + body.Length.ToString(CultureInfo.InvariantCulture));
            }
            case "accept-encoding":
            {
                using var client = Client(selection, new HttpClientHandler { AutomaticDecompression = DecompressionMethods.GZip | DecompressionMethods.Deflate | DecompressionMethods.Brotli });
                return await client.GetStringAsync(Url("http", path[1], path[2], "/accept-encoding")).ConfigureAwait(false);
            }
            case "reuse":
            {
                // Sequential requests on one client share one pooled connection.
                using var client = Client(selection);
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
                using var client = Client(selection);
                string first = await client.GetStringAsync(Url("http", path[1], path[2], "/stale")).ConfigureAwait(false);
                string second = await client.GetStringAsync(Url("http", path[1], path[2], "/stale")).ConfigureAwait(false);
                return first + "|" + second;
            }
            case "headers":
            {
                using var client = Client(selection);
                using var request = new HttpRequestMessage(HttpMethod.Get, Url("http", path[1], path[2], "/headers"));
                request.Headers.Add("X-Isoplane", "ping");
                using var response = await client.SendAsync(request).ConfigureAwait(false);
                string body = await response.Content.ReadAsStringAsync().ConfigureAwait(false);
                return Describe(response, selection) + " body=" + body + " x-answer=" + string.Join(",", response.Headers.GetValues("X-Answer"));
            }
            case "stream":
            case "stream-gated":
            {
                bool gated = path[0] == "stream-gated";
                int prefixBytes = gated ? int.Parse(path[4], CultureInfo.InvariantCulture) : 0;
                using var client = Client(selection);
                string route = gated ? "/stream" : "/stream/" + path[3];
                using var response = await client.GetAsync(Url("http", path[1], path[2], route), HttpCompletionOption.ResponseHeadersRead).ConfigureAwait(false);
                using var stream = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
                using var digest = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
                byte[] buffer = new byte[8192];
                long length = 0;
                int reads = 0;
                while (true)
                {
                    int capacity = gated && length < prefixBytes ? (int)Math.Min(buffer.Length, prefixBytes - length) : buffer.Length;
                    int count = await stream.ReadAsync(buffer.AsMemory(0, capacity)).ConfigureAwait(false);
                    if (count == 0) break;
                    digest.AppendData(buffer, 0, count);
                    length += count;
                    reads++;
                    if (gated && length == prefixBytes)
                    {
                        // A separate client permits this checkpoint while HTTP/1's body is still open.
                        using var progress = Client(selection);
                        await progress.GetStringAsync(Url("http", path[1], path[2], "/download-progress/" + Convert.ToHexString(digest.GetCurrentHash()))).ConfigureAwait(false);
                    }
                }
                return Describe(response, selection) + " length=" + length.ToString(CultureInfo.InvariantCulture)
                    + " reads=" + reads.ToString(CultureInfo.InvariantCulture)
                    + " sha256=" + Convert.ToHexString(digest.GetHashAndReset());
            }
            case "post":
            case "post-gated":
            {
                byte[] payload = Pattern(int.Parse(path[3], CultureInfo.InvariantCulture));
                using var client = Client(selection);
                using HttpContent content = path[0] == "post-gated"
                    ? new GatedUploadContent(payload, int.Parse(path[4], CultureInfo.InvariantCulture), selection, Url("http", path[1], path[2], "/upload-progress"))
                    : new StreamContent(new MemoryStream(payload));
                content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
                using var response = await client.PostAsync(Url("http", path[1], path[2], "/echo"), content).ConfigureAwait(false);
                byte[] echoed = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
                return Describe(response, selection) + " sent=" + Convert.ToHexString(SHA256.HashData(payload))
                    + " echoed=" + Convert.ToHexString(SHA256.HashData(echoed));
            }
            case "two":
            {
                using var client = Client(selection);
                Task<string> hello = client.GetStringAsync(Url("http", path[1], path[2], "/hello"));
                Task<string> world = client.GetStringAsync(Url("http", path[1], path[2], "/world"));
                await Task.WhenAll(hello, world).ConfigureAwait(false);
                return hello.Result + "+" + world.Result;
            }
            case "timeout":
            {
                using var client = Client(selection);
                client.Timeout = TimeSpan.FromMilliseconds(int.Parse(path[3], CultureInfo.InvariantCulture));
                try
                {
                    using var response = await client.GetAsync(Url("http", path[1], path[2], "/stall")).ConfigureAwait(false);
                    return "unexpected " + Describe(response, selection);
                }
                catch (TaskCanceledException)
                {
                    return "TaskCanceledException";
                }
            }
            case "cancel":
            {
                using var client = Client(selection);
                using var cancellation = new CancellationTokenSource();
                cancellation.CancelAfter(int.Parse(path[3], CultureInfo.InvariantCulture));
                try
                {
                    using var response = await client.GetAsync(Url("http", path[1], path[2], "/stall"), cancellation.Token).ConfigureAwait(false);
                    return "unexpected " + Describe(response, selection);
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

    private sealed class GatedUploadContent(byte[] payload, int prefixBytes, Selection selection, string checkpoint) : HttpContent
    {
        protected override bool TryComputeLength(out long length)
        {
            length = payload.Length;
            return true;
        }

        protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context)
        {
            await stream.WriteAsync(payload.AsMemory(0, prefixBytes)).ConfigureAwait(false);
            await stream.FlushAsync().ConfigureAwait(false);
            // Even a completely buffered transport cannot obtain the remainder before release.
            using var progress = Client(selection);
            await progress.GetStringAsync(checkpoint).ConfigureAwait(false);
            await stream.WriteAsync(payload.AsMemory(prefixBytes)).ConfigureAwait(false);
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

    /// <summary>A client asking for the selected version, or the defaults (HTTP/1.1 or lower) when none is.</summary>
    private static HttpClient Client(Selection selection, HttpMessageHandler? handler = null)
    {
        var client = handler is null ? new HttpClient() : new HttpClient(handler);
        if (selection.Version is not null) client.DefaultRequestVersion = selection.Version;
        if (selection.Policy is HttpVersionPolicy policy) client.DefaultVersionPolicy = policy;
        return client;
    }

    private static async Task<string> GetAsync(HttpClient client, Selection selection, string scheme, string host, string port, string route)
    {
        using (client)
        {
            try
            {
                using var response = await client.GetAsync(Url(scheme, host, port, route)).ConfigureAwait(false);
                string body = await response.Content.ReadAsStringAsync().ConfigureAwait(false);
                string location = response.Headers.Location is null ? "" : " location=" + response.Headers.Location;
                return Describe(response, selection) + location + " body=" + body;
            }
            catch (HttpRequestException error)
            {
                return "HttpRequestException(" + error.HttpRequestError + "): " + error.Message + (error.InnerException is null ? "" : " <- " + error.InnerException.GetType().Name);
            }
        }
    }

    private static string Url(string scheme, string host, string port, string route)
        => scheme + "://" + host + ":" + port + route;

    /// <summary>The status and media type, and the response's HTTP version when the run selected one.</summary>
    private static string Describe(HttpResponseMessage response, Selection selection)
    {
        string contentType = response.Content.Headers.ContentType is null ? "" : response.Content.Headers.ContentType.MediaType ?? "";
        string version = selection.IsExplicit ? " version=" + response.Version : "";
        return "status=" + ((int)response.StatusCode).ToString(CultureInfo.InvariantCulture) + version + " content-type=" + contentType;
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
