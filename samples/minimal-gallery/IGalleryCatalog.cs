namespace MinimalGallery;

internal interface IGalleryCatalog
{
    Task Add(GalleryImage image, CancellationToken cancellation);
    Task<GalleryImage[]> List(CancellationToken cancellation);
    Task<GalleryImage?> Find(Guid id, CancellationToken cancellation);
}
