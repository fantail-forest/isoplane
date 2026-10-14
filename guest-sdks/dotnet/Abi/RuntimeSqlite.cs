using System.Runtime.InteropServices;

namespace Isoplane.Abi;

/// <summary>
/// The SQLite the shared runtime links as module <c>e_sqlite3</c>, beside the
/// functions of SQLite's own API that applications reach through SQLitePCLRaw.
/// </summary>
internal static class RuntimeSqlite
{
    /// <summary>
    /// Makes SQLite's generator, behind <c>random()</c> and <c>randomblob()</c>,
    /// reseed on its next draw if SQLite was initialized before the template was
    /// captured. Initializes nothing otherwise.
    /// </summary>
    [DllImport("e_sqlite3", EntryPoint = "isoplane_sqlite_reseed")]
    internal static extern void Reseed();
}
