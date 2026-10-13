using Sample_WebApp.Notes;

namespace Sample_WebApp.Tests;

/// <summary>Ordinary unit tests: application logic runs on the test machine, outside the sandbox.</summary>
public sealed class NoteTests
{
    private static readonly DateTimeOffset Now = new(2026, 1, 2, 3, 4, 5, TimeSpan.Zero);

    [Fact]
    public void A_new_note_keeps_its_text_without_surrounding_space()
    {
        var note = Note.Create("  Buy oat milk ", " Two cartons.\n", Now);

        Assert.Equal(("Buy oat milk", "Two cartons."), (note.Title, note.Body));
        Assert.Equal((Now, Now), (note.CreatedAt, note.UpdatedAt));
    }

    [Fact]
    public void A_note_without_a_body_has_an_empty_one()
    {
        Assert.Equal("", Note.Create("Title only", null, Now).Body);
    }

    [Fact]
    public void Editing_a_note_records_when_it_changed()
    {
        var note = Note.Create("Draft", null, Now);

        Assert.True(note.Edit("Final", "Done.", Now.AddHours(1)));
        Assert.Equal(("Final", "Done."), (note.Title, note.Body));
        Assert.Equal((Now, Now.AddHours(1)), (note.CreatedAt, note.UpdatedAt));
    }

    [Fact]
    public void Saving_a_note_unchanged_leaves_it_as_it_was()
    {
        var note = Note.Create("Draft", "Text.", Now);

        Assert.False(note.Edit(" Draft ", "Text.", Now.AddHours(1)));
        Assert.Equal(Now, note.UpdatedAt);
    }
}
