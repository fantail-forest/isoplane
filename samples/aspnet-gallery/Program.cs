using AspNetGallery;
using GallerySchema;
using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.EntityFrameworkCore;
using MinimalGallery;
using Npgsql;

// A PostgreSQL and S3 gallery with request-scoped EF persistence.

// EF compiled models initialize synchronously because guests cannot start OS threads.
AppContext.SetSwitch("Microsoft.EntityFrameworkCore.Issue31751", true);
var builder = IsoplaneHosting.CreateBuilder(args);
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
GalleryEndpoints.Map(application);

// Serialization metadata and the read-only database endpoint, warmed before template capture.
Initialization.Initializing += async () =>
{
    _ = GallerySchema.Generated.GalleryDbModel.Instance.GetEntityTypes().ToArray();
    _ = application.Services.GetRequiredService<Microsoft.Extensions.Options.IOptions<Microsoft.AspNetCore.Http.Json.JsonOptions>>().Value;
    _ = GalleryJson.Default.GalleryImage;
    _ = GalleryJson.Default.GalleryImageArray;
    _ = GalleryJson.Default.ExifInfo;
    await application.WarmupAsync("/images");
};

await application.RunAsync();
