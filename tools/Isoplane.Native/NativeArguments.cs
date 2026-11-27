using System.Runtime.InteropServices;
using System.Text;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>
/// Native copies of the managed values one native call reads, released together when it is disposed. Keep it
/// alive until the call returns.
/// </summary>
internal sealed unsafe class NativeArguments : IDisposable
{
    private readonly List<nint> allocations = [];

    /// <summary>A copy of <paramref name="text"/> as UTF-8. Null and empty text are both empty.</summary>
    internal NativeText Text(string? text)
    {
        if (string.IsNullOrEmpty(text)) return default;
        var length = Encoding.UTF8.GetByteCount(text);
        var data = Allocate<byte>(length);
        Encoding.UTF8.GetBytes(text, new Span<byte>(data, length));
        return new NativeText { data = data, length = (nuint)length };
    }

    /// <summary>A copy of each of <paramref name="texts"/>, as an array of <paramref name="texts"/>' length.</summary>
    internal NativeText* Texts(IReadOnlyList<string> texts)
    {
        var copies = Allocate<NativeText>(texts.Count);
        for (var index = 0; index < texts.Count; index++) copies[index] = Text(texts[index]);
        return copies;
    }

    /// <summary>A copy of each of <paramref name="settings"/>, as an array of <paramref name="settings"/>' length.</summary>
    internal NativeSetting* Settings(IReadOnlyCollection<KeyValuePair<string, string>> settings)
    {
        var copies = Allocate<NativeSetting>(settings.Count);
        var index = 0;
        foreach (var (name, value) in settings) copies[index++] = new NativeSetting { name = Text(name), value = Text(value) };
        return copies;
    }

    private T* Allocate<T>(int count) where T : unmanaged
    {
        var data = (T*)NativeMemory.Alloc((nuint)count, (nuint)sizeof(T));
        allocations.Add((nint)data);
        return data;
    }

    public void Dispose()
    {
        foreach (var data in allocations) NativeMemory.Free((void*)data);
        allocations.Clear();
    }
}
