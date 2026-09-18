using System.Text;

namespace Isoplane.Testing;

internal sealed class Diagnostics(int limit)
{
    private readonly object gate = new();
    private readonly StringBuilder text = new();

    internal void Append(ReadOnlySpan<char> value)
    {
        lock (gate)
        {
            if (value.Length >= limit)
            {
                text.Clear();
                text.Append(value[^limit..]);
                return;
            }
            var excess = text.Length + value.Length - limit;
            if (excess > 0)
                text.Remove(0, excess);
            text.Append(value);
        }
    }

    internal async Task DrainAsync(TextReader reader, CancellationToken cancellation)
    {
        var buffer = new char[2048];
        int count;
        while ((count = await reader.ReadAsync(buffer.AsMemory(), cancellation)) != 0)
            Append(buffer.AsSpan(0, count));
    }

    public override string ToString()
    {
        lock (gate)
            return text.ToString();
    }
}
