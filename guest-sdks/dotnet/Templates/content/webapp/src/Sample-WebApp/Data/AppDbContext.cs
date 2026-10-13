using Microsoft.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore;
using Sample_WebApp.Notes;

namespace Sample_WebApp.Data;

/// <summary>
/// The application's database. To change the schema, change the entities or the
/// model below, then add a migration:
/// <c>dotnet ef migrations add NAME --project src/Sample-WebApp</c>.
/// </summary>
public sealed class AppDbContext(DbContextOptions<AppDbContext> options) : DbContext(options), IDataProtectionKeyContext
{
    public DbSet<Note> Notes => Set<Note>();

    /// <summary>
    /// The keys that protect antiforgery tokens and cookies. Program.cs explains
    /// why they are kept here.
    /// </summary>
    public DbSet<DataProtectionKey> DataProtectionKeys => Set<DataProtectionKey>();

    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<Note>(note =>
        {
            note.Property(item => item.Title).HasMaxLength(Note.TitleLength);
            note.Property(item => item.Body).HasMaxLength(Note.BodyLength);
            // The home page lists notes by this column.
            note.HasIndex(item => item.UpdatedAt);
        });
    }
}
