using System;
using System.Collections.Generic;
using System.Text;

namespace Wisp.Abi;

/// <summary>
/// The subset of the postcard wire format the SDK reads and writes: varints,
/// bools, strings, byte vectors, options and enum discriminants. Postcard
/// writes a struct as its fields in declaration order with no framing, an
/// option as a 0 or a 1 and the payload, and an enum as a varint
/// discriminant and the variant's fields.
/// </summary>
internal ref struct PostcardReader
{
    private readonly ReadOnlySpan<byte> _buf;
    private int _pos;

    internal PostcardReader(ReadOnlySpan<byte> buf)
    {
        _buf = buf;
        _pos = 0;
    }

    internal bool AtEnd => _pos >= _buf.Length;

    internal byte U8()
    {
        if (_pos >= _buf.Length) throw new PostcardException();
        return _buf[_pos++];
    }

    internal ulong Varint()
    {
        ulong value = 0;
        int shift = 0;
        while (true)
        {
            byte b = U8();
            value |= (ulong)(b & 0x7F) << shift;
            if ((b & 0x80) == 0) return value;
            shift += 7;
            if (shift > 63) throw new PostcardException();
        }
    }

    internal uint U32() => (uint)Varint();
    internal ulong U64() => Varint();
    internal ushort U16() => (ushort)Varint();

    internal bool Bool() => U8() != 0;

    internal ReadOnlySpan<byte> Bytes()
    {
        int len = checked((int)Varint());
        if (_pos + len > _buf.Length) throw new PostcardException();
        var s = _buf.Slice(_pos, len);
        _pos += len;
        return s;
    }

    internal string Str() => Encoding.UTF8.GetString(Bytes());

    internal void SkipBytes() => Bytes();

    internal KeyValuePair<string, string>[] StringPairs()
    {
        int count = checked((int)Varint());
        var pairs = new KeyValuePair<string, string>[count];
        for (int i = 0; i < count; i++)
            pairs[i] = new KeyValuePair<string, string>(Str(), Str());
        return pairs;
    }

}

internal sealed class PostcardWriter
{
    private byte[] _buf;
    private int _len;

    internal PostcardWriter(int capacity = 256)
    {
        _buf = new byte[capacity];
        _len = 0;
    }

    private void Ensure(int extra)
    {
        if (_len + extra <= _buf.Length) return;
        int cap = _buf.Length;
        while (cap < _len + extra) cap *= 2;
        Array.Resize(ref _buf, cap);
    }

    internal void U8(byte b)
    {
        Ensure(1);
        _buf[_len++] = b;
    }

    internal void Varint(ulong v)
    {
        Ensure(10);
        while (v >= 0x80)
        {
            _buf[_len++] = (byte)(v | 0x80);
            v >>= 7;
        }
        _buf[_len++] = (byte)v;
    }

    internal void Bool(bool b) => U8(b ? (byte)1 : (byte)0);

    internal void Bytes(ReadOnlySpan<byte> b)
    {
        Varint((ulong)b.Length);
        Ensure(b.Length);
        b.CopyTo(_buf.AsSpan(_len));
        _len += b.Length;
    }

    internal void Str(string s) => Bytes(Encoding.UTF8.GetBytes(s));

    internal ReadOnlySpan<byte> Span => _buf.AsSpan(0, _len);
}

/// <summary>
/// A malformed dispatch or result encoding, or a trigger this guest cannot
/// receive. The entrypoint converts it to <see cref="TrapCode.Protocol"/>.
/// </summary>
public sealed class PostcardException : Exception
{
}
