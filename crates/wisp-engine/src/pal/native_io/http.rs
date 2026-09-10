//! Canonical scalar and memory conversion for the `wasi:http` operations:
//! decoding a call's arguments into a typed request and writing the host's
//! answer where the runtime's generated bindings read it. The return areas
//! are the bindings' own pinned arrays, so every write is exactly the size
//! the binding declares.

use anyhow::{bail, ensure, Context, Result};
use wisp_abi::imports::MAX_OUT_BUFFER_BYTES;
use wisp_abi::native_io::canonical::HttpAction;
use wisp_abi::native_io::http::{Field, Method, Request, Response, Scheme};
use wisp_abi::native_io::{Error, ResourceHandle};

use crate::pal::filesystem::Memory;

/// Decode the arguments of `action` into the operation they name.
pub fn request(memory: &mut impl Memory, action: HttpAction, arguments: &[u64]) -> Result<Request> {
    let scalar = |index: usize| {
        arguments
            .get(index)
            .copied()
            .context("wasi:http argument missing")
    };
    let handle = |index| scalar(index).and_then(|value| Ok(ResourceHandle(u32::try_from(value)?)));
    let optional = |index: usize| -> Result<bool> {
        match scalar(index)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => bail!("invalid canonical option discriminant"),
        }
    };
    Ok(match action {
        HttpAction::FieldsNew => Request::FieldsNew,
        HttpAction::FieldsFromList => {
            Request::FieldsFromList(fields(memory, scalar(0)?, scalar(1)?)?)
        }
        HttpAction::FieldsGet => Request::FieldsGet {
            fields: handle(0)?,
            name: text(memory, scalar(1)?, scalar(2)?)?,
        },
        HttpAction::FieldsHas => Request::FieldsHas {
            fields: handle(0)?,
            name: text(memory, scalar(1)?, scalar(2)?)?,
        },
        HttpAction::FieldsSet => Request::FieldsSet {
            fields: handle(0)?,
            name: text(memory, scalar(1)?, scalar(2)?)?,
            values: values(memory, scalar(3)?, scalar(4)?)?,
        },
        HttpAction::FieldsDelete => Request::FieldsDelete {
            fields: handle(0)?,
            name: text(memory, scalar(1)?, scalar(2)?)?,
        },
        HttpAction::FieldsAppend => Request::FieldsAppend {
            fields: handle(0)?,
            name: text(memory, scalar(1)?, scalar(2)?)?,
            value: bytes(memory, scalar(3)?, scalar(4)?)?,
        },
        HttpAction::FieldsEntries => Request::FieldsEntries(handle(0)?),
        HttpAction::FieldsClone => Request::FieldsClone(handle(0)?),
        HttpAction::RequestNew => Request::RequestNew {
            headers: handle(0)?,
        },
        HttpAction::RequestBody => Request::RequestBody(handle(0)?),
        HttpAction::RequestSetMethod => Request::RequestSetMethod {
            request: handle(0)?,
            method: match scalar(1)? {
                0 => Method::Get,
                1 => Method::Head,
                2 => Method::Post,
                3 => Method::Put,
                4 => Method::Delete,
                5 => Method::Connect,
                6 => Method::Options,
                7 => Method::Trace,
                8 => Method::Patch,
                9 => Method::Other(text(memory, scalar(2)?, scalar(3)?)?),
                _ => bail!("invalid canonical HTTP method discriminant"),
            },
        },
        HttpAction::RequestSetPath => Request::RequestSetPath {
            request: handle(0)?,
            path: if optional(1)? {
                Some(text(memory, scalar(2)?, scalar(3)?)?)
            } else {
                None
            },
        },
        HttpAction::RequestSetScheme => Request::RequestSetScheme {
            request: handle(0)?,
            scheme: if optional(1)? {
                Some(match scalar(2)? {
                    0 => Scheme::Http,
                    1 => Scheme::Https,
                    2 => Scheme::Other(text(memory, scalar(3)?, scalar(4)?)?),
                    _ => bail!("invalid canonical HTTP scheme discriminant"),
                })
            } else {
                None
            },
        },
        HttpAction::RequestSetAuthority => Request::RequestSetAuthority {
            request: handle(0)?,
            authority: if optional(1)? {
                Some(text(memory, scalar(2)?, scalar(3)?)?)
            } else {
                None
            },
        },
        HttpAction::RequestHeaders => Request::RequestHeaders(handle(0)?),
        HttpAction::OutgoingBodyWrite => Request::OutgoingBodyWrite(handle(0)?),
        HttpAction::OutgoingBodyFinish => Request::OutgoingBodyFinish {
            body: handle(0)?,
            trailers: if optional(1)? { Some(handle(2)?) } else { None },
        },
        HttpAction::Handle => Request::Handle {
            request: handle(0)?,
            options: if optional(1)? { Some(handle(2)?) } else { None },
        },
        HttpAction::FutureSubscribe => Request::FutureSubscribe(handle(0)?),
        HttpAction::FutureGet => Request::FutureGet(handle(0)?),
        HttpAction::ResponseStatus => Request::ResponseStatus(handle(0)?),
        HttpAction::ResponseHeaders => Request::ResponseHeaders(handle(0)?),
        HttpAction::ResponseConsume => Request::ResponseConsume(handle(0)?),
        HttpAction::IncomingBodyStream => Request::IncomingBodyStream(handle(0)?),
        HttpAction::IncomingBodyFinish => Request::IncomingBodyFinish(handle(0)?),
    })
}

/// Write the host's answer to `action` where its binding reads it, and
/// return the call's scalar result.
pub fn respond(
    memory: &mut impl Memory,
    action: HttpAction,
    arguments: &[u64],
    result: Result<Response, Error>,
) -> Result<u64> {
    let response = result?;
    let return_pointer = || {
        arguments
            .last()
            .copied()
            .context("wasi:http result pointer missing")
            .and_then(|pointer| Ok(u32::try_from(pointer)?))
    };
    match (action, response) {
        // A bare handle or number is the call's result.
        (
            HttpAction::FieldsNew
            | HttpAction::FieldsClone
            | HttpAction::RequestNew
            | HttpAction::RequestHeaders
            | HttpAction::ResponseHeaders
            | HttpAction::FutureSubscribe
            | HttpAction::IncomingBodyFinish,
            Response::Handle(handle),
        ) => Ok(u64::from(handle.0)),
        (HttpAction::FieldsHas, Response::Bool(value)) => Ok(u64::from(value)),
        (HttpAction::ResponseStatus, Response::Status(status)) => Ok(u64::from(status)),
        // result<_, _> as the call's result: 0 ok, 1 err.
        (
            HttpAction::RequestSetMethod
            | HttpAction::RequestSetPath
            | HttpAction::RequestSetScheme
            | HttpAction::RequestSetAuthority,
            Response::Done,
        ) => Ok(0),
        (
            HttpAction::RequestSetMethod
            | HttpAction::RequestSetPath
            | HttpAction::RequestSetScheme
            | HttpAction::RequestSetAuthority,
            Response::Refused,
        ) => Ok(1),
        // result<own<T>, header-error>: disc at 0, handle or error at 4.
        (HttpAction::FieldsFromList, Response::Handle(handle)) => {
            let mut record = [0; 8];
            record[4..].copy_from_slice(&handle.0.to_le_bytes());
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (HttpAction::FieldsFromList, Response::HeaderError(error)) => {
            memory.write(return_pointer()?, &[1, 0, 0, 0, error as u8])?;
            Ok(0)
        }
        // result<_, header-error> in a two-byte area.
        (
            HttpAction::FieldsSet | HttpAction::FieldsDelete | HttpAction::FieldsAppend,
            Response::Done,
        ) => {
            memory.write(return_pointer()?, &[0, 0])?;
            Ok(0)
        }
        (
            HttpAction::FieldsSet | HttpAction::FieldsDelete | HttpAction::FieldsAppend,
            Response::HeaderError(error),
        ) => {
            memory.write(return_pointer()?, &[1, error as u8])?;
            Ok(0)
        }
        (HttpAction::FieldsGet, Response::Values(values)) => {
            let (pointer, count) = write_values(memory, &values)?;
            write_list(memory, return_pointer()?, pointer, count)?;
            Ok(0)
        }
        (HttpAction::FieldsEntries, Response::Entries(entries)) => {
            let (pointer, count) = write_entries(memory, &entries)?;
            write_list(memory, return_pointer()?, pointer, count)?;
            Ok(0)
        }
        // result<own<T>, _>: disc at 0, handle at 4.
        (
            HttpAction::RequestBody
            | HttpAction::OutgoingBodyWrite
            | HttpAction::ResponseConsume
            | HttpAction::IncomingBodyStream,
            Response::Handle(handle),
        ) => {
            let mut record = [0; 8];
            record[4..].copy_from_slice(&handle.0.to_le_bytes());
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (
            HttpAction::RequestBody
            | HttpAction::OutgoingBodyWrite
            | HttpAction::ResponseConsume
            | HttpAction::IncomingBodyStream,
            Response::Refused,
        ) => {
            memory.write(return_pointer()?, &[1])?;
            Ok(0)
        }
        // result<_, error-code>: disc at 0, code at 8, its empty payload at 16.
        (HttpAction::OutgoingBodyFinish, Response::Done) => {
            memory.write(return_pointer()?, &[0])?;
            Ok(0)
        }
        (HttpAction::OutgoingBodyFinish, Response::Failed(code)) => {
            let mut record = [0; 24];
            record[0] = 1;
            record[8] = code as u8;
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        // result<own<future-incoming-response>, error-code>: handle at 8.
        (HttpAction::Handle, Response::Handle(handle)) => {
            let mut record = [0; 12];
            record[8..].copy_from_slice(&handle.0.to_le_bytes());
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (HttpAction::Handle, Response::Failed(code)) => {
            let mut record = [0; 24];
            record[0] = 1;
            record[8] = code as u8;
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        // option<result<result<own<incoming-response>, error-code>, _>>.
        (HttpAction::FutureGet, Response::Pending) => {
            memory.write(return_pointer()?, &[0])?;
            Ok(0)
        }
        (HttpAction::FutureGet, Response::Taken) => {
            let mut record = [0; 9];
            record[0] = 1;
            record[8] = 1;
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (HttpAction::FutureGet, Response::Handle(handle)) => {
            let mut record = [0; 28];
            record[0] = 1;
            record[24..].copy_from_slice(&handle.0.to_le_bytes());
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (HttpAction::FutureGet, Response::Failed(code)) => {
            let mut record = [0; 40];
            record[0] = 1;
            record[16] = 1;
            record[24] = code as u8;
            memory.write(return_pointer()?, &record)?;
            Ok(0)
        }
        (action, response) => bail!("wasi:http {action:?} answered with an unrelated {response:?}"),
    }
}

fn bytes(memory: &mut impl Memory, pointer: u64, length: u64) -> Result<Vec<u8>> {
    let length = usize::try_from(length)?;
    ensure!(
        length <= MAX_OUT_BUFFER_BYTES,
        "wasi:http value exceeds the host buffer limit"
    );
    let mut bytes = vec![0; length];
    memory.read(u32::try_from(pointer)?, &mut bytes)?;
    Ok(bytes)
}

fn text(memory: &mut impl Memory, pointer: u64, length: u64) -> Result<String> {
    Ok(String::from_utf8(bytes(memory, pointer, length)?)?)
}

/// A `list<list<u8>>`: eight-byte elements of pointer and length.
fn values(memory: &mut impl Memory, pointer: u64, count: u64) -> Result<Vec<Vec<u8>>> {
    let count = usize::try_from(count)?;
    ensure!(
        count <= MAX_OUT_BUFFER_BYTES / 8,
        "wasi:http value list exceeds the host buffer limit"
    );
    let mut table = vec![0; count * 8];
    memory.read(u32::try_from(pointer)?, &mut table)?;
    let mut values = Vec::with_capacity(count);
    let mut total = 0;
    for element in table.as_chunks::<8>().0 {
        let value = bytes(memory, word(element, 0), word(element, 4))?;
        total += value.len();
        ensure!(
            total <= MAX_OUT_BUFFER_BYTES,
            "wasi:http values exceed the host buffer limit"
        );
        values.push(value);
    }
    Ok(values)
}

/// A `list<tuple<string, list<u8>>>`: sixteen-byte elements of name
/// pointer and length, then value pointer and length.
fn fields(memory: &mut impl Memory, pointer: u64, count: u64) -> Result<Vec<Field>> {
    let count = usize::try_from(count)?;
    ensure!(
        count <= MAX_OUT_BUFFER_BYTES / 16,
        "wasi:http field list exceeds the host buffer limit"
    );
    let mut table = vec![0; count * 16];
    memory.read(u32::try_from(pointer)?, &mut table)?;
    let mut fields = Vec::with_capacity(count);
    let mut total = 0;
    for element in table.as_chunks::<16>().0 {
        let name = text(memory, word(element, 0), word(element, 4))?;
        let value = bytes(memory, word(element, 8), word(element, 12))?;
        total += name.len() + value.len();
        ensure!(
            total <= MAX_OUT_BUFFER_BYTES,
            "wasi:http fields exceed the host buffer limit"
        );
        fields.push((name, value));
    }
    Ok(fields)
}

fn word(element: &[u8], offset: usize) -> u64 {
    u64::from(u32::from_le_bytes([
        element[offset],
        element[offset + 1],
        element[offset + 2],
        element[offset + 3],
    ]))
}

/// Place `values` in guest memory as one allocation: the element table,
/// then every value, and return the table's address and element count.
fn write_values(memory: &mut impl Memory, values: &[Vec<u8>]) -> Result<(u32, u32)> {
    let table_bytes = values.len() * 8;
    let total = table_bytes + values.iter().map(Vec::len).sum::<usize>();
    let base = allocate(memory, total)?;
    let mut table = vec![0; table_bytes];
    let mut offset = table_bytes;
    for (index, value) in values.iter().enumerate() {
        let address = base + u32::try_from(offset)?;
        memory.write(address, value)?;
        table[index * 8..index * 8 + 4].copy_from_slice(&address.to_le_bytes());
        table[index * 8 + 4..index * 8 + 8]
            .copy_from_slice(&u32::try_from(value.len())?.to_le_bytes());
        offset += value.len();
    }
    memory.write(base, &table)?;
    Ok((base, u32::try_from(values.len())?))
}

/// Place `entries` in guest memory as one allocation, sixteen-byte
/// elements first.
fn write_entries(memory: &mut impl Memory, entries: &[Field]) -> Result<(u32, u32)> {
    let table_bytes = entries.len() * 16;
    let total = table_bytes
        + entries
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>();
    let base = allocate(memory, total)?;
    let mut table = vec![0; table_bytes];
    let mut offset = table_bytes;
    for (index, (name, value)) in entries.iter().enumerate() {
        let element = &mut table[index * 16..index * 16 + 16];
        for (slot, bytes) in [(0, name.as_bytes()), (8, value.as_slice())] {
            let address = base + u32::try_from(offset)?;
            memory.write(address, bytes)?;
            element[slot..slot + 4].copy_from_slice(&address.to_le_bytes());
            element[slot + 4..slot + 8].copy_from_slice(&u32::try_from(bytes.len())?.to_le_bytes());
            offset += bytes.len();
        }
    }
    memory.write(base, &table)?;
    Ok((base, u32::try_from(entries.len())?))
}

fn allocate(memory: &mut impl Memory, total: usize) -> Result<u32> {
    ensure!(
        total <= MAX_OUT_BUFFER_BYTES,
        "wasi:http result exceeds the host buffer limit"
    );
    if total == 0 {
        return Ok(0);
    }
    let base = memory.allocate(u32::try_from(total)?)?;
    ensure!(base != 0, "wasi:http result allocation failed");
    Ok(base)
}

fn write_list(
    memory: &mut impl Memory,
    return_pointer: u32,
    pointer: u32,
    count: u32,
) -> Result<()> {
    let mut record = [0; 8];
    record[..4].copy_from_slice(&pointer.to_le_bytes());
    record[4..].copy_from_slice(&count.to_le_bytes());
    memory.write(return_pointer, &record)
}
