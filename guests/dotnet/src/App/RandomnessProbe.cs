#if WISP_RANDOMNESS_PROBE
using System;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

public sealed class RandomnessProbe : IWispHandler
{
    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        if (request.Path == "/empty")
        {
            RandomNumberGenerator.Fill(Span<byte>.Empty);
            return Task.FromResult(Response.Text(RandomNumberGenerator.GetBytes(0).Length.ToString()));
        }
        if (request.Path == "/negative")
        {
            try { RandomNumberGenerator.GetBytes(-1); }
            catch (ArgumentOutOfRangeException error)
            {
                return Task.FromResult(Response.Text($"{error.GetType().Name}:{error.ParamName}:{error.ActualValue}"));
            }
            throw new InvalidOperationException("A negative random byte count was accepted.");
        }
        if (request.Path == "/large")
        {
            int count = int.Parse(request.Query[0].Value);
            byte[] large = RandomNumberGenerator.GetBytes(count);
            int transitions = 0;
            for (int index = 1; index < large.Length; index++)
                if (large[index] != large[index - 1]) transitions++;
            return Task.FromResult(Response.Text($"{large.Length}:{large[0]:X2}:{large[^1]:X2}:{transitions}"));
        }
        byte[] bytes = new byte[] { 0x11, 0x22, 0x33, 0x44 };
        RandomNumberGenerator.Fill(bytes.AsSpan(1, 2));
        byte[] allocated = RandomNumberGenerator.GetBytes(3);
        return Task.FromResult(Response.Text(Convert.ToHexString(bytes) + ":" + Convert.ToHexString(allocated)));
    }
}
#endif
