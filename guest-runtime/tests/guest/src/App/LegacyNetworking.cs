using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Mail;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

internal static class LegacyNetworking
{
    public static Task<string> RunAsync(string[] path)
    {
        int port = int.Parse(path[1], CultureInfo.InvariantCulture);
        return path[0].StartsWith("smtp", StringComparison.Ordinal)
            ? SendMailAsync(path[0], port)
            : RequestAsync(path[0], port);
    }

    private static async Task<string> RequestAsync(string mode, int port)
    {
        var origin = new Uri($"http://127.0.0.1:{port}/");
        if (mode == "web-client")
        {
            using var client = new ClosingWebClient();
            return await client.DownloadStringTaskAsync(new Uri(origin, "hello")).ConfigureAwait(false);
        }
        var request = (HttpWebRequest)WebRequest.Create(new Uri(origin, mode == "request-cancel" ? "stall" : "echo"));
        request.Proxy = null;
        request.KeepAlive = false;
        if (mode == "request-cancel")
        {
            Task<WebResponse> pending = request.GetResponseAsync();
            await Dns.GetHostAddressesAsync("request-gate.test").ConfigureAwait(false);
            request.Abort();
            try
            {
                using WebResponse response = await pending.ConfigureAwait(false);
                return "request ignored abort";
            }
            catch (WebException error) when (error.Status == WebExceptionStatus.RequestCanceled)
            {
                return "request cancelled";
            }
        }
        byte[] body = Encoding.UTF8.GetBytes("legacy async body");
        request.Method = "POST";
        request.ContentLength = body.Length;
        await using (Stream outgoing = await request.GetRequestStreamAsync().ConfigureAwait(false))
            await outgoing.WriteAsync(body).ConfigureAwait(false);
        using WebResponse result = await request.GetResponseAsync().ConfigureAwait(false);
        using var reader = new StreamReader(result.GetResponseStream());
        return await reader.ReadToEndAsync().ConfigureAwait(false);
    }

    private static async Task<string> SendMailAsync(string mode, int port)
    {
        using var message = new MailMessage("sender@example.test", "recipient@example.test")
        {
            Subject = "sandbox async message",
            Body = "sandbox body",
            BodyEncoding = Encoding.UTF8
        };
        message.Attachments.Add(new Attachment(new MemoryStream(new byte[] { 1, 2, 3, 4, 5 }), "payload.bin"));
        using var client = new SmtpClient("leaf.acceptance.test", port)
        {
            EnableSsl = mode == "smtp-tls"
        };
        using var cancellation = new CancellationTokenSource();
        Task pending = client.SendMailAsync(message, cancellation.Token);
        if (mode == "smtp-cancel")
        {
            await Dns.GetHostAddressesAsync("cancel-gate.test").ConfigureAwait(false);
            cancellation.Cancel();
            try
            {
                await pending.ConfigureAwait(false);
                return "SMTP ignored cancellation";
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
                return "SMTP cancelled";
            }
        }
        await pending.ConfigureAwait(false);
        return "SMTP delivered";
    }

    private sealed class ClosingWebClient : WebClient
    {
        protected override WebRequest GetWebRequest(Uri address)
        {
            var request = (HttpWebRequest)base.GetWebRequest(address);
            request.Proxy = null;
            request.KeepAlive = false;
            return request;
        }
    }
}
