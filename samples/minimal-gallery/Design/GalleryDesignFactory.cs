using Microsoft.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore.Design;

namespace GallerySchema;

/// <summary>Creates the design-time context without opening the database.</summary>
public sealed class GalleryDesignFactory : IDesignTimeDbContextFactory<GalleryDb>
{
    /// <summary>Uses a migration connection supplied by the operator at invocation time.</summary>
    public GalleryDb CreateDbContext(string[] args) => new(new DbContextOptionsBuilder<GalleryDb>()
        .UseNpgsql("Host=localhost;Database=gallery;Username=gallery;SSL Mode=VerifyFull;Pooling=false").Options);
}
