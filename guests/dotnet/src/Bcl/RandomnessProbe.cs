using System;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

public static class RandomnessProbe
{
    private static RandomNumberGenerator InstanceRandom = null!;

    public static Task InitializeAsync()
    {
        InstanceRandom = RandomNumberGenerator.Create();
        return Task.CompletedTask;
    }

    public static Task<Response> HandleAsync(Request request, string path)
    {
        if (path == "/instance")
        {
            byte[] slice = new byte[] { 0x11, 0x22, 0x33, 0x44 };
            InstanceRandom.GetBytes(slice, 1, 2);
            byte[] result = new byte[3];
            InstanceRandom.GetBytes(result.AsSpan());
            return Task.FromResult(Response.Text(Convert.ToHexString(slice) + ":" + Convert.ToHexString(result)));
        }
        if (path == "/instance-empty")
        {
            InstanceRandom.GetBytes(Array.Empty<byte>());
            InstanceRandom.GetBytes(Span<byte>.Empty);
            return Task.FromResult(Response.Text("0"));
        }
        if (path == "/instance-null")
        {
            try { InstanceRandom.GetBytes(null!); }
            catch (ArgumentNullException error)
            {
                return Task.FromResult(Response.Text($"{error.GetType().Name}:{error.ParamName}"));
            }
            throw new InvalidOperationException("A null random buffer was accepted.");
        }
        if (path == "/instance-offset" || path == "/instance-count")
        {
            int offset = path == "/instance-offset" ? -1 : 0;
            int count = path == "/instance-count" ? -1 : 0;
            try { InstanceRandom.GetBytes(new byte[1], offset, count); }
            catch (ArgumentOutOfRangeException error)
            {
                return Task.FromResult(Response.Text($"{error.GetType().Name}:{error.ParamName}"));
            }
            throw new InvalidOperationException("An invalid instance buffer range was accepted.");
        }
        if (path == "/values")
        {
            var name = new HashAlgorithmName("SHA256");
            if (name != HashAlgorithmName.SHA256 || name.Name != "SHA256")
                throw new InvalidOperationException("HashAlgorithmName lost its value semantics.");
            var certificates = new System.Security.Cryptography.X509Certificates.X509CertificateCollection();
            var oid = new Oid("1.2.840.113549.1.1.11", "sha256RSA");
            if (certificates.Count != 0 || new Oid(oid).FriendlyName != "sha256RSA")
                throw new InvalidOperationException("Managed crypto containers lost their state.");
            return Task.FromResult(Response.Text("values"));
        }
        if (path == "/empty")
        {
            RandomNumberGenerator.Fill(Span<byte>.Empty);
            return Task.FromResult(Response.Text(RandomNumberGenerator.GetBytes(0).Length.ToString()));
        }
        if (path == "/negative")
        {
            try { RandomNumberGenerator.GetBytes(-1); }
            catch (ArgumentOutOfRangeException error)
            {
                return Task.FromResult(Response.Text($"{error.GetType().Name}:{error.ParamName}:{error.ActualValue}"));
            }
            throw new InvalidOperationException("A negative random byte count was accepted.");
        }
        if (path == "/large" || path == "/instance-large")
        {
            int count = int.Parse(request.Query[0].Value);
            byte[] large;
            if (path == "/instance-large")
            {
                large = new byte[count];
                InstanceRandom.GetBytes(large);
            }
            else large = RandomNumberGenerator.GetBytes(count);
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
