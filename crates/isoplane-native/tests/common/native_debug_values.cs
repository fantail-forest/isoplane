    private static string Handle() => Compute().ToString();

    private static int Compute()
    {
        var sample = new DebugSample { Value = 14, Name = "hello 🌲" };
        int[] numbers = [3, 7];
        DebugSample? absent = null;
        int answer = sample.Value + numbers[1];
        answer *= 2;
        return answer + sample.Value - 14 + numbers[1] - 7 + (absent is null ? 0 : 1);
    }

    private sealed class DebugSample
    {
        public int Value;
        public string Name = "";
    }
