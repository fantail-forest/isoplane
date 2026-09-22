    private static int StaticValue = 0;

    private sealed class WatchedSample
    {
        public int Value = 7;
        public string Text = "old";
        public object Box = 42;
        public (object Item, int Count) Pair = (42, 0);
    }

    private static string Handle()
    {
        var sample = new WatchedSample();
        int[] numbers = [2, 4, 6];
        int[,] grid = new int[2, 2];
        object original = 42;
        sample.Box = original;
        object[] references = [original];
        sample.Pair = (original, 0);
        (object Item, int Count)[] pairs = [(original, 0)];
        int checkpoint = 0;
        GC.Collect();
        sample.Value = 7;
        var unrelated = new WatchedSample();
        unrelated.Value = 99;
        sample.Value = 11; sample.Value = 7;
        numbers[1] = 12; numbers[1] = 4;
        grid[1, 0] = 9; grid[1, 0] = 0;
        StaticValue = 3; StaticValue = 0;
        sample.Text = "new"; sample.Text = "old";
        sample.Box = 42; sample.Box = original;
        references[0] = 42; references[0] = original;
        sample.Pair = (42, 0); sample.Pair = (original, 0);
        pairs[0] = (42, 0); pairs[0] = (original, 0);
        sample.Value = 8; sample.Value = 9;
        sample.Value = 10; sample.Value = 11; sample.Value = 7;
        numbers[1] = 10; numbers[1] = 12; numbers[1] = 4;
        sample.Value = 8; sample.Value = 7;
        return $"{sample.Value}:{numbers[1]}:{grid[1, 0]}:{StaticValue}:{sample.Text}:{checkpoint}";
    }
