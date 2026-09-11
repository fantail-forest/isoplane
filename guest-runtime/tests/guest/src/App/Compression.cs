using System;
using System.Collections.Generic;
using System.Formats.Tar;
using System.Globalization;
using System.IO;
using System.IO.Compression;
using System.Text;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Compression through the pack's native zlib: the stream codecs round-trip
/// and reject corrupt input, archives (zip, tar) build and read back in
/// memory, a codec's native state survives parks between its writes, and
/// anything that needs the disk fails the declared way.
/// </summary>
internal sealed class Compression
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "round-trip":
                return RoundTrip(path[1]);
            case "corrupt":
                return Corrupt();
            case "truncated":
                return Truncated();
            case "zip":
                return Zip();
            case "tar":
                return Tar();
            case "disk":
                return Disk();
            case "gzip-across-parks":
                return await GzipAcrossParks().ConfigureAwait(false);
            default:
                throw new ArgumentException("unknown compression case " + path[0]);
        }
    }

    /// <summary>
    /// Compresses a compressible buffer with the named codec and decodes it
    /// back: the first two bytes of the encoding, whether it shrank, and
    /// whether it restored. A codec without a native backend answers with
    /// the exception it throws.
    /// </summary>
    private static string RoundTrip(string codec)
    {
        byte[] plain = Pattern(64 * 1024);
        try
        {
            using var compressed = new MemoryStream();
            using (Stream encoder = Encoder(codec, compressed))
            {
                encoder.Write(plain);
            }
            byte[] bytes = compressed.ToArray();
            using var restored = new MemoryStream();
            using (Stream decoder = Decoder(codec, new MemoryStream(bytes)))
            {
                decoder.CopyTo(restored);
            }
            return Convert.ToHexString(bytes.AsSpan(0, 2)).ToLowerInvariant()
                + " smaller=" + (bytes.Length < plain.Length)
                + " restored=" + restored.ToArray().AsSpan().SequenceEqual(plain);
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static Stream Encoder(string codec, Stream sink) => codec switch
    {
        "gzip" => new GZipStream(sink, CompressionLevel.Optimal, leaveOpen: true),
        "deflate" => new DeflateStream(sink, CompressionLevel.Optimal, leaveOpen: true),
        "zlib" => new ZLibStream(sink, CompressionLevel.Optimal, leaveOpen: true),
        "brotli" => new BrotliStream(sink, CompressionLevel.Optimal, leaveOpen: true),
        _ => throw new ArgumentException("unknown codec " + codec),
    };

    private static Stream Decoder(string codec, Stream source) => codec switch
    {
        "gzip" => new GZipStream(source, CompressionMode.Decompress),
        "deflate" => new DeflateStream(source, CompressionMode.Decompress),
        "zlib" => new ZLibStream(source, CompressionMode.Decompress),
        "brotli" => new BrotliStream(source, CompressionMode.Decompress),
        _ => throw new ArgumentException("unknown codec " + codec),
    };

    /// <summary>Bytes that are not a gzip member, decoded as one.</summary>
    private static string Corrupt()
    {
        try
        {
            using var decoder = new GZipStream(new MemoryStream(Pattern(4096)), CompressionMode.Decompress);
            using var sink = new MemoryStream();
            decoder.CopyTo(sink);
            return "decoded " + sink.Length.ToString(CultureInfo.InvariantCulture);
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    /// <summary>A gzip member cut before its trailer.</summary>
    private static string Truncated()
    {
        byte[] plain = Pattern(64 * 1024);
        using var compressed = new MemoryStream();
        using (var encoder = new GZipStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            encoder.Write(plain);
        }
        byte[] cut = compressed.ToArray()[..^16];
        try
        {
            using var decoder = new GZipStream(new MemoryStream(cut), CompressionMode.Decompress);
            using var sink = new MemoryStream();
            decoder.CopyTo(sink);
            return "decoded " + (sink.Length < plain.Length ? "less" : "all");
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    /// <summary>
    /// A zip archive built in memory with a deflated and a stored entry,
    /// reopened and listed.
    /// </summary>
    private static string Zip()
    {
        byte[] text = Encoding.ASCII.GetBytes(new string('z', 4000));
        byte[] stored = Pattern(1000);
        using var buffer = new MemoryStream();
        using (var archive = new ZipArchive(buffer, ZipArchiveMode.Create, leaveOpen: true))
        {
            using (Stream entry = archive.CreateEntry("docs/text.txt", CompressionLevel.Optimal).Open())
            {
                entry.Write(text);
            }
            using (Stream entry = archive.CreateEntry("raw.bin", CompressionLevel.NoCompression).Open())
            {
                entry.Write(stored);
            }
        }
        buffer.Position = 0;
        using var reopened = new ZipArchive(buffer, ZipArchiveMode.Read);
        var lines = new List<string>();
        foreach (ZipArchiveEntry entry in reopened.Entries)
        {
            using var content = new MemoryStream();
            using (Stream stream = entry.Open())
            {
                stream.CopyTo(content);
            }
            byte[] expected = entry.FullName == "raw.bin" ? stored : text;
            lines.Add(entry.FullName + ":" + entry.Length.ToString(CultureInfo.InvariantCulture)
                + ":" + (entry.CompressedLength < entry.Length ? "deflated" : "stored")
                + ":" + (content.ToArray().AsSpan().SequenceEqual(expected) ? "same" : "different"));
        }
        return string.Join(" ", lines);
    }

    /// <summary>
    /// A PAX tar written to memory and read back entry by entry, or the
    /// exception the library throws where it has no wasi implementation.
    /// </summary>
    private static string Tar()
    {
        byte[] payload = Encoding.ASCII.GetBytes("hello from tar");
        try
        {
            using var buffer = new MemoryStream();
            using (var writer = new TarWriter(buffer, TarEntryFormat.Pax, leaveOpen: true))
            {
                writer.WriteEntry(new PaxTarEntry(TarEntryType.Directory, "dir/"));
                var file = new PaxTarEntry(TarEntryType.RegularFile, "dir/hello.txt")
                {
                    DataStream = new MemoryStream(payload),
                };
                writer.WriteEntry(file);
            }
            buffer.Position = 0;
            using var reader = new TarReader(buffer);
            var lines = new List<string>();
            while (reader.GetNextEntry() is TarEntry entry)
            {
                string data = "";
                if (entry.DataStream is Stream stream)
                {
                    using var content = new MemoryStream();
                    stream.CopyTo(content);
                    data = Encoding.ASCII.GetString(content.ToArray());
                }
                lines.Add(entry.Name + ":" + entry.EntryType + ":" + entry.Format
                    + ":" + entry.Length.ToString(CultureInfo.InvariantCulture) + ":" + data);
            }
            return string.Join(" ", lines);
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    /// <summary>The archive helper that needs a file on the empty disk.</summary>
    private static string Disk()
    {
        try
        {
            using ZipArchive archive = ZipFile.Open("/archive.zip", ZipArchiveMode.Create);
            return "created";
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    /// <summary>
    /// Feeds an encoder four chunks with a park between each, parks once
    /// more, then decodes in four exact reads with a park between each: nine
    /// parks in all, each with live zlib state in the guest's memory.
    /// </summary>
    private static async Task<string> GzipAcrossParks()
    {
        byte[] plain = Pattern(200_000);
        using var compressed = new MemoryStream();
        using (var encoder = new GZipStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            for (int chunk = 0; chunk < 4; chunk++)
            {
                await encoder.WriteAsync(plain.AsMemory(chunk * 50_000, 50_000)).ConfigureAwait(false);
                await Task.Delay(10).ConfigureAwait(false);
            }
        }
        await Task.Delay(10).ConfigureAwait(false);
        byte[] restored = new byte[plain.Length];
        using (var decoder = new GZipStream(new MemoryStream(compressed.ToArray()), CompressionMode.Decompress))
        {
            for (int chunk = 0; chunk < 4; chunk++)
            {
                await decoder.ReadExactlyAsync(restored.AsMemory(chunk * 50_000, 50_000)).ConfigureAwait(false);
                await Task.Delay(10).ConfigureAwait(false);
            }
        }
        return "compressed=" + (compressed.Length < plain.Length)
            + " restored=" + restored.AsSpan().SequenceEqual(plain);
    }

    /// <summary>Compressible text of exactly <paramref name="length"/> bytes.</summary>
    private static byte[] Pattern(int length)
    {
        var text = new StringBuilder(length + 32);
        int line = 0;
        while (text.Length < length)
        {
            text.Append("line ").Append(line++).Append(" of the acceptance pattern\n");
        }
        return Encoding.ASCII.GetBytes(text.ToString(0, length));
    }
}
