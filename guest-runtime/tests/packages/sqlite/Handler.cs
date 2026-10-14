using System;
using System.Globalization;
using System.Linq;
using System.Threading.Tasks;
using Isoplane;
using Microsoft.Data.Sqlite;
using Microsoft.EntityFrameworkCore;
using SQLitePCL;

namespace Sqlite;

/// <summary>
/// Uses SQLite through Microsoft.Data.Sqlite and EF Core as an application
/// would, answering after one host timer so each request parks once.
/// </summary>
public static class Handler
{
    private const string Reference = "Data Source=reference;Mode=Memory;Cache=Shared";

    // An in-memory database lives while a connection to it is open.
    private static SqliteConnection? reference;

    /// <summary>
    /// Advances SQLite's generator before the template is captured, through SQLitePCLRaw: the
    /// first <see cref="SqliteConnection"/> starts a timer that initialization refuses.
    /// </summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        Batteries_V2.Init();
        Check(raw.sqlite3_open(":memory:", out sqlite3 database), "open");
        using (database)
        {
            Check(raw.sqlite3_prepare_v2(database, "SELECT random()", out sqlite3_stmt statement), "prepare");
            using (statement)
                Check(raw.sqlite3_step(statement), "step", raw.SQLITE_ROW);
        }
        return Task.CompletedTask;
    }

    private static void Check(int result, string step, int expected = raw.SQLITE_OK)
    {
        if (result != expected)
            throw new InvalidOperationException($"SQLite {step} during initialization returned {result}");
    }

    /// <summary>Loads reference data in each instance, which starts from the captured template.</summary>
    [Activated]
    public static async Task ActivateAsync()
    {
        reference = new SqliteConnection(Reference);
        await reference.OpenAsync();
        await ExecuteAsync(reference, "CREATE TABLE country (code TEXT PRIMARY KEY, name TEXT); " +
            "INSERT INTO country VALUES ('NZ', 'New Zealand'), ('FR', 'France');");
    }

    /// <summary>Answers the outcome of the use the request path names.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        try
        {
            return Response.Text(req.Path switch
            {
                "/memory" => await MemoryAsync(),
                "/reference" => await ReferenceAsync(),
                "/shared" => await SharedAsync(),
                "/catalog" => await CatalogAsync(),
                "/file" => await FileAsync(),
                "/locked" => await LockedAsync(),
                "/random" => await RandomAsync(),
                _ => throw new ArgumentException("unknown SQLite probe")
            });
        }
        catch (Exception error) when (error is not ArgumentException)
        {
            return Response.Text(error.GetType().Name + ": " + error.Message);
        }
    }

    // A private in-memory database, with a function and a collation of the application's own.
    private static async Task<string> MemoryAsync()
    {
        await using var connection = new SqliteConnection("Data Source=:memory:");
        connection.CreateFunction("twice", (long value) => value * 2);
        connection.CreateCollation("reversed", (x, y) => string.CompareOrdinal(y, x));
        await connection.OpenAsync();
        await ExecuteAsync(connection, "CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('a'), ('c'), ('b');");
        string order = await ScalarAsync(connection, "SELECT group_concat(body, '') FROM (SELECT body FROM note ORDER BY body COLLATE reversed)");
        string twice = await ScalarAsync(connection, "SELECT twice(count(*)) FROM note");
        return order + ";" + twice;
    }

    // The reference data the instance loaded as it started, from a connection opened now.
    private static async Task<string> ReferenceAsync()
    {
        await using var connection = new SqliteConnection(Reference);
        await connection.OpenAsync();
        return await ScalarAsync(connection, "SELECT group_concat(name, ',') FROM (SELECT name FROM country ORDER BY code)");
    }

    // A named shared-cache database: a second connection sees what the first wrote.
    private static async Task<string> SharedAsync()
    {
        const string shared = "Data Source=shared;Mode=Memory;Cache=Shared";
        await using var first = new SqliteConnection(shared);
        await first.OpenAsync();
        await ExecuteAsync(first, "CREATE TABLE city (name TEXT); INSERT INTO city VALUES ('Paris'), ('Auckland');");
        await using var second = new SqliteConnection(shared);
        await second.OpenAsync();
        await ExecuteAsync(second, "INSERT INTO city VALUES ('Tokyo')");
        return await ScalarAsync(first, "SELECT group_concat(name, ',') FROM (SELECT name FROM city ORDER BY name)");
    }

    // EF Core's decimal comparison and ordering run its registered function and collation, and
    // its sum its registered aggregate.
    private static async Task<string> CatalogAsync()
    {
        await using var connection = new SqliteConnection("Data Source=:memory:");
        await connection.OpenAsync();
        await using var context = new Catalog(connection);
        await context.Database.EnsureCreatedAsync();
        context.Products.AddRange(
            new Product { Name = "kettle", Price = 24.5m },
            new Product { Name = "teapot", Price = 3.25m },
            new Product { Name = "cup", Price = 12m },
            new Product { Name = "saucer", Price = 4m });
        await context.SaveChangesAsync();
        var names = await context.Products.Where(p => p.Price > 3.5m).OrderBy(p => p.Price).Select(p => p.Name).ToListAsync();
        decimal total = await context.Products.SumAsync(p => p.Price);
        return string.Join(",", names) + ";" + total.ToString(CultureInfo.InvariantCulture);
    }

    private static async Task<string> FileAsync()
    {
        await using var connection = new SqliteConnection("Data Source=app.db");
        await connection.OpenAsync();
        return "opened";
    }

    // A second connection reads a table the first holds an uncommitted write on.
    private static async Task<string> LockedAsync()
    {
        const string locks = "Data Source=locks;Mode=Memory;Cache=Shared";
        await using var writer = new SqliteConnection(locks);
        await writer.OpenAsync();
        await ExecuteAsync(writer, "CREATE TABLE entry (value INTEGER)");
        await using var transaction = await writer.BeginTransactionAsync();
        await ExecuteAsync(writer, "INSERT INTO entry VALUES (1)");
        await using var reader = new SqliteConnection(locks);
        await reader.OpenAsync();
        return await ScalarAsync(reader, "SELECT count(*) FROM entry");
    }

    // The instance's first draw from SQLite's generator.
    private static async Task<string> RandomAsync()
    {
        await using var connection = new SqliteConnection("Data Source=:memory:");
        await connection.OpenAsync();
        return await ScalarAsync(connection, "SELECT random()");
    }

    private static async Task ExecuteAsync(SqliteConnection connection, string sql)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        await command.ExecuteNonQueryAsync();
    }

    private static async Task<string> ScalarAsync(SqliteConnection connection, string sql)
    {
        await using var command = connection.CreateCommand();
        command.CommandText = sql;
        return Convert.ToString(await command.ExecuteScalarAsync(), CultureInfo.InvariantCulture)!;
    }
}

/// <summary>A product with a decimal price, which EF Core stores as text.</summary>
public sealed class Product
{
    /// <summary>The key.</summary>
    public int Id { get; set; }

    /// <summary>The name.</summary>
    public string Name { get; set; } = "";

    /// <summary>The price.</summary>
    public decimal Price { get; set; }
}

/// <summary>An EF Core model over an open SQLite connection.</summary>
public sealed class Catalog(SqliteConnection connection) : DbContext
{
    /// <summary>The products.</summary>
    public DbSet<Product> Products => Set<Product>();

    /// <inheritdoc />
    protected override void OnConfiguring(DbContextOptionsBuilder options) => options.UseSqlite(connection);
}
