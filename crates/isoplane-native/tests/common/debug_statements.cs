    private static bool finished;

    private static string Handle()
    {
        int result = Protected(Compute());
        return finished ? result.ToString() : "missing cleanup";
    }

    private static int Protected(int answer)
    {
        try
        {
            int saved = answer;
            return saved;
        }
        finally { finished = true; }
    }

    private static int Compute()
    {
        int answer = 1;
        answer += 10;
        answer *= 2;
        return answer;
    }
