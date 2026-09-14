using System.Text.Json;
using GallerySchema;
using Microsoft.EntityFrameworkCore;
using MinimalGallery;
using GalleryImage = MinimalGallery.GalleryImage;
using ImageRow = GallerySchema.GalleryImageRow;

namespace AspNetGallery;

internal sealed class EfGalleryCatalog(GalleryDb db) : IGalleryCatalog
{
    public async Task Add(GalleryImage image, CancellationToken cancellation)
    {
        db.Add(new ImageRow
        {
            Id = image.Id,
            OriginalContentType = image.OriginalContentType,
            Width = image.Width,
            Height = image.Height,
            ThumbnailWidth = image.ThumbnailWidth,
            ThumbnailHeight = image.ThumbnailHeight,
            ExifJson = JsonSerializer.Serialize(image.Exif, GalleryJson.Default.ExifInfo)
        });
        await db.SaveChangesAsync(cancellation);
    }

    public async Task<GalleryImage[]> List(CancellationToken cancellation)
    {
        var context = db;
        CancellationToken token = cancellation;
        var images = await context.Images.OrderBy(image => image.Id).ToArrayAsync(token);
        return images.Select(ToImage).ToArray();
    }

    public async Task<GalleryImage?> Find(Guid id, CancellationToken cancellation)
    {
        var context = db;
        Guid imageId = id;
        CancellationToken token = cancellation;
        var image = await context.Images.SingleOrDefaultAsync(image => image.Id == imageId, token);
        return image is null ? null : ToImage(image);
    }

    private static GalleryImage ToImage(ImageRow row) => new(row.Id, row.OriginalContentType,
        row.Width, row.Height, row.ThumbnailWidth, row.ThumbnailHeight,
        JsonSerializer.Deserialize(row.ExifJson, GalleryJson.Default.ExifInfo)
            ?? throw new InvalidOperationException("Persisted image metadata is missing."));
}
