using System.Text.Json;
using Isoplane;

namespace MinimalGallery;

/// <summary>A gallery using maintained image, PostgreSQL, and S3 libraries without ASP.NET or EF Core.</summary>
public sealed class Application : IIsoplaneHandler
{
    /// <summary>Constructs the handler and warms the read-only database endpoint before template capture.</summary>
    public static IIsoplaneHandler Create()
    {
        Initialization.Initializing += async () =>
        {
            _ = GalleryJson.Default.GalleryImage;
            _ = GalleryJson.Default.GalleryImageArray;
            _ = GalleryJson.Default.ExifInfo;
            _ = await HandleAsync("GET", "/images", ReadOnlyMemory<byte>.Empty, CancellationToken.None);
        };
        return new Application();
    }

    /// <summary>Ingests original images and thumbnails, lists metadata, and retrieves stored image bytes.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        HandleAsync(request.Method, request.Path, request.Body, request.Aborted);

    private static async Task<Response> HandleAsync(string method, string path, ReadOnlyMemory<byte> body, CancellationToken cancellation)
    {
        var parts = path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0 || parts[0] != "images") return Response.Text("Not found", 404);
        if (parts.Length == 1 && method is not ("GET" or "POST")) return Response.Text("Method not allowed", 405);
        var id = Guid.Empty;
        if (parts.Length != 1 && (parts.Length != 3 || !Guid.TryParse(parts[1], out id)
            || parts[2] is not ("original" or "thumbnail"))) return Response.Text("Not found", 404);
        if (parts.Length == 3 && method != "GET") return Response.Text("Method not allowed", 405);
        var settings = GallerySettings.Read();
        IGalleryCatalog catalog = new GalleryCatalog(settings.ConnectionString);
        if (parts.Length == 1 && method == "GET")
            return Response.Json(JsonSerializer.Serialize(await catalog.List(cancellation), GalleryJson.Default.GalleryImageArray));
        using var storage = new GalleryObjects(settings);
        if (parts.Length == 3)
        {
            var image = await catalog.Find(id, cancellation);
            if (image is null) return Response.Text("Not found", 404);
            var thumbnail = parts[2] == "thumbnail";
            return Response.Bytes(await storage.Read(id, thumbnail, cancellation), thumbnail ? "image/png" : image.OriginalContentType);
        }
        var original = body.ToArray();
        ProcessedImage processed;
        try { processed = ImageProcessing.Process(original, settings.ThumbnailEdge); }
        catch (UnsupportedImageException) { return Response.Text("JPEG or PNG required", 415); }
        await GalleryIngest.Ingest(catalog, storage, original, processed, cancellation);
        return Response.Http(201, JsonSerializer.SerializeToUtf8Bytes(processed.Metadata, GalleryJson.Default.GalleryImage),
            [new("content-type", "application/json"), new("location", $"/images/{processed.Metadata.Id}/original")]);
    }

}
