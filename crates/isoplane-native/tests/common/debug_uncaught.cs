    private static async Task<string> Handle()
    {
        int caught = 0;
        try { throw new InvalidOperationException("debug-caught"); }
        catch (InvalidOperationException) { caught++; }
        FailWithoutTask();
        await new TaskCompletionSource<string>().Task;
        return caught.ToString();
    }

    private static async void FailWithoutTask()
    {
        await Task.Yield();
        throw new InvalidOperationException("debug-unhandled");
    }
