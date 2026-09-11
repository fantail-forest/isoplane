//! The PAL filesystem: a read-only root holding nothing but the host-declared
//! [`PlatformFiles`], whose bytes a guest reaches only through an immediate
//! `descriptor.read` copy. Descriptor records live in guest memory, so
//! templates and deltas retain their complete state and the host holds no
//! resource on a guest's behalf.

use anyhow::{bail, ensure, Result};
use wisp_abi::filesystem::{Definition, Operation, IMPORTS, MAX_ARGUMENTS};
use wisp_abi::imports::MAX_OUT_BUFFER_BYTES;

use super::platform_files::{LookupError, Node, NodeKind, PlatformFile, PlatformFiles};

const DESCRIPTOR: u32 = 0x77534601;
const DIRECTORY_STREAM: u32 = 0x77534602;
/// A record is three little-endian words: the kind magic, the node word and,
/// for a directory stream, the cursor.
pub const RECORD_BYTES: u32 = 12;
const BAD_DESCRIPTOR: u8 = 3;
const INVALID: u8 = 12;
const IS_DIRECTORY: u8 = 14;
const NO_ENTRY: u8 = 20;
const NOT_DIRECTORY: u8 = 24;
const NOT_PERMITTED: u8 = 31;
const READ_ONLY: u8 = 33;
const DIRECTORY: u8 = 3;
const REGULAR_FILE: u8 = 6;
const OPEN_FLAG_CREATE: u32 = 1;
const OPEN_FLAG_DIRECTORY: u32 = 2;
const OPEN_FLAG_TRUNCATE: u32 = 8;
const DESCRIPTOR_FLAG_READ: u8 = 1;
const DESCRIPTOR_FLAG_WRITE: u32 = 2;
const DESCRIPTOR_FLAG_MUTATE_DIRECTORY: u32 = 32;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Guest memory access for both the execution engine and the sealed initializer.
/// Allocation and deallocation invoke the guest's allocator, never a host resource table.
pub trait Memory {
    /// Copy bytes from guest memory, rejecting out-of-bounds addresses.
    fn read(&mut self, pointer: u32, bytes: &mut [u8]) -> Result<()>;
    /// Copy bytes into guest memory, rejecting out-of-bounds addresses.
    fn write(&mut self, pointer: u32, bytes: &[u8]) -> Result<()>;
    /// Allocate guest memory aligned to at least four bytes.
    fn allocate(&mut self, size_bytes: u32) -> Result<u32>;
    /// Release an allocation of `size_bytes` through the guest allocator.
    fn free(&mut self, pointer: u32, size_bytes: u32) -> Result<()>;
}

/// Execute an SDK wrapper's operation and canonical scalar arguments.
/// Unknown operations, malformed packets and invalid memory trap.
pub fn packet(
    files: &PlatformFiles,
    memory: &mut impl Memory,
    operation: u32,
    pointer: u32,
    count: u32,
) -> Result<()> {
    let definition = IMPORTS
        .iter()
        .find(|definition| definition.operation as u32 == operation)
        .ok_or_else(|| anyhow::anyhow!("unknown filesystem operation {operation}"))?;
    let signature = definition
        .pal
        .sig
        .ok_or_else(|| anyhow::anyhow!("filesystem operation has no signature"))?;
    ensure!(
        count as usize == signature.params.len(),
        "filesystem operation {} expected {} arguments, received {count}",
        definition.pal.func,
        signature.params.len()
    );
    let mut arguments = [0u64; MAX_ARGUMENTS];
    for (index, argument) in arguments.iter_mut().take(count as usize).enumerate() {
        let address = pointer
            .checked_add((index * 8) as u32)
            .ok_or_else(|| anyhow::anyhow!("filesystem argument address overflow"))?;
        let mut bytes = [0; 8];
        memory.read(address, &mut bytes)?;
        *argument = u64::from_le_bytes(bytes);
    }
    call(files, memory, definition, &arguments[..count as usize])
}

struct Record {
    node: Node,
    cursor: u32,
}

fn record(memory: &mut impl Memory, kind: u32, node: Node) -> Result<u32> {
    let pointer = memory.allocate(RECORD_BYTES)?;
    ensure!(
        pointer != 0 && pointer < (1 << 31),
        "filesystem descriptor allocation failed"
    );
    let words: Vec<u8> = [kind, node.word(), 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    memory.write(pointer, &words)?;
    Ok(pointer)
}

/// Decodes a record of `kind` whose index is valid in the current `files` set.
/// Records contain no set identity; indices are interpreted against this set.
fn read_record(
    files: &PlatformFiles,
    memory: &mut impl Memory,
    pointer: u32,
    kind: u32,
) -> Option<Record> {
    if pointer == 0 || pointer >= (1 << 31) {
        return None;
    }
    let mut bytes = [0; RECORD_BYTES as usize];
    if memory.read(pointer, &mut bytes).is_err() {
        return None;
    }
    let word = |index: usize| {
        u32::from_le_bytes([
            bytes[index],
            bytes[index + 1],
            bytes[index + 2],
            bytes[index + 3],
        ])
    };
    if word(0) != kind {
        return None;
    }
    let node = files.decode(word(4))?;
    if kind == DIRECTORY_STREAM && node.kind() == NodeKind::File {
        return None;
    }
    Some(Record {
        node,
        cursor: word(8),
    })
}

fn error(memory: &mut impl Memory, pointer: u32, offset: usize, code: u8) -> Result<()> {
    let mut bytes = [0; 9];
    bytes[0] = 1;
    bytes[offset] = code;
    memory.write(pointer, &bytes[..=offset])
}

/// Resolve a guest path argument from `from`, as an error code on failure.
fn lookup(
    files: &PlatformFiles,
    memory: &mut impl Memory,
    from: Node,
    pointer: u32,
    length: u32,
) -> Result<std::result::Result<Node, u8>> {
    ensure!(
        length as usize <= MAX_OUT_BUFFER_BYTES,
        "filesystem path exceeds the guest argument buffer limit"
    );
    let mut bytes = vec![0; length as usize];
    memory.read(pointer, &mut bytes)?;
    let Ok(path) = std::str::from_utf8(&bytes) else {
        return Ok(Err(INVALID));
    };
    if path.starts_with('/') {
        return Ok(Err(NOT_PERMITTED));
    }
    if path.is_empty() || path.contains('\0') {
        return Ok(Err(NO_ENTRY));
    }
    Ok(files.lookup(from, path).map_err(|error| match error {
        LookupError::NoEntry => NO_ENTRY,
        LookupError::NotDirectory => NOT_DIRECTORY,
        LookupError::Escape => NOT_PERMITTED,
    }))
}

/// Copy `bytes` into a fresh guest allocation and return the canonical
/// `(pointer, length)` pair; the empty list is `(0, 0)`.
fn allocate_list(memory: &mut impl Memory, bytes: &[u8]) -> Result<[u32; 2]> {
    if bytes.is_empty() {
        return Ok([0, 0]);
    }
    let length = u32::try_from(bytes.len())?;
    let pointer = memory.allocate(length)?;
    ensure!(
        pointer != 0 && pointer < (1 << 31),
        "filesystem list allocation failed"
    );
    memory.write(pointer, bytes)?;
    Ok([pointer, length])
}

fn write_words(out: &mut [u8], words: &[u32]) {
    for (slot, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *slot = word.to_le_bytes();
    }
}

/// `descriptor-stat` as the canonical ABI lays it out: type at 0, link
/// count at 8, size at 16, then three `option<datetime>` of 24 bytes each
/// (flag, seconds at +8, nanoseconds at +16) from 24. Every timestamp is
/// the node's modification time, which for a directory is the newest file
/// beneath it, so a consumer polling a directory sees a file rotate.
fn stat(files: &PlatformFiles, node: Node, out: &mut [u8]) {
    out[0] = descriptor_type(node);
    out[8..16].copy_from_slice(&1u64.to_le_bytes());
    if let Some(file) = files.file(node) {
        out[16..24].copy_from_slice(&(file.bytes.len() as u64).to_le_bytes());
    }
    if let Some(modified_ns) = files.modified_ns(node) {
        let seconds = modified_ns / NANOS_PER_SECOND;
        let nanoseconds = (modified_ns % NANOS_PER_SECOND) as u32;
        for timestamp in out[24..96].as_chunks_mut::<24>().0 {
            timestamp[0] = 1;
            timestamp[8..16].copy_from_slice(&seconds.to_le_bytes());
            timestamp[16..20].copy_from_slice(&nanoseconds.to_le_bytes());
        }
    }
}

/// `metadata-hash-value`: the root keeps its fixed value; every other node
/// hashes its path, so no two nodes share an identity.
fn metadata_hash(files: &PlatformFiles, node: Node, out: &mut [u8]) {
    match node.kind() {
        NodeKind::Root => out[..8].copy_from_slice(&u64::from(DESCRIPTOR).to_le_bytes()),
        NodeKind::Directory | NodeKind::File => {
            out[..16].copy_from_slice(&blake3::hash(files.path(node).as_bytes()).as_bytes()[..16]);
        }
    }
}

fn descriptor_type(node: Node) -> u8 {
    match node.kind() {
        NodeKind::Root | NodeKind::Directory => DIRECTORY,
        NodeKind::File => REGULAR_FILE,
    }
}

/// `read(length, offset)`: the bytes from `offset`, capped at the ABI's
/// out-buffer limit, and whether they reach the end of the file.
fn read_file(
    memory: &mut impl Memory,
    file: &PlatformFile,
    length: u64,
    offset: u64,
    out: &mut [u8],
) -> Result<()> {
    let total = file.bytes.len();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
    let wanted = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(MAX_OUT_BUFFER_BYTES);
    let end = start.saturating_add(wanted).min(total);
    let list = allocate_list(memory, &file.bytes[start..end])?;
    write_words(&mut out[..8], &list);
    out[8] = u8::from(end == total);
    Ok(())
}

fn result_layout(operation: Operation) -> (usize, usize) {
    use Operation::*;
    match operation {
        Stat | StatAt => (104, 8),
        Hash | HashAt => (24, 8),
        Write => (16, 8),
        Read => (16, 4),
        ReadlinkAt => (12, 4),
        DirectoryEntry => (20, 4),
        OpenAt | ReadDirectory | ReadViaStream | WriteViaStream | AppendViaStream => (8, 4),
        Directories => (8, 4),
        DropDescriptor | DropDirectoryStream => (0, 0),
        Advise | Flags | Type | SetSize | SetTimes | SetTimesAt | Sync | CreateDirectoryAt
        | RemoveDirectoryAt | RenameAt | SymlinkAt | LinkAt | UnlinkFileAt => (2, 1),
    }
}

/// Execute an admitted canonical filesystem call. All state changes affect only guest bytes.
/// Invalid descriptors precede path lookup; mutations fail read-only before lookup.
pub fn call(
    files: &PlatformFiles,
    memory: &mut impl Memory,
    definition: &Definition,
    arguments: &[u64],
) -> Result<()> {
    use Operation::*;
    let signature = definition
        .pal
        .sig
        .ok_or_else(|| anyhow::anyhow!("filesystem signature is missing"))?;
    ensure!(
        arguments.len() == signature.params.len(),
        "filesystem canonical argument count is wrong"
    );
    let operation = definition.operation;
    let arg = |index: usize| arguments[index] as u32;
    if operation == Directories {
        let descriptor = record(memory, DESCRIPTOR, Node::ROOT)?;
        let path = memory.allocate(1)?;
        ensure!(path != 0, "filesystem root path allocation failed");
        memory.write(path, b"/")?;
        let entries = memory.allocate(12)?;
        ensure!(entries != 0, "filesystem preopen list allocation failed");
        let mut entry = [0u8; 12];
        write_words(&mut entry, &[descriptor, path, 1]);
        memory.write(entries, &entry)?;
        let mut list = [0u8; 8];
        write_words(&mut list, &[entries, 1]);
        return memory.write(arg(0), &list);
    }
    let stream = matches!(operation, DirectoryEntry | DropDirectoryStream);
    let kind = if stream { DIRECTORY_STREAM } else { DESCRIPTOR };
    let record_of = read_record(files, memory, arg(0), kind);
    if matches!(operation, DropDescriptor | DropDirectoryStream) {
        ensure!(
            record_of.is_some(),
            "filesystem resource-drop used an invalid handle"
        );
        memory.write(arg(0), &[0; RECORD_BYTES as usize])?;
        return memory.free(arg(0), RECORD_BYTES);
    }
    let retptr = arg(arguments.len() - 1);
    let (size, offset) = result_layout(operation);
    let Some(Record { node, cursor }) = record_of else {
        return error(memory, retptr, offset, BAD_DESCRIPTOR);
    };
    if matches!(operation, RenameAt | LinkAt) {
        let index = if operation == RenameAt { 3 } else { 4 };
        if read_record(files, memory, arg(index), DESCRIPTOR).is_none() {
            return error(memory, retptr, offset, BAD_DESCRIPTOR);
        }
    }
    let mut result = [0u8; 104];
    match operation {
        SetSize | SetTimes | SetTimesAt | Write | WriteViaStream | AppendViaStream
        | CreateDirectoryAt | RemoveDirectoryAt | RenameAt | SymlinkAt | LinkAt | UnlinkFileAt => {
            return error(memory, retptr, offset, READ_ONLY)
        }
        // A stream's blocking read would be blocking I/O inside a call.
        ReadViaStream => {
            let code = if node.kind() == NodeKind::File {
                NOT_PERMITTED
            } else {
                IS_DIRECTORY
            };
            return error(memory, retptr, offset, code);
        }
        Read => match files.file(node) {
            Some(file) => read_file(memory, file, arguments[1], arguments[2], &mut result[4..])?,
            None => return error(memory, retptr, offset, IS_DIRECTORY),
        },
        // Nothing declared is a symbolic link.
        ReadlinkAt => {
            let code = lookup(files, memory, node, arg(1), arg(2))?
                .err()
                .unwrap_or(INVALID);
            return error(memory, retptr, offset, code);
        }
        StatAt | HashAt | OpenAt => {
            if operation == OpenAt
                && (arg(4) & (OPEN_FLAG_CREATE | OPEN_FLAG_TRUNCATE) != 0
                    || arg(5) & (DESCRIPTOR_FLAG_WRITE | DESCRIPTOR_FLAG_MUTATE_DIRECTORY) != 0)
            {
                return error(memory, retptr, offset, READ_ONLY);
            }
            let target = match lookup(files, memory, node, arg(2), arg(3))? {
                Ok(target) => target,
                Err(code) => return error(memory, retptr, offset, code),
            };
            match operation {
                StatAt => stat(files, target, &mut result[8..]),
                HashAt => metadata_hash(files, target, &mut result[8..]),
                OpenAt => {
                    if arg(4) & OPEN_FLAG_DIRECTORY != 0 && target.kind() == NodeKind::File {
                        return error(memory, retptr, offset, NOT_DIRECTORY);
                    }
                    let opened = record(memory, DESCRIPTOR, target)?;
                    write_words(&mut result[4..8], &[opened]);
                }
                _ => bail!("filesystem path operation reached the wrong dispatch path"),
            }
        }
        Stat => stat(files, node, &mut result[8..]),
        Hash => metadata_hash(files, node, &mut result[8..]),
        Type => result[1] = descriptor_type(node),
        Flags => result[1] = DESCRIPTOR_FLAG_READ,
        ReadDirectory => {
            if node.kind() == NodeKind::File {
                return error(memory, retptr, offset, NOT_DIRECTORY);
            }
            let stream = record(memory, DIRECTORY_STREAM, node)?;
            write_words(&mut result[4..8], &[stream]);
        }
        // `option<directory-entry>`: the flag at 4, then type at 8 and the
        // name's `(pointer, length)` at 12; an exhausted stream is EOF, all
        // zero.
        DirectoryEntry => {
            let child = files
                .children(node)
                .and_then(|children| children.get(cursor as usize));
            if let Some(child) = child {
                let name = allocate_list(memory, child.name.as_bytes())?;
                result[4] = 1;
                result[8] = descriptor_type(child.node);
                write_words(&mut result[12..20], &name);
                memory.write(arg(0) + 8, &(cursor + 1).to_le_bytes())?;
            }
        }
        // Read-only sync and advice have no effects.
        Sync | Advise => {}
        Directories | DropDescriptor | DropDirectoryStream => {
            bail!("filesystem operation reached the wrong dispatch path")
        }
    }
    memory.write(retptr, &result[..size])
}

/// Memory adapter for the execution engine's Wasmtime caller.
pub struct CallerMemory<'a, 'b, T: 'static>(pub &'a mut wasmtime::Caller<'b, T>);

impl<T: 'static> CallerMemory<'_, '_, T> {
    fn memory(&mut self) -> Result<wasmtime::Memory> {
        self.0
            .get_export("memory")
            .and_then(wasmtime::Extern::into_memory)
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no exported memory"))
    }
}

impl<T: 'static> Memory for CallerMemory<'_, '_, T> {
    fn read(&mut self, pointer: u32, bytes: &mut [u8]) -> Result<()> {
        self.memory()?.read(&*self.0, pointer as usize, bytes)?;
        Ok(())
    }
    fn write(&mut self, pointer: u32, bytes: &[u8]) -> Result<()> {
        self.memory()?
            .write(&mut *self.0, pointer as usize, bytes)?;
        Ok(())
    }
    fn allocate(&mut self, size_bytes: u32) -> Result<u32> {
        if let Some(function) = self
            .0
            .get_export(wisp_abi::exports::FS_ALLOC)
            .and_then(wasmtime::Extern::into_func)
        {
            return Ok(function
                .typed::<i32, i32>(&*self.0)?
                .call(&mut *self.0, size_bytes as i32)? as u32);
        }
        let function = self
            .0
            .get_export("cabi_realloc")
            .and_then(wasmtime::Extern::into_func)
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no allocator export"))?;
        Ok(function
            .typed::<(i32, i32, i32, i32), i32>(&*self.0)?
            .call(&mut *self.0, (0, 0, 4, size_bytes as i32))? as u32)
    }
    fn free(&mut self, pointer: u32, size_bytes: u32) -> Result<()> {
        if let Some(function) = self
            .0
            .get_export(wisp_abi::exports::FS_FREE)
            .and_then(wasmtime::Extern::into_func)
        {
            function
                .typed::<i32, ()>(&*self.0)?
                .call(&mut *self.0, pointer as i32)?;
            return Ok(());
        }
        let function = self
            .0
            .get_export("cabi_realloc")
            .and_then(wasmtime::Extern::into_func)
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no allocator export"))?;
        function
            .typed::<(i32, i32, i32, i32), i32>(&*self.0)?
            .call(&mut *self.0, (pointer as i32, size_bytes as i32, 4, 0))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Flat guest memory with a bump allocator, so an encoding can be read
    /// back byte for byte.
    struct FlatMemory {
        bytes: Vec<u8>,
        next: u32,
    }

    impl FlatMemory {
        fn new() -> Self {
            FlatMemory {
                bytes: vec![0; 1 << 16],
                next: 0x1000,
            }
        }
        fn word(&self, at: usize) -> u32 {
            u32::from_le_bytes([
                self.bytes[at],
                self.bytes[at + 1],
                self.bytes[at + 2],
                self.bytes[at + 3],
            ])
        }
        fn u64(&self, at: usize) -> u64 {
            let mut raw = [0; 8];
            raw.copy_from_slice(&self.bytes[at..at + 8]);
            u64::from_le_bytes(raw)
        }
        fn list(&self, at: usize) -> &[u8] {
            let (pointer, length) = (self.word(at) as usize, self.word(at + 4) as usize);
            &self.bytes[pointer..pointer + length]
        }
    }

    impl Memory for FlatMemory {
        fn read(&mut self, pointer: u32, bytes: &mut [u8]) -> Result<()> {
            let start = pointer as usize;
            let source = self
                .bytes
                .get(start..start + bytes.len())
                .ok_or_else(|| anyhow::anyhow!("read outside flat memory"))?;
            bytes.copy_from_slice(source);
            Ok(())
        }
        fn write(&mut self, pointer: u32, bytes: &[u8]) -> Result<()> {
            let start = pointer as usize;
            let target = self
                .bytes
                .get_mut(start..start + bytes.len())
                .ok_or_else(|| anyhow::anyhow!("write outside flat memory"))?;
            target.copy_from_slice(bytes);
            Ok(())
        }
        fn allocate(&mut self, size_bytes: u32) -> Result<u32> {
            let pointer = self.next;
            self.next += (size_bytes + 15) & !15;
            Ok(pointer)
        }
        fn free(&mut self, _pointer: u32, _size_bytes: u32) -> Result<()> {
            Ok(())
        }
    }

    const MODIFIED_NS: u64 = 1_700_000_000_123_456_789;
    const RET: u32 = 0x200;

    fn files() -> PlatformFiles {
        PlatformFiles::new(vec![PlatformFile {
            path: "/etc/ssl/certs/ca-certificates.crt".into(),
            bytes: Arc::from(&b"-----BEGIN CERTIFICATE-----"[..]),
            modified_ns: MODIFIED_NS,
        }])
        .expect("one declared file")
    }

    fn definition(operation: Operation) -> &'static Definition {
        &IMPORTS[operation as usize]
    }

    fn open(files: &PlatformFiles, memory: &mut FlatMemory, path: &str) -> u32 {
        let root = record(memory, DESCRIPTOR, Node::ROOT).expect("root record");
        let path_at = memory.allocate(path.len() as u32).expect("path bytes");
        memory
            .write(path_at, path.as_bytes())
            .expect("path written");
        call(
            files,
            memory,
            definition(Operation::OpenAt),
            &[
                root.into(),
                0,
                path_at.into(),
                path.len() as u64,
                0,
                0,
                RET.into(),
            ],
        )
        .expect("open-at executes");
        assert_eq!(
            memory.bytes[RET as usize], 0,
            "open-at must succeed for {path}"
        );
        memory.word(RET as usize + 4)
    }

    #[test]
    fn a_file_stat_is_encoded_as_the_canonical_descriptor_stat() {
        let files = files();
        let mut memory = FlatMemory::new();
        let file = open(&files, &mut memory, "etc/ssl/certs/ca-certificates.crt");
        call(
            &files,
            &mut memory,
            definition(Operation::Stat),
            &[file.into(), RET.into()],
        )
        .expect("stat executes");
        let at = RET as usize;
        assert_eq!(memory.bytes[at], 0, "stat succeeds");
        assert_eq!(
            memory.bytes[at + 8],
            REGULAR_FILE,
            "type at payload offset 0"
        );
        assert_eq!(memory.u64(at + 16), 1, "link count at payload offset 8");
        assert_eq!(memory.u64(at + 24), 27, "size at payload offset 16");
        for (name, base) in [("access", 32), ("modification", 56), ("status-change", 80)] {
            assert_eq!(memory.bytes[at + base], 1, "{name} timestamp is present");
            assert_eq!(
                memory.u64(at + base + 8),
                1_700_000_000,
                "{name} seconds at option payload offset 8"
            );
            assert_eq!(
                memory.word(at + base + 16),
                123_456_789,
                "{name} nanoseconds at option payload offset 16"
            );
        }
    }

    #[test]
    fn a_directory_stat_carries_the_newest_file_time_beneath_it() {
        let files = files();
        let mut memory = FlatMemory::new();
        let certs = open(&files, &mut memory, "etc/ssl/certs");
        call(
            &files,
            &mut memory,
            definition(Operation::Stat),
            &[certs.into(), RET.into()],
        )
        .expect("stat executes");
        let at = RET as usize;
        assert_eq!(
            memory.bytes[at + 8],
            DIRECTORY,
            "the implied directory is a directory"
        );
        assert_eq!(memory.u64(at + 24), 0, "a directory has no size");
        for base in [32, 56, 80] {
            assert_eq!(
                (
                    memory.bytes[at + base],
                    memory.u64(at + base + 8),
                    memory.word(at + base + 16)
                ),
                (1, 1_700_000_000, 123_456_789),
                "each directory timestamp is its newest file's modification time"
            );
        }
    }

    #[test]
    fn a_directory_entry_is_encoded_as_the_canonical_option_record() {
        let files = files();
        let mut memory = FlatMemory::new();
        let certs = open(&files, &mut memory, "etc/ssl/certs");
        call(
            &files,
            &mut memory,
            definition(Operation::ReadDirectory),
            &[certs.into(), RET.into()],
        )
        .expect("read-directory executes");
        let stream = memory.word(RET as usize + 4);
        call(
            &files,
            &mut memory,
            definition(Operation::DirectoryEntry),
            &[stream.into(), RET.into()],
        )
        .expect("read-directory-entry executes");
        let at = RET as usize;
        assert_eq!(memory.bytes[at], 0, "the entry read succeeds");
        assert_eq!(
            memory.bytes[at + 4],
            1,
            "the option flag at payload offset 0 is some"
        );
        assert_eq!(
            memory.bytes[at + 8],
            REGULAR_FILE,
            "entry type at option payload offset 0"
        );
        assert_eq!(
            memory.list(at + 12),
            b"ca-certificates.crt",
            "the name's (pointer, length) at option payload offset 4"
        );
        call(
            &files,
            &mut memory,
            definition(Operation::DirectoryEntry),
            &[stream.into(), RET.into()],
        )
        .expect("read-directory-entry executes again");
        assert_eq!(
            &memory.bytes[at..at + 20],
            &[0; 20],
            "the second read is EOF: ok, none"
        );
    }

    #[test]
    fn a_file_read_is_encoded_as_the_canonical_list_and_eof_pair() {
        let files = files();
        let mut memory = FlatMemory::new();
        let file = open(&files, &mut memory, "etc/ssl/certs/ca-certificates.crt");
        call(
            &files,
            &mut memory,
            definition(Operation::Read),
            &[file.into(), 5, 11, RET.into()],
        )
        .expect("read executes");
        let at = RET as usize;
        assert_eq!(memory.bytes[at], 0, "the read succeeds");
        assert_eq!(
            memory.list(at + 4),
            b"CERTI",
            "the list's (pointer, length) at payload offset 0 holds the slice from the offset"
        );
        assert_eq!(
            memory.bytes[at + 12],
            0,
            "the eof flag at payload offset 8 is clear mid-file"
        );
        call(
            &files,
            &mut memory,
            definition(Operation::Read),
            &[file.into(), 100, 11, RET.into()],
        )
        .expect("read executes");
        assert_eq!(
            memory.list(at + 4),
            b"CERTIFICATE-----",
            "a read past the end returns what remains"
        );
        assert_eq!(
            memory.bytes[at + 12],
            1,
            "reaching the end sets the eof flag"
        );
    }

    #[test]
    fn a_metadata_hash_is_encoded_as_two_words_after_the_result_tag() {
        let files = files();
        let mut memory = FlatMemory::new();
        let file = open(&files, &mut memory, "etc/ssl/certs/ca-certificates.crt");
        call(
            &files,
            &mut memory,
            definition(Operation::Hash),
            &[file.into(), RET.into()],
        )
        .expect("metadata-hash executes");
        let at = RET as usize;
        let expected = blake3::hash(b"/etc/ssl/certs/ca-certificates.crt");
        assert_eq!(memory.bytes[at], 0, "the hash succeeds");
        assert_eq!(
            &memory.bytes[at + 8..at + 24],
            &expected.as_bytes()[..16],
            "lower at payload offset 0 and upper at 8 are the path hash"
        );
    }

    #[test]
    fn a_descriptor_record_names_its_node_and_a_stream_carries_its_cursor() {
        let files = files();
        let mut memory = FlatMemory::new();
        let etc = open(&files, &mut memory, "etc");
        let at = etc as usize;
        assert_eq!(
            memory.word(at),
            DESCRIPTOR,
            "word 0 is the descriptor magic"
        );
        let node = files
            .decode(memory.word(at + 4))
            .expect("word 1 names a node");
        assert_eq!(
            files.path(node),
            "/etc",
            "word 1 names the opened directory"
        );
        assert_eq!(memory.word(at + 8), 0, "a descriptor's cursor word is zero");
        call(
            &files,
            &mut memory,
            definition(Operation::ReadDirectory),
            &[etc.into(), RET.into()],
        )
        .expect("read-directory executes");
        let stream = memory.word(RET as usize + 4) as usize;
        assert_eq!(
            memory.word(stream),
            DIRECTORY_STREAM,
            "word 0 is the stream magic"
        );
        assert_eq!(
            memory.word(stream + 4),
            node.word(),
            "a stream names the directory it enumerates"
        );
        call(
            &files,
            &mut memory,
            definition(Operation::DirectoryEntry),
            &[(stream as u32).into(), RET.into()],
        )
        .expect("read-directory-entry executes");
        assert_eq!(
            memory.word(stream + 8),
            1,
            "each entry read advances the cursor in the record"
        );
    }
}
