#if WISP_HASHING_PROBE
using System;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

public sealed class HashingProbe : IWispHandler
{
    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        byte[] data = request.Body.ToArray();
        byte[] key = Convert.FromHexString(request.Query[0].Value);
        byte[] result;
        try
        {
            result = request.Path switch
            {
                "/sha-array" => SHA256.HashData(data),
                "/sha-span" => SHA256.HashData(data.AsSpan()),
                "/hmac-array" => HMACSHA256.HashData(key, data),
                "/hmac-span" => HMACSHA256.HashData(key.AsSpan(), data.AsSpan()),
                "/sha-null" => SHA256.HashData((byte[])null!),
                "/hmac-null-key" => HMACSHA256.HashData((byte[])null!, data),
                "/hmac-null-source" => HMACSHA256.HashData(key, (byte[])null!),
                _ => throw new InvalidOperationException("Unknown hash fixture operation.")
            };
        }
        catch (ArgumentNullException error)
        {
            return Task.FromResult(Response.Text($"ArgumentNullException:{error.ParamName}"));
        }
        return Task.FromResult(Response.Text(Convert.ToHexString(result)));
    }
}
#endif
