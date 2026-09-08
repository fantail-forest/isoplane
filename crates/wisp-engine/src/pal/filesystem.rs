//! The empty disk has one directory and no external resources. Descriptor records
//! live in guest memory, so templates and deltas retain their complete state.

use anyhow::{bail, ensure, Result};
use wisp_abi::filesystem::{Definition, Operation, IMPORTS, MAX_ARGUMENTS};
use wisp_abi::imports::MAX_OUT_BUFFER_BYTES;

const ROOT: u32 = 0x77534601;
const DIRECTORY_STREAM: u32 = 0x77534602;
const BAD_DESCRIPTOR: u8 = 3;
const INVALID: u8 = 12;
const IS_DIRECTORY: u8 = 14;
const NO_ENTRY: u8 = 20;
const NOT_PERMITTED: u8 = 31;
const READ_ONLY: u8 = 33;
const DIRECTORY: u8 = 3;

/// Guest memory access for both the execution engine and the sealed initializer.
/// Allocation and deallocation invoke the guest's allocator, never a host resource table.
pub trait Memory {
    /// Copy bytes from guest memory, rejecting out-of-bounds addresses.
    fn read(&mut self, pointer: u32, bytes: &mut [u8]) -> Result<()>;
    /// Copy bytes into guest memory, rejecting out-of-bounds addresses.
    fn write(&mut self, pointer: u32, bytes: &[u8]) -> Result<()>;
    /// Allocate guest memory aligned to at least four bytes.
    fn allocate(&mut self, size_bytes: u32) -> Result<u32>;
    /// Release a four-byte descriptor record through the guest allocator.
    fn free(&mut self, pointer: u32) -> Result<()>;
}

/// Execute an SDK wrapper's operation and canonical scalar arguments.
/// Unknown operations, malformed packets and invalid memory trap.
pub fn packet(memory: &mut impl Memory, operation: u32, pointer: u32, count: u32) -> Result<()> {
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
    call(memory, definition, &arguments[..count as usize])
}

fn record(memory: &mut impl Memory, kind: u32) -> Result<u32> {
    let pointer = memory.allocate(4)?;
    ensure!(
        pointer != 0 && pointer < (1 << 31),
        "filesystem descriptor allocation failed"
    );
    memory.write(pointer, &kind.to_le_bytes())?;
    Ok(pointer)
}

fn valid(memory: &mut impl Memory, pointer: u32, kind: u32) -> bool {
    if pointer == 0 || pointer >= (1 << 31) {
        return false;
    }
    let mut bytes = [0; 4];
    // An arbitrary integer is an invalid descriptor, including an address outside memory.
    memory.read(pointer, &mut bytes).is_ok() && u32::from_le_bytes(bytes) == kind
}

fn error(memory: &mut impl Memory, pointer: u32, offset: usize, code: u8) -> Result<()> {
    let mut bytes = [0; 9];
    bytes[0] = 1;
    bytes[offset] = code;
    memory.write(pointer, &bytes[..=offset])
}

fn root_path(
    memory: &mut impl Memory,
    pointer: u32,
    length: u32,
) -> Result<std::result::Result<(), u8>> {
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
    if path
        .split('/')
        .all(|part| part.is_empty() || part == "." || part == "..")
    {
        Ok(Ok(()))
    } else if path.split('/').any(|part| part == "..") {
        Ok(Err(NOT_PERMITTED))
    } else {
        Ok(Err(NO_ENTRY))
    }
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
        Advise | Flags | Type | SetSize | SetTimes | Sync | CreateDirectoryAt
        | RemoveDirectoryAt | RenameAt | SymlinkAt | LinkAt | UnlinkFileAt => (2, 1),
    }
}

/// Execute an admitted canonical filesystem call. All state changes affect only guest bytes.
/// Invalid descriptors precede path lookup; mutations fail read-only before lookup.
pub fn call(memory: &mut impl Memory, definition: &Definition, arguments: &[u64]) -> Result<()> {
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
        let descriptor = record(memory, ROOT)?;
        let path = memory.allocate(1)?;
        ensure!(path != 0, "filesystem root path allocation failed");
        memory.write(path, b"/")?;
        let entries = memory.allocate(12)?;
        ensure!(entries != 0, "filesystem preopen list allocation failed");
        let bytes: Vec<u8> = [descriptor, path, 1]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        memory.write(entries, &bytes)?;
        let list: Vec<u8> = [entries, 1]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        return memory.write(arg(0), &list);
    }
    let stream = matches!(operation, DirectoryEntry | DropDirectoryStream);
    let kind = if stream { DIRECTORY_STREAM } else { ROOT };
    let is_valid = valid(memory, arg(0), kind);
    if matches!(operation, DropDescriptor | DropDirectoryStream) {
        ensure!(is_valid, "filesystem resource-drop used an invalid handle");
        memory.write(arg(0), &[0; 4])?;
        return memory.free(arg(0));
    }
    let retptr = arg(arguments.len() - 1);
    let (size, offset) = result_layout(operation);
    if !is_valid {
        return error(memory, retptr, offset, BAD_DESCRIPTOR);
    }
    if matches!(operation, RenameAt | LinkAt) {
        let index = if operation == RenameAt { 3 } else { 4 };
        if !valid(memory, arg(index), ROOT) {
            return error(memory, retptr, offset, BAD_DESCRIPTOR);
        }
    }
    let mut result = [0u8; 104];
    match operation {
        SetSize | SetTimes | Write | WriteViaStream | AppendViaStream | CreateDirectoryAt
        | RemoveDirectoryAt | RenameAt | SymlinkAt | LinkAt | UnlinkFileAt => {
            return error(memory, retptr, offset, READ_ONLY)
        }
        Read | ReadViaStream => return error(memory, retptr, offset, IS_DIRECTORY),
        ReadlinkAt => {
            let code = root_path(memory, arg(1), arg(2))?.err().unwrap_or(INVALID);
            return error(memory, retptr, offset, code);
        }
        StatAt | HashAt | OpenAt => {
            if operation == OpenAt && (arg(4) & (1 | 8) != 0 || arg(5) & (2 | 32) != 0) {
                return error(memory, retptr, offset, READ_ONLY);
            }
            if let Err(code) = root_path(memory, arg(2), arg(3))? {
                return error(memory, retptr, offset, code);
            }
            match operation {
                StatAt => {
                    result[8] = DIRECTORY;
                    result[16] = 1;
                }
                HashAt => {
                    result[8..16].copy_from_slice(&u64::from(ROOT).to_le_bytes());
                }
                OpenAt => {
                    result[4..8].copy_from_slice(&record(memory, ROOT)?.to_le_bytes());
                }
                _ => bail!("filesystem path operation reached the wrong dispatch path"),
            }
        }
        Stat => {
            result[8] = DIRECTORY;
            result[16] = 1;
        }
        Hash => {
            result[8..16].copy_from_slice(&u64::from(ROOT).to_le_bytes());
        }
        Type => result[1] = DIRECTORY,
        Flags => result[1] = 1,
        ReadDirectory => {
            result[4..8].copy_from_slice(&record(memory, DIRECTORY_STREAM)?.to_le_bytes())
        }
        // An empty enumeration is EOF; read-only sync and advice have no effects.
        DirectoryEntry | Sync | Advise => {}
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
    fn free(&mut self, pointer: u32) -> Result<()> {
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
            .call(&mut *self.0, (pointer as i32, 4, 4, 0))?;
        Ok(())
    }
}
