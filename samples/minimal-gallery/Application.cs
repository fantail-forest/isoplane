using System.Text.Json;
using Wisp;
using Wisp.Durable;

namespace MinimalGallery;

/// <summary>A gallery using maintained image, PostgreSQL, and S3 libraries without ASP.NET or EF Core.</summary>
public sealed class Application : IWispHandler
{
    /// <summary>Constructs the handler without reading credentials during preparation.</summary>
    public static IWispHandler Create() => new Application();

    /// <summary>Ingests original images and thumbnails, lists metadata, and retrieves stored image bytes.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        var parts = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0 || parts[0] != "images") return Response.Text("Not found", 404);
        if (parts.Length == 1 && request.Method is not ("GET" or "POST")) return Response.Text("Method not allowed", 405);
        var id = Guid.Empty;
        if (parts.Length != 1 && (parts.Length != 3 || !Guid.TryParse(parts[1], out id)
            || parts[2] is not ("original" or "thumbnail"))) return Response.Text("Not found", 404);
        if (parts.Length == 3 && request.Method != "GET") return Response.Text("Method not allowed", 405);
        var settings = GallerySettings.Read();
        IGalleryCatalog catalog = new GalleryCatalog(settings.ConnectionString);
        if (parts.Length == 1 && request.Method == "GET")
            return Response.Json(JsonSerializer.Serialize(await catalog.List(request.Aborted), GalleryJson.Default.GalleryImageArray));
        using var storage = new GalleryObjects(settings);
        if (parts.Length == 3)
        {
            var image = await catalog.Find(id, request.Aborted);
            if (image is null) return Response.Text("Not found", 404);
            var thumbnail = parts[2] == "thumbnail";
            return Response.Bytes(await storage.Read(id, thumbnail, request.Aborted), thumbnail ? "image/png" : image.OriginalContentType);
        }
        var original = request.Body.ToArray();
        ProcessedImage processed;
        try { processed = ImageProcessing.Process(original, settings.ThumbnailEdge); }
        catch (UnsupportedImageException) { return Response.Text("JPEG or PNG required", 415); }
        await GalleryIngest.Ingest(catalog, storage, original, processed, request.Aborted);
        return Response.Http(201, JsonSerializer.SerializeToUtf8Bytes(processed.Metadata, GalleryJson.Default.GalleryImage),
            [new("content-type", "application/json"), new("location", $"/images/{processed.Metadata.Id}/original")]);
    }

}
