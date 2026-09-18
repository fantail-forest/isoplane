using System.Globalization;
using Isoplane;

namespace MinimalGallery;

internal sealed record GallerySettings(string ConnectionString, string Endpoint, string Region,
    string Bucket, string AccessKey, string SecretKey, int ThumbnailEdge)
{
    internal static GallerySettings Read()
    {
        var values = RuntimeConfiguration.ReadValues();
        var edge = values.TryGetValue("Gallery:ThumbnailEdge", out var configured)
            ? int.Parse(configured, NumberStyles.None, CultureInfo.InvariantCulture) : 256;
        if (edge <= 0) throw new InvalidOperationException("Gallery:ThumbnailEdge must be positive.");
        return new(Required("ConnectionStrings:Gallery"), Required("Gallery:S3:Endpoint"),
            Required("Gallery:S3:Region"), Required("Gallery:S3:Bucket"), Required("Gallery:S3:AccessKey"),
            Required("Gallery:S3:SecretKey"), edge);

        string Required(string key) => values.TryGetValue(key, out var value) && !string.IsNullOrWhiteSpace(value)
            ? value : throw new InvalidOperationException($"Set {key} through runtime configuration.");
    }
}
