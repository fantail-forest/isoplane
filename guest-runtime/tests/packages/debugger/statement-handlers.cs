using System;
using Microsoft.AspNetCore.Builder;

namespace Debugger.StatementHandlers;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/statement-handlers", Handle);
    private static int cleanup;
    private static int skipped;

    private static string Handle()
    {
        string message = "missing exception";
        try { Run(true); }
        catch (InvalidOperationException error) { message = error.Message; }
        Run(false);
        return $"{message}:{cleanup}:{skipped}";
    }

    private static void Run(bool fail)
    {
        try
        {
            if (fail) throw new InvalidOperationException("original");
        }
        catch (InvalidOperationException error)
        {
            string original = error.Message;
            skipped += 100;
            throw;
        }
        finally
        {
            cleanup += 1;
            skipped += 1000;
            cleanup += 10;
        }
    }
}
