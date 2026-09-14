using Amazon.Runtime;
using Amazon.S3;
using Amazon.S3.Model;

namespace MinimalGallery;

internal interface IGalleryObjects
{
    Task Put(Guid id, byte[] original, ProcessedImage processed, CancellationToken cancellation);
    Task Remove(Guid id, CancellationToken cancellation);
    Task<byte[]> Read(Guid id, bool thumbnail, CancellationToken cancellation);
}

internal sealed class GalleryObjects : IGalleryObjects, IDisposable
{
    private readonly AmazonS3Client client;
    private readonly string bucket;

    internal GalleryObjects(GallerySettings settings)
    {
        bucket = settings.Bucket;
        client = new(new BasicAWSCredentials(settings.AccessKey, settings.SecretKey), new AmazonS3Config
        {
            ServiceURL = settings.Endpoint,
            AuthenticationRegion = settings.Region,
            ForcePathStyle = true,
        });
    }

    public async Task Put(Guid id, byte[] original, ProcessedImage processed, CancellationToken cancellation)
    {
        await Upload(Key(id, thumbnail: false), original, processed.Metadata.OriginalContentType, cancellation);
        await Upload(Key(id, thumbnail: true), processed.Thumbnail, "image/png", cancellation);
    }

    private async Task Upload(string key, byte[] bytes, string contentType, CancellationToken cancellation)
    {
        using var input = new MemoryStream(bytes, writable: false);
        await client.PutObjectAsync(new PutObjectRequest
        {
            BucketName = bucket, Key = key, InputStream = input, ContentType = contentType,
            UseChunkEncoding = false,
        }, cancellation);
    }

    public async Task Remove(Guid id, CancellationToken cancellation)
    {
        await client.DeleteObjectAsync(bucket, Key(id, thumbnail: false), cancellation);
        await client.DeleteObjectAsync(bucket, Key(id, thumbnail: true), cancellation);
    }

    public async Task<byte[]> Read(Guid id, bool thumbnail, CancellationToken cancellation)
    {
        using var response = await client.GetObjectAsync(bucket, Key(id, thumbnail), cancellation);
        using var output = new MemoryStream();
        await response.ResponseStream.CopyToAsync(output, cancellation);
        return output.ToArray();
    }

    private static string Key(Guid id, bool thumbnail) => $"{id:N}/{(thumbnail ? "thumbnail.png" : "original")}";

    public void Dispose() => client.Dispose();
}
