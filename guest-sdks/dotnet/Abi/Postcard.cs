using System;
using System.Text;

namespace Wisp.Abi;

/// <summary>
/// A hand-rolled decoder/encoder for the subset of the postcard wire format
/// the guest actually needs.
///
/// This is deliberately NOT a port of postcard. The PoC needs six shapes —
/// varint, bool, string, byte-vector, option, enum discriminant — and porting
/// the rest would be a serializer project, not evidence for claim C10. In a
/// real SDK this file is generated from the same table that generates
/// <c>wisp-abi</c>'s Rust types, so the two sides cannot drift.
///
/// Format notes (postcard 1.x):
///   * all unsigned integers wider than a byte are LEB128 varints;
///   * <c>String</c> and <c>Vec&lt;u8&gt;</c> are varint length + bytes;
///   * <c>Option&lt;T&gt;</c> is 0x00 | 0x01 + payload;
///   * an enum is a varint discriminant followed by the variant's fields;
///   * a struct is just its fields in declaration order, with no framing.
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

    /// <summary>Skips a value we decode the shape of but do not need.</summary>
    internal void SkipBytes() => Bytes();

    /// <summary>Skips a <c>Vec&lt;(String, String)&gt;</c>.</summary>
    internal void SkipStringPairs()
    {
        ulong n = Varint();
        for (ulong i = 0; i < n; i++) { SkipBytes(); SkipBytes(); }
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

public sealed class PostcardException : Exception
{
    // §12.3: UseSystemResourceKeys=true means exception messages are resource
    // keys anyway, so carrying a message here would buy nothing.
}
