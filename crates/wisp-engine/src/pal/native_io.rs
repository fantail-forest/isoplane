//! Canonical scalar and memory conversion for host-owned native resources.

use anyhow::{bail, ensure, Context, Result};
use wisp_abi::native_io::canonical::{Action, Definition, DEFINITIONS, MAX_ARGUMENTS};
use wisp_abi::native_io::{DurationNanos, Request, ResourceHandle, Response};

use super::filesystem::{CallerMemory, Memory};
use super::PalSource;

/// Decode a bounded packet and enforce the capability declared by its caller.
pub fn packet<T: PalSource>(
    caller: &mut wasmtime::Caller<'_, T>,
    capability: &str,
    operation: u32,
    pointer: u32,
    count: u32,
) -> Result<u64> {
    let definition = DEFINITIONS
        .iter()
        .find(|definition| definition.id.0 == operation)
        .context("unknown native I/O operation")?;
    ensure!(
        definition.capability == capability,
        "native I/O packet attempted a different capability"
    );
    let signature = definition.pal.sig.context("native I/O signature missing")?;
    ensure!(
        count as usize == signature.params.len(),
        "native I/O argument count differs from its canonical signature"
    );
    let mut arguments = [0; MAX_ARGUMENTS];
    for (index, argument) in arguments.iter_mut().take(count as usize).enumerate() {
        let address = pointer
            .checked_add((index * 8) as u32)
            .context("native I/O packet address overflow")?;
        let mut bytes = [0; 8];
        CallerMemory(caller).read(address, &mut bytes)?;
        *argument = u64::from_le_bytes(bytes);
    }
    call(caller, definition, &arguments[..count as usize])
}

/// Execute an admitted canonical call without retaining a guest invocation stack.
pub fn call<T: PalSource>(
    caller: &mut wasmtime::Caller<'_, T>,
    definition: &Definition,
    arguments: &[u64],
) -> Result<u64> {
    let scalar = |index: usize| {
        arguments
            .get(index)
            .copied()
            .context("native I/O argument missing")
    };
    let handle = |index| scalar(index).and_then(|value| Ok(ResourceHandle(u32::try_from(value)?)));
    let request = match definition.action {
        Action::Timer { absolute } => {
            let nanos = scalar(0)?;
            let duration = if absolute {
                nanos.saturating_sub(caller.data_mut().slice_clock_ms().saturating_mul(1_000_000))
            } else {
                nanos
            };
            Request::Timer(DurationNanos(duration))
        }
        Action::ClockNow => {
            return Ok(caller.data_mut().slice_clock_ms().saturating_mul(1_000_000))
        }
        Action::ClockResolution => return Ok(wisp_abi::pal::CLOCK_QUANTUM_NS as u64),
        Action::Ready => Request::Ready(handle(0)?),
        Action::Drop => Request::Drop(handle(0)?),
        Action::Poll => {
            let count = usize::try_from(scalar(1)?)?;
            ensure!(
                count <= wisp_abi::imports::MAX_OUT_BUFFER_BYTES / 4,
                "native poll list exceeds host buffer limit"
            );
            let pointer = u32::try_from(scalar(0)?)?;
            let mut handles = Vec::with_capacity(count);
            for index in 0..count {
                let address = pointer
                    .checked_add((index * 4) as u32)
                    .context("native poll address overflow")?;
                let mut bytes = [0; 4];
                CallerMemory(caller).read(address, &mut bytes)?;
                handles.push(ResourceHandle(u32::from_le_bytes(bytes)));
            }
            Request::Poll(handles)
        }
        Action::Trap => bail!(
            "unsupported native operation {}",
            definition.pal.full_name()
        ),
    };
    let result = caller.data_mut().native_io(request)?;
    match result {
        Response::Done => Ok(0),
        Response::Handle(handle) => Ok(u64::from(handle.0)),
        Response::Ready(ready) => Ok(u64::from(ready)),
        Response::ReadyIndexes(indexes) => {
            let bytes: Vec<_> = indexes
                .iter()
                .flat_map(|index| index.to_le_bytes())
                .collect();
            let pointer = CallerMemory(caller).allocate(u32::try_from(bytes.len())?)?;
            ensure!(pointer != 0, "native poll result allocation failed");
            CallerMemory(caller).write(pointer, &bytes)?;
            let mut record = [0; 8];
            record[..4].copy_from_slice(&pointer.to_le_bytes());
            record[4..].copy_from_slice(&u32::try_from(indexes.len())?.to_le_bytes());
            CallerMemory(caller).write(return_pointer(arguments)?, &record)?;
            Ok(0)
        }
    }
}

fn return_pointer(arguments: &[u64]) -> Result<u32> {
    Ok(u32::try_from(
        *arguments.last().context("native result pointer missing")?,
    )?)
}
