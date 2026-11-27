using Microsoft.AspNetCore.Mvc.ViewFeatures;

namespace Sample_WebApp.Pages;

/// <summary>
/// What a page handler did, for the page it redirects to to confirm. Add a value
/// here for another outcome, and its wording to <see cref="ConfirmationWording"/>:
/// an outcome with no wording is not shown.
/// </summary>
public enum Confirmation
{
    NoteAdded,
    NoteSaved,
    NoteDeleted
}

/// <summary>
/// What each confirmation says. Reword them here: the layout shows these, and
/// the tests tell confirmations apart by outcome, so neither needs a change.
/// </summary>
public static class ConfirmationWording
{
    /// <summary>The wording for a confirmation, or null when it has none.</summary>
    public static string? For(Confirmation confirmation) => confirmation switch
    {
        Confirmation.NoteAdded => "Note added.",
        Confirmation.NoteSaved => "Note saved.",
        Confirmation.NoteDeleted => "Note deleted.",
        _ => null
    };
}

/// <summary>
/// Carries a <see cref="Confirmation"/> across a redirect in TempData, which
/// keeps a value until it is read. TempData stores simple values such as
/// strings, so the confirmation travels by name.
/// </summary>
public static class ConfirmationTempData
{
    private const string Key = "Confirmation";

    /// <summary>Leaves a confirmation for the next page to show.</summary>
    public static void Confirm(this ITempDataDictionary tempData, Confirmation confirmation) =>
        tempData[Key] = confirmation.ToString();

    /// <summary>
    /// The confirmation the previous page left, or null when it left none.
    /// Reading it removes it, so a confirmation is shown once.
    /// </summary>
    public static Confirmation? TakeConfirmation(this ITempDataDictionary tempData) =>
        Enum.TryParse<Confirmation>(tempData[Key] as string, out var confirmation) ? confirmation : null;
}
