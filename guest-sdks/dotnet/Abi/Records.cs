using System;

namespace Isoplane.Abi;

/// <summary>
/// <c>isoplane-abi::dispatch::DispatchRecord</c>. Its encoded fields and their
/// decoder are generated from the Rust definition.
/// </summary>
public sealed partial class DispatchRecord
{
    // Not part of the encoded record: the runtime reads the whole body from
    // the host before it routes a request to a handler that takes it buffered.
    internal byte[] Body = Array.Empty<byte>();
}
