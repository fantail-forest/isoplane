using System.ComponentModel.DataAnnotations;

namespace Sample_WebApp.Notes;

/// <summary>
/// The entity. Its rules live in its methods, where unit tests reach them
/// without a database or the sandbox (tests/Sample-WebApp.Tests/NoteTests.cs).
/// </summary>
public sealed class Note
{
    public const int TitleLength = 120;
    public const int BodyLength = 4000;

    public int Id { get; set; }

    public required string Title { get; set; }

    public string Body { get; set; } = "";

    public DateTimeOffset CreatedAt { get; init; }

    public DateTimeOffset UpdatedAt { get; private set; }

    public static Note Create(string title, string? body, DateTimeOffset now) =>
        new() { Title = title.Trim(), Body = body?.Trim() ?? "", CreatedAt = now, UpdatedAt = now };

    /// <summary>Replaces the note's text at <paramref name="now"/>. Returns false, and changes nothing, when the text is the same.</summary>
    public bool Edit(string title, string? body, DateTimeOffset now)
    {
        (title, body) = (title.Trim(), body?.Trim() ?? "");
        if (title == Title && body == Body) return false;
        (Title, Body, UpdatedAt) = (title, body, now);
        return true;
    }
}

/// <summary>
/// What the forms post. The attributes are checked when a page binds it, and the
/// tag helpers in the pages read them for labels and input lengths.
/// </summary>
public sealed class NoteInput
{
    [Required, StringLength(Note.TitleLength)]
    public string Title { get; set; } = "";

    // An empty text box binds as null.
    [StringLength(Note.BodyLength)]
    public string? Body { get; set; }
}
