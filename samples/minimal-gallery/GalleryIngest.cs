namespace MinimalGallery;

internal static class GalleryIngest
{
    internal static async Task Ingest(IGalleryCatalog catalog, IGalleryObjects objects, byte[] original,
        ProcessedImage processed, CancellationToken cancellation)
    {
        try
        {
            await objects.Put(processed.Metadata.Id, original, processed, cancellation);
        }
        catch (Exception failure)
        {
            try { await objects.Remove(processed.Metadata.Id, CancellationToken.None); }
            catch (Exception cleanup) { throw new AggregateException("Image ingest and object cleanup failed.", failure, cleanup); }
            throw;
        }
        // A failed database acknowledgement can still mean the INSERT committed;
        // retaining the objects avoids deleting data referenced by that row.
        await catalog.Add(processed.Metadata, cancellation);
    }
}
