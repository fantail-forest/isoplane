using GallerySchema;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;
using MinimalGallery;
using Npgsql;
using Isoplane.AspNetCore;

namespace AspNetGallery;

/// <summary>A PostgreSQL and S3 gallery with request-scoped EF persistence.</summary>
public static class Application
{
    /// <summary>Builds routes and warms the read-only database endpoint before template capture.</summary>
    public static WebApplication Create()
    {
        // EF compiled models initialize synchronously because guests cannot start OS threads.
        AppContext.SetSwitch("Microsoft.EntityFrameworkCore.Issue31751", true);
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        builder.Services.ConfigureHttpJsonOptions(options =>
            options.SerializerOptions.TypeInfoResolverChain.Insert(0, GalleryJson.Default));
        builder.Services.AddScoped(_ => GallerySettings.Read());
        builder.Services.AddDbContext<GalleryDb>((services, options) =>
        {
            var settings = services.GetRequiredService<GallerySettings>();
            var connection = new NpgsqlConnectionStringBuilder(settings.ConnectionString)
            {
                SslMode = SslMode.VerifyFull,
                Pooling = false
            };
            options.UseModel(GallerySchema.Generated.GalleryDbModel.Instance)
                .UseNpgsql(connection.ConnectionString);
        });
        builder.Services.AddScoped<IGalleryCatalog, EfGalleryCatalog>();
        builder.Services.AddScoped<IGalleryObjects>(services =>
            new GalleryObjects(services.GetRequiredService<GallerySettings>()));
        var application = builder.Build();
        application.MapPost("/images", IngestAsync);
        application.MapGet("/images", ListAsync);
        application.MapGet("/images/{id:guid}/original", OriginalAsync);
        application.MapGet("/images/{id:guid}/thumbnail", ThumbnailAsync);
        Isoplane.Initialization.Initializing += async () =>
        {
            _ = GallerySchema.Generated.GalleryDbModel.Instance.GetEntityTypes().ToArray();
            _ = application.Services.GetRequiredService<Microsoft.Extensions.Options.IOptions<Microsoft.AspNetCore.Http.Json.JsonOptions>>().Value;
            _ = GalleryJson.Default.GalleryImage;
            _ = GalleryJson.Default.GalleryImageArray;
            _ = GalleryJson.Default.ExifInfo;
            await new IsoplaneApplication(application).WarmupAsync("/images");
        };
        return application;
    }

    private static async Task<IResult> IngestAsync(HttpContext context, GallerySettings settings,
        IGalleryCatalog catalog, IGalleryObjects objects)
    {
        using var input = new MemoryStream();
        await context.Request.Body.CopyToAsync(input, context.RequestAborted);
        var original = input.ToArray();
        ProcessedImage processed;
        try { processed = ImageProcessing.Process(original, settings.ThumbnailEdge); }
        catch (UnsupportedImageException) { return Results.Text("JPEG or PNG required", statusCode: 415); }
        await GalleryIngest.Ingest(catalog, objects, original, processed, context.RequestAborted);
        return Results.Created($"/images/{processed.Metadata.Id}/original", processed.Metadata);
    }

    private static async Task<IResult> ListAsync(IGalleryCatalog catalog, CancellationToken cancellation) =>
        Results.Json(await catalog.List(cancellation), GalleryJson.Default.GalleryImageArray);

    private static Task<IResult> OriginalAsync(Guid id, IGalleryCatalog catalog,
        IGalleryObjects objects, CancellationToken cancellation) => ReadAsync(id, false, catalog, objects, cancellation);

    private static Task<IResult> ThumbnailAsync(Guid id, IGalleryCatalog catalog,
        IGalleryObjects objects, CancellationToken cancellation) => ReadAsync(id, true, catalog, objects, cancellation);

    private static async Task<IResult> ReadAsync(Guid id, bool thumbnail, IGalleryCatalog catalog,
        IGalleryObjects objects, CancellationToken cancellation)
    {
        var image = await catalog.Find(id, cancellation);
        if (image is null) return Results.Text("Not found", statusCode: 404);
        return Results.Bytes(await objects.Read(id, thumbnail, cancellation),
            thumbnail ? "image/png" : image.OriginalContentType);
    }
}
