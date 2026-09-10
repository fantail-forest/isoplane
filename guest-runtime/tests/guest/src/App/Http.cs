using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Outbound HTTP through the runtime's <c>wasi:http</c> handler: a request
/// parks on the host's future-incoming-response, bodies stream both ways
/// through <c>wasi:io/streams</c>, TLS terminates at the host, and every
/// failure the host reports reaches the ordinary <see cref="HttpRequestException"/>.
/// </summary>
internal sealed class Http
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "get":
                return await GetAsync("http", path[1], path[2], "/hello").ConfigureAwait(false);
            case "https":
                return await GetAsync("https", path[1], path[2], "/hello").ConfigureAwait(false);
            case "redirect":
                return await GetAsync("http", path[1], path[2], "/redirect").ConfigureAwait(false);
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

    private static async Task<string> GetAsync(string scheme, string host, string port, string route)
    {
        using var client = new HttpClient();
        try
        {
            using var response = await client.GetAsync(Url(scheme, host, port, route)).ConfigureAwait(false);
            string body = await response.Content.ReadAsStringAsync().ConfigureAwait(false);
            string location = response.Headers.Location is null ? "" : " location=" + response.Headers.Location;
            return Describe(response) + location + " body=" + body;
        }
        catch (HttpRequestException error)
        {
            return "HttpRequestException: " + error.Message;
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
}
