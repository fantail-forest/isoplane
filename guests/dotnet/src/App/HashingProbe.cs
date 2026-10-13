#if ISOPLANE_HASHING_PROBE
using System;
using System.Security.Cryptography;
using System.Text;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

public static class HashingProbe
{
    private static IncrementalHash? saved;
    private static SHA256? savedSha;

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {
        byte[] data = request.Body.ToArray();
        byte[] key = Convert.FromHexString(request.Query[0].Value);
        byte[] result;
        try
        {
            if (request.Path.StartsWith("/pbkdf2/", StringComparison.Ordinal))
            {
                string[] parts = request.Path.Split('/');
                int iterations = int.Parse(parts[3]);
                int count = int.Parse(parts[4]);
                result = Derive(parts[2], data, key, iterations, count);
            }
            else result = request.Path switch
            {
                "/pbkdf-null-password" => Rfc2898DeriveBytes.Pbkdf2((byte[])null!, key, 1, HashAlgorithmName.SHA256, 32),
                "/pbkdf-null-salt" => Rfc2898DeriveBytes.Pbkdf2(data, (byte[])null!, 1, HashAlgorithmName.SHA256, 32),
                "/pbkdf-sha1" => Rfc2898DeriveBytes.Pbkdf2(data, key, 1, HashAlgorithmName.SHA1, 32),
                "/hmac-instance" => InstanceHash(key, data),
                "/hmac-reset" => ResetHash(key, data),
                "/hmac-start" => StartHash(key, data),
                "/hmac-finish" => FinishHash(data),
                "/hmac-clone" => UnsupportedHash(key, true),
                "/hmac-current" => UnsupportedHash(key, false),
                "/hmac-sha1" => UnsupportedAlgorithm(key),
                "/hmac-disposed" => DisposedHash(key),
                "/sha-instance" => InstanceSha(data),
                "/sha-short-output" => ShortOutputSha(data),
                "/sha-clone" => UnsupportedShaOperation(true),
                "/sha-current" => UnsupportedShaOperation(false),
                "/sha-reset" => ResetSha(data),
                "/sha-start" => StartSha(data),
                "/sha-finish" => FinishSha(data),
                "/sha-instance-null" => InvalidSha(data, true),
                "/sha-instance-offset" => InvalidSha(data, false),
                "/sha-instance-disposed" => DisposedSha(),
                "/sha-unsupported" => UnsupportedSha(),
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
        catch (ArgumentException error)
        {
            return Task.FromResult(Response.Text($"{error.GetType().Name}:{error.ParamName}"));
        }
        catch (PlatformNotSupportedException)
        {
            return Task.FromResult(Response.Text("PlatformNotSupportedException"));
        }
        catch (ObjectDisposedException)
        {
            return Task.FromResult(Response.Text("ObjectDisposedException"));
        }
        return Task.FromResult(Response.Text(Convert.ToHexString(result)));
    }
    private static byte[] InstanceSha(byte[] data)
    {
        using var hash = SHA256.Create();
        byte[] first = hash.ComputeHash(data);
        if (!first.AsSpan().SequenceEqual(hash.ComputeHash(data)))
            throw new InvalidOperationException("Instance SHA256 retained previous input.");
        return first;
    }

    private static byte[] ShortOutputSha(byte[] data)
    {
        using var hash = SHA256.Create();
        byte[] shortOutput = new byte[31];
        if (hash.TryComputeHash(data, shortOutput, out int written) || written != 0)
            throw new InvalidOperationException("SHA256 accepted a short output buffer.");
        byte[] output = new byte[34];
        output[0] = 0x41;
        output[^1] = 0x42;
        if (!hash.TryComputeHash(data, output.AsSpan(1, 32), out written) || written != 32 ||
            output[0] != 0x41 || output[^1] != 0x42)
            throw new InvalidOperationException("SHA256 did not respect the destination span.");
        return output.AsSpan(1, 32).ToArray();
    }

    private static byte[] UnsupportedShaOperation(bool clone)
    {
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        if (!clone) return hash.GetCurrentHash();
        using var copy = hash.Clone();
        return copy.GetHashAndReset();
    }

    private static byte[] ResetSha(byte[] data)
    {
        using var hash = SHA256.Create();
        hash.TransformBlock(new byte[] { 0xff }, 0, 1, null, 0);
        hash.Initialize();
        int split = data.Length / 2;
        hash.TransformBlock(data, 0, split, null, 0);
        hash.TransformFinalBlock(data, split, data.Length - split);
        return hash.Hash ?? throw new InvalidOperationException("SHA256 did not finalize.");
    }

    private static byte[] StartSha(byte[] data)
    {
        savedSha?.Dispose();
        savedSha = SHA256.Create();
        savedSha.TransformBlock(data, 0, data.Length, null, 0);
        return Array.Empty<byte>();
    }

    private static byte[] FinishSha(byte[] data)
    {
        using var hash = savedSha ?? throw new InvalidOperationException("No captured SHA256 state.");
        savedSha = null;
        hash.TransformFinalBlock(data, 0, data.Length);
        return hash.Hash ?? throw new InvalidOperationException("Restored SHA256 did not finalize.");
    }

    private static byte[] InvalidSha(byte[] data, bool nullInput)
    {
        using var hash = SHA256.Create();
        return nullInput ? hash.ComputeHash((byte[])null!) : hash.ComputeHash(data, -1, data.Length);
    }

    private static byte[] DisposedSha()
    {
        var hash = SHA256.Create();
        hash.Dispose();
        hash.Dispose();
        return hash.ComputeHash(Array.Empty<byte>());
    }

    private static byte[] UnsupportedSha()
    {
        using var hash = SHA384.Create();
        return hash.ComputeHash(Array.Empty<byte>());
    }

    private static byte[] Derive(string kind, byte[] password, byte[] salt, int iterations, int count)
    {
        if (kind == "bytes") return Rfc2898DeriveBytes.Pbkdf2(password, salt, iterations, HashAlgorithmName.SHA256, count);
        if (kind == "string") return Rfc2898DeriveBytes.Pbkdf2(Encoding.UTF8.GetString(password), salt, iterations, HashAlgorithmName.SHA256, count);
        if (kind != "span") throw new InvalidOperationException("Unknown derivation fixture operation.");
        byte[] destination = new byte[count + 2];
        destination[0] = 0x41;
        destination[^1] = 0x42;
        Rfc2898DeriveBytes.Pbkdf2(password, salt, destination.AsSpan(1, count), iterations, HashAlgorithmName.SHA256);
        if (destination[0] != 0x41 || destination[^1] != 0x42)
            throw new InvalidOperationException("PBKDF2 overwrote destination sentinels.");
        return destination.AsSpan(1, count).ToArray();
    }

    private static byte[] InstanceHash(byte[] key, byte[] data)
    {
        using var hash = new HMACSHA256(key);
        byte[] first = hash.ComputeHash(data);
        if (!first.AsSpan().SequenceEqual(hash.ComputeHash(data)))
            throw new InvalidOperationException("Instance HMAC retained previous message data.");
        return first;
    }

    private static byte[] ResetHash(byte[] key, byte[] data)
    {
        using var hash = IncrementalHash.CreateHMAC(HashAlgorithmName.SHA256, key);
        hash.AppendData(data.AsSpan(0, data.Length / 2));
        hash.AppendData(data.AsSpan(data.Length / 2));
        byte[] first = hash.GetHashAndReset();
        hash.AppendData(data);
        if (!first.AsSpan().SequenceEqual(hash.GetHashAndReset()))
            throw new InvalidOperationException("Incremental HMAC did not reset its message.");
        return first;
    }

    private static byte[] StartHash(byte[] key, byte[] data)
    {
        saved?.Dispose();
        saved = IncrementalHash.CreateHMAC(HashAlgorithmName.SHA256, key);
        saved.AppendData(data);
        return Array.Empty<byte>();
    }

    private static byte[] FinishHash(byte[] data)
    {
        using var hash = saved ?? throw new InvalidOperationException("No captured keyed state.");
        saved = null;
        hash.AppendData(data);
        return hash.GetHashAndReset();
    }

    private static byte[] UnsupportedHash(byte[] key, bool clone)
    {
        using var hash = IncrementalHash.CreateHMAC(HashAlgorithmName.SHA256, key);
        if (!clone) return hash.GetCurrentHash();
        using var copy = hash.Clone();
        return copy.GetHashAndReset();
    }

    private static byte[] UnsupportedAlgorithm(byte[] key)
    {
        using var hash = IncrementalHash.CreateHMAC(HashAlgorithmName.SHA1, key);
        return hash.GetHashAndReset();
    }

    private static byte[] DisposedHash(byte[] key)
    {
        var hash = IncrementalHash.CreateHMAC(HashAlgorithmName.SHA256, key);
        hash.Dispose();
        hash.Dispose();
        hash.AppendData(key);
        return Array.Empty<byte>();
    }

}
#endif
