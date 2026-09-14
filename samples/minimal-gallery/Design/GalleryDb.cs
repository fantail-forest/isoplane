using Microsoft.EntityFrameworkCore;

namespace GallerySchema;

/// <summary>Owns the gallery schema for both workload implementations.</summary>
public sealed class GalleryDb(DbContextOptions<GalleryDb> options) : DbContext(options)
{
    /// <summary>Metadata for successfully ingested original and thumbnail objects.</summary>
    public DbSet<GalleryImageRow> Images => Set<GalleryImageRow>();

    /// <summary>Maps the common gallery workload table.</summary>
    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<GalleryImageRow>().ToTable("GalleryImages").HasKey(image => image.Id);
    }
}

/// <summary>A gallery image row shared by the minimal and framework samples.</summary>
public class GalleryImageRow
{
    /// <summary>The image and object-key identity.</summary>
    public Guid Id { get; set; }
    /// <summary>The detected media type of the preserved original.</summary>
    public required string OriginalContentType { get; set; }
    /// <summary>Original pixel width before applying EXIF orientation.</summary>
    public int Width { get; set; }
    /// <summary>Original pixel height before applying EXIF orientation.</summary>
    public int Height { get; set; }
    /// <summary>Thumbnail pixel width.</summary>
    public int ThumbnailWidth { get; set; }
    /// <summary>Thumbnail pixel height.</summary>
    public int ThumbnailHeight { get; set; }
    /// <summary>Extracted EXIF fields, with nulls for absent values.</summary>
    public required string ExifJson { get; set; }
}
