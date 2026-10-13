using Microsoft.AspNetCore.Mvc;
using Microsoft.AspNetCore.Mvc.RazorPages;
using Microsoft.EntityFrameworkCore;
using Sample_WebApp.Data;
using Sample_WebApp.Notes;

namespace Sample_WebApp.Pages;

/// <summary>
/// The home page: every note, and a form that adds one. Handlers await their
/// database work, because a sandboxed application cannot block, and pass on the
/// request's cancellation token, which is cancelled when the visitor goes away.
/// </summary>
public sealed class IndexModel(AppDbContext db, TimeProvider clock) : PageModel
{
    /// <summary>The posted form. Binding it also validates it against NoteInput's attributes.</summary>
    [BindProperty]
    public NoteInput Input { get; set; } = new();

    public IReadOnlyList<Note> Notes { get; private set; } = [];

    public async Task OnGetAsync(CancellationToken cancellation) => Notes = await ListAsync(cancellation);

    public async Task<IActionResult> OnPostAsync(CancellationToken cancellation)
    {
        if (!ModelState.IsValid)
        {
            // Shows the form again, with what was typed and the messages beside it.
            Notes = await ListAsync(cancellation);
            return Page();
        }

        db.Notes.Add(Note.Create(Input.Title, Input.Body, clock.GetUtcNow()));
        await db.SaveChangesAsync(cancellation);
        // A message for the next page to show once; the layout renders it.
        TempData["Flash"] = "Note added.";
        // Redirecting after a post means reloading the page does not post again.
        return RedirectToPage();
    }

    private async Task<IReadOnlyList<Note>> ListAsync(CancellationToken cancellation) =>
        await db.Notes.AsNoTracking().OrderByDescending(note => note.UpdatedAt).ThenByDescending(note => note.Id).ToListAsync(cancellation);
}
