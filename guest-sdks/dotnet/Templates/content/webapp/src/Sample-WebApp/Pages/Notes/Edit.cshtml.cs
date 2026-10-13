using Microsoft.AspNetCore.Mvc;
using Microsoft.AspNetCore.Mvc.RazorPages;
using Microsoft.EntityFrameworkCore;
using Sample_WebApp.Data;
using Sample_WebApp.Notes;

namespace Sample_WebApp.Pages.Notes;

public sealed class EditModel(AppDbContext db, TimeProvider clock) : PageModel
{
    [BindProperty]
    public NoteInput Input { get; set; } = new();

    public async Task<IActionResult> OnGetAsync(int id, CancellationToken cancellation)
    {
        var note = await db.Notes.FindAsync([id], cancellation);
        if (note is null) return NotFound();
        Input = new NoteInput { Title = note.Title, Body = note.Body };
        return Page();
    }

    public async Task<IActionResult> OnPostAsync(int id, CancellationToken cancellation)
    {
        if (!ModelState.IsValid) return Page();
        var note = await db.Notes.FindAsync([id], cancellation);
        if (note is null) return NotFound();
        if (note.Edit(Input.Title, Input.Body, clock.GetUtcNow()))
        {
            await db.SaveChangesAsync(cancellation);
            TempData["Flash"] = "Note saved.";
        }
        return RedirectToPage("/Index");
    }

    public async Task<IActionResult> OnPostDeleteAsync(int id, CancellationToken cancellation)
    {
        await db.Notes.Where(note => note.Id == id).ExecuteDeleteAsync(cancellation);
        TempData["Flash"] = "Note deleted.";
        return RedirectToPage("/Index");
    }
}
