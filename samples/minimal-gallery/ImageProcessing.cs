using MetadataExtractor;
using MetadataExtractor.Formats.Exif;
using MetadataExtractor.Util;
using StbImageResizeSharp;
using StbImageSharp;

namespace MinimalGallery;

internal sealed record ProcessedImage(GalleryImage Metadata, byte[] Thumbnail);

internal static class ImageProcessing
{
    internal static ProcessedImage Process(byte[] original, int thumbnailEdge)
    {
        using var input = new MemoryStream(original, writable: false);
        var contentType = FileTypeDetector.DetectFileType(input) switch
        {
            FileType.Jpeg => "image/jpeg",
            FileType.Png => "image/png",
            _ => throw new UnsupportedImageException()
        };
        input.Position = 0;
        var directories = ImageMetadataReader.ReadMetadata(input);
        var exif = ReadExif(directories);
        input.Position = 0;
        var image = ImageResult.FromStream(input, ColorComponents.RedGreenBlueAlpha);
        var scale = Math.Min(1.0, thumbnailEdge / (double)Math.Max(image.Width, image.Height));
        var width = Math.Max(1, (int)Math.Floor(image.Width * scale));
        var height = Math.Max(1, (int)Math.Floor(image.Height * scale));
        var pixels = new byte[checked(width * height * 4)];
        if (StbImageResize.stbir_resize_uint8(image.Data, image.Width, image.Height, checked(image.Width * 4),
            pixels, width, height, checked(width * 4), 4) == 0)
            throw new InvalidOperationException("Image resizing failed.");
        using var output = new MemoryStream();
        new StbImageWriteSharp.ImageWriter().WritePng(pixels, width, height,
            StbImageWriteSharp.ColorComponents.RedGreenBlueAlpha, output);
        return new(new(Guid.NewGuid(), contentType, image.Width, image.Height, width, height, exif), output.ToArray());
    }

    private static ExifInfo ReadExif(IReadOnlyList<MetadataExtractor.Directory> directories)
    {
        var primary = directories.OfType<ExifIfd0Directory>().FirstOrDefault();
        var details = directories.OfType<ExifSubIfdDirectory>().FirstOrDefault();
        int? orientation = primary is not null && primary.TryGetInt32(ExifDirectoryBase.TagOrientation, out var value)
            ? value : null;
        return new(primary?.GetString(ExifDirectoryBase.TagMake), primary?.GetString(ExifDirectoryBase.TagModel),
            details?.GetString(ExifDirectoryBase.TagDateTimeOriginal), orientation);
    }
}

internal sealed class UnsupportedImageException : Exception;
