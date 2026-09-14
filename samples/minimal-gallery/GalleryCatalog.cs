using System.Text.Json;
using Npgsql;

namespace MinimalGallery;

internal sealed class GalleryCatalog(string configured) : IGalleryCatalog
{
    private readonly string connectionString = new NpgsqlConnectionStringBuilder(configured)
    { SslMode = SslMode.VerifyFull, Pooling = false }.ConnectionString;
    private const string Columns = "\"Id\", \"OriginalContentType\", \"Width\", \"Height\", \"ThumbnailWidth\", \"ThumbnailHeight\", \"ExifJson\"";

    public async Task Add(GalleryImage image, CancellationToken cancellation)
    {
        await using var connection = await Open(cancellation);
        await using var command = new NpgsqlCommand($"INSERT INTO \"GalleryImages\" ({Columns}) VALUES ($1, $2, $3, $4, $5, $6, $7)", connection);
        command.Parameters.AddWithValue(image.Id);
        command.Parameters.AddWithValue(image.OriginalContentType);
        command.Parameters.AddWithValue(image.Width);
        command.Parameters.AddWithValue(image.Height);
        command.Parameters.AddWithValue(image.ThumbnailWidth);
        command.Parameters.AddWithValue(image.ThumbnailHeight);
        command.Parameters.AddWithValue(JsonSerializer.Serialize(image.Exif, GalleryJson.Default.ExifInfo));
        await command.ExecuteNonQueryAsync(cancellation);
    }

    public async Task<GalleryImage[]> List(CancellationToken cancellation)
    {
        await using var connection = await Open(cancellation);
        await using var command = new NpgsqlCommand($"SELECT {Columns} FROM \"GalleryImages\" ORDER BY \"Id\"", connection);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        var images = new List<GalleryImage>();
        while (await reader.ReadAsync(cancellation)) images.Add(Row(reader));
        return images.ToArray();
    }

    public async Task<GalleryImage?> Find(Guid id, CancellationToken cancellation)
    {
        await using var connection = await Open(cancellation);
        await using var command = new NpgsqlCommand($"SELECT {Columns} FROM \"GalleryImages\" WHERE \"Id\" = $1", connection);
        command.Parameters.AddWithValue(id);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        return await reader.ReadAsync(cancellation) ? Row(reader) : null;
    }

    private async Task<NpgsqlConnection> Open(CancellationToken cancellation)
    {
        var connection = new NpgsqlConnection(connectionString);
        try { await connection.OpenAsync(cancellation); return connection; }
        catch { await connection.DisposeAsync(); throw; }
    }

    private static GalleryImage Row(NpgsqlDataReader reader) => new(reader.GetGuid(0), reader.GetString(1),
        reader.GetInt32(2), reader.GetInt32(3), reader.GetInt32(4), reader.GetInt32(5),
        JsonSerializer.Deserialize(reader.GetString(6), GalleryJson.Default.ExifInfo)
            ?? throw new InvalidOperationException("Persisted image metadata is missing."));
}
