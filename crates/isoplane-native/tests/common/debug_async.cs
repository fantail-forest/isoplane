    private static async Task<string> Handle()
    {
        int seed = 21;
        int result = await Outer(seed);
        return (result + seed - 21).ToString();
    }

    private static async Task<int> Outer(int input)
    {
        int outer = input + 1;
        int result = await Inner(outer).ConfigureAwait(false);
        return result + outer;
    }

    private static async Task<int> Inner(int input)
    {
        int answer = input;
        var sample = new DebugSample { Value = 7, Name = "hello" };
        int[] numbers = [3, 4];
        Task delay = Task.Delay(100);
        if (delay.IsCompleted) throw new InvalidOperationException("the host timer must be pending before await");
        await delay;
        int total = answer + sample.Value + numbers[1];
        return total;
    }

    private sealed class DebugSample
    {
        public int Value;
        public string Name = "";
    }
