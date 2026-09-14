using System.Text.Json.Serialization;

namespace MinimalGallery;

internal sealed record ExifInfo(string? Make, string? Model, string? DateTimeOriginal, int? Orientation);
internal sealed record GalleryImage(Guid Id, string OriginalContentType, int Width, int Height,
    int ThumbnailWidth, int ThumbnailHeight, ExifInfo Exif);

[JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
[JsonSerializable(typeof(GalleryImage))]
[JsonSerializable(typeof(GalleryImage[]))]
[JsonSerializable(typeof(ExifInfo))]
internal partial class GalleryJson : JsonSerializerContext;
