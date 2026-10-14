//! Canonical scalar and memory conversion for host-owned native resources.

use anyhow::{bail, ensure, Context, Result};
use isoplane_abi::native_io::canonical::{
    Action, Definition, KeepAliveSetting, DEFINITIONS, MAX_ARGUMENTS,
};
use isoplane_abi::native_io::{
    AddressFamily, ByteCount, DurationNanos, Error, KeepAlive, KeepAliveChange, ProbeCount,
    Request, ResourceHandle, Response,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

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
    caller
        .data_mut()
        .check_capability(definition.capability)
        .map_err(anyhow::Error::from)?;
    let scalar = |index: usize| {
        arguments
            .get(index)
            .copied()
            .context("native I/O argument missing")
    };
    let handle = |index| scalar(index).and_then(|value| Ok(ResourceHandle(u32::try_from(value)?)));
    let request = match definition.action {
        Action::NoDelay => Request::NoDelay {
            socket: handle(0)?,
            enabled: match scalar(1)? {
                0 => Some(false),
                1 => Some(true),
                value if value == u64::from(u32::MAX) => None,
                _ => bail!("invalid TCP_NODELAY setting"),
            },
        },
        Action::KeepAlive(_) => Request::KeepAlive {
            socket: handle(0)?,
            change: None,
        },
        Action::SetKeepAlive(setting) => Request::KeepAlive {
            socket: handle(0)?,
            change: Some(keep_alive_change(setting, scalar(1)?)?),
        },
        Action::Network => Request::Network,
        Action::Resolve => {
            let length = usize::try_from(scalar(2)?)?;
            ensure!(
                length <= isoplane_abi::imports::MAX_OUT_BUFFER_BYTES,
                "native DNS name exceeds host buffer limit"
            );
            let mut bytes = vec![0; length];
            CallerMemory(caller).read(u32::try_from(scalar(1)?)?, &mut bytes)?;
            Request::Resolve {
                network: handle(0)?,
                name: String::from_utf8(bytes)?,
            }
        }
        Action::NextAddress => Request::NextAddress(handle(0)?),
        Action::SocketAddress { peer } => Request::SocketAddress {
            socket: handle(0)?,
            peer,
        },
        Action::Shutdown => Request::Shutdown {
            socket: handle(0)?,
            how: match scalar(1)? {
                0 => std::net::Shutdown::Read,
                1 => std::net::Shutdown::Write,
                2 => std::net::Shutdown::Both,
                _ => bail!("invalid canonical socket shutdown direction"),
            },
        },
        Action::CreateTcp => Request::CreateTcp(match scalar(0)? {
            0 => AddressFamily::V4,
            1 => AddressFamily::V6,
            _ => bail!("invalid socket address family"),
        }),
        Action::Connect => Request::Connect {
            socket: handle(0)?,
            network: handle(1)?,
            destination: destination(arguments)?,
        },
        Action::FinishConnect => Request::FinishConnect(handle(0)?),
        Action::Read => Request::Read {
            stream: handle(0)?,
            length: ByteCount(scalar(1)?),
        },
        Action::ErrorMessage => Request::ErrorMessage(handle(0)?),
        Action::ErrorCode => Request::ErrorCode(handle(0)?),
        Action::TrySend | Action::TrySendWithErrorCode => {
            let count = usize::try_from(scalar(1)?)?;
            ensure!(
                count <= isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4,
                "native send pollables exceed host buffer limit"
            );
            let pointer = u32::try_from(scalar(0)?)?;
            let mut pollables = Vec::with_capacity(count);
            for index in 0..count {
                let address = pointer
                    .checked_add((index * 4) as u32)
                    .context("native send pollable address overflow")?;
                let mut bytes = [0; 4];
                CallerMemory(caller).read(address, &mut bytes)?;
                pollables.push(ResourceHandle(u32::from_le_bytes(bytes)));
            }
            let length =
                usize::try_from(scalar(3)?)?.min(isoplane_abi::imports::MAX_OUT_BUFFER_BYTES);
            let mut bytes = vec![0; length];
            CallerMemory(caller).read(u32::try_from(scalar(2)?)?, &mut bytes)?;
            Request::TrySend { pollables, bytes }
        }
        Action::Subscribe => Request::Subscribe(handle(0)?),
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
        Action::ClockResolution => return Ok(isoplane_abi::pal::CLOCK_QUANTUM_NS as u64),
        Action::Ready => Request::Ready(handle(0)?),
        Action::Drop(kind) => Request::Drop {
            handle: handle(0)?,
            kind,
        },
        Action::Poll => {
            let count = usize::try_from(scalar(1)?)?;
            ensure!(
                count <= isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4,
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
        Action::Reject { error_offset, code } => {
            write_error(&mut CallerMemory(caller), arguments, error_offset, code)?;
            return Ok(0);
        }
        Action::Trap => bail!(
            "unsupported native operation {}",
            definition.pal.full_name()
        ),
    };
    let result = caller.data_mut().native_io(request);
    match result {
        Ok(Response::NoDelay(enabled)) => Ok(u64::from(enabled)),
        Ok(Response::KeepAlive(effective)) => {
            // A setter's `result<_, error-code>` is the ok discriminant and
            // its padding; a getter's carries the value it read.
            let mut record = [0; 16];
            let length = if let Action::KeepAlive(setting) = definition.action {
                write_keep_alive_value(&mut record, setting, effective)
            } else {
                2
            };
            CallerMemory(caller).write(return_pointer(arguments)?, &record[..length])?;
            Ok(0)
        }
        Ok(Response::Done) => {
            if matches!(definition.action, Action::Connect | Action::Shutdown) {
                CallerMemory(caller).write(return_pointer(arguments)?, &[0, 0])?;
            }
            Ok(0)
        }
        Ok(Response::Handle(handle)) => {
            if matches!(definition.action, Action::CreateTcp | Action::Resolve) {
                let mut bytes = [0; 8];
                bytes[4..].copy_from_slice(&handle.0.to_le_bytes());
                CallerMemory(caller).write(return_pointer(arguments)?, &bytes)?;
                Ok(0)
            } else {
                Ok(u64::from(handle.0))
            }
        }
        Ok(Response::Streams { input, output }) => {
            let mut bytes = [0; 12];
            bytes[4..8].copy_from_slice(&input.0.to_le_bytes());
            bytes[8..12].copy_from_slice(&output.0.to_le_bytes());
            CallerMemory(caller).write(return_pointer(arguments)?, &bytes)?;
            Ok(0)
        }
        Ok(Response::Ready(ready)) => Ok(u64::from(ready)),
        Ok(Response::SocketAddress(address)) => {
            let mut record = [0; 36];
            record[8..10].copy_from_slice(&address.port().to_le_bytes());
            match address {
                SocketAddr::V4(address) => record[10..14].copy_from_slice(&address.ip().octets()),
                SocketAddr::V6(address) => {
                    record[4] = 1;
                    record[12..16].copy_from_slice(&address.flowinfo().to_le_bytes());
                    for (index, segment) in address.ip().segments().into_iter().enumerate() {
                        record[16 + index * 2..18 + index * 2]
                            .copy_from_slice(&segment.to_le_bytes());
                    }
                    record[32..].copy_from_slice(&address.scope_id().to_le_bytes());
                }
            }
            CallerMemory(caller).write(return_pointer(arguments)?, &record)?;
            Ok(0)
        }
        Ok(Response::Address(address)) => {
            let mut record = [0; 22];
            if let Some(address) = address {
                record[2] = 1;
                match address {
                    IpAddr::V4(address) => record[6..10].copy_from_slice(&address.octets()),
                    IpAddr::V6(address) => {
                        record[4] = 1;
                        for (index, segment) in address.segments().into_iter().enumerate() {
                            record[6 + index * 2..8 + index * 2]
                                .copy_from_slice(&segment.to_le_bytes());
                        }
                    }
                }
            }
            CallerMemory(caller).write(return_pointer(arguments)?, &record)?;
            Ok(0)
        }
        Ok(Response::Bytes(bytes)) => {
            let pointer = if bytes.is_empty() {
                0
            } else {
                let pointer = CallerMemory(caller).allocate(u32::try_from(bytes.len())?)?;
                ensure!(pointer != 0, "native stream result allocation failed");
                CallerMemory(caller).write(pointer, &bytes)?;
                pointer
            };
            let mut record = [0; 12];
            let offset = if matches!(definition.action, Action::ErrorMessage) {
                0
            } else {
                4
            };
            record[offset..offset + 4].copy_from_slice(&pointer.to_le_bytes());
            record[offset + 4..offset + 8]
                .copy_from_slice(&u32::try_from(bytes.len())?.to_le_bytes());
            CallerMemory(caller).write(return_pointer(arguments)?, &record[..offset + 8])?;
            Ok(0)
        }
        Ok(Response::Capacity(capacity)) => Ok(capacity.0),
        Ok(Response::Closed) => {
            // stream-error::closed, after the discriminant word.
            let mut record = [0; 12];
            record[0] = 1;
            record[4] = 1;
            CallerMemory(caller).write(return_pointer(arguments)?, &record)?;
            Ok(0)
        }
        Ok(Response::ErrorCode(code)) => {
            // option<error-code>: the discriminant, then the code.
            CallerMemory(caller).write(return_pointer(arguments)?, &[1, code as u8])?;
            Ok(0)
        }
        Ok(Response::StreamError(handle)) => {
            // stream-error::last-operation-failed, the error handle after
            // its discriminant.
            let mut record = [0; 12];
            record[0] = 1;
            record[8..].copy_from_slice(&handle.0.to_le_bytes());
            CallerMemory(caller).write(return_pointer(arguments)?, &record)?;
            Ok(0)
        }
        Ok(Response::ReadyIndexes(indexes)) => {
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
        Err(error) => {
            if matches!(definition.action, Action::NoDelay) {
                return Ok(u64::MAX);
            }
            if matches!(
                definition.action,
                Action::TrySend | Action::TrySendWithErrorCode
            ) {
                return Ok(if matches!(error, Error::WouldBlock) {
                    isoplane_abi::native_io::canonical::SEND_PENDING
                } else if matches!(definition.action, Action::TrySend) {
                    isoplane_abi::native_io::canonical::SEND_FAILED
                } else {
                    isoplane_abi::native_io::canonical::send_failure(error.network_error_code())
                });
            }
            let offset = match definition.action {
                Action::CreateTcp
                | Action::FinishConnect
                | Action::Resolve
                | Action::SocketAddress { .. } => 4,
                Action::NextAddress => 2,
                Action::Connect | Action::Shutdown | Action::SetKeepAlive(_) => 1,
                Action::KeepAlive(setting) => keep_alive_value_offset(setting),
                Action::Network
                | Action::NoDelay
                | Action::Subscribe
                | Action::Read
                | Action::TrySend
                | Action::TrySendWithErrorCode
                | Action::ErrorMessage
                | Action::ErrorCode
                | Action::Timer { .. }
                | Action::ClockNow
                | Action::ClockResolution
                | Action::Poll
                | Action::Ready
                | Action::Drop(_)
                | Action::Reject { .. }
                | Action::Trap => return Err(error.into()),
            };
            let code = error.network_error_code();
            write_error(&mut CallerMemory(caller), arguments, offset, code as u8)?;
            Ok(0)
        }
    }
}

fn keep_alive_change(setting: KeepAliveSetting, value: u64) -> Result<KeepAliveChange> {
    Ok(match setting {
        KeepAliveSetting::Enabled => KeepAliveChange::Enabled(value != 0),
        KeepAliveSetting::IdleTime => KeepAliveChange::IdleTime(DurationNanos(value)),
        KeepAliveSetting::Interval => KeepAliveChange::Interval(DurationNanos(value)),
        KeepAliveSetting::Count => KeepAliveChange::Count(ProbeCount(u32::try_from(value)?)),
    })
}

/// Where a keep-alive getter's `result<T, error-code>` holds its value or
/// error code: after the discriminant, at the value's canonical alignment.
fn keep_alive_value_offset(setting: KeepAliveSetting) -> usize {
    match setting {
        KeepAliveSetting::Enabled => 1,
        KeepAliveSetting::IdleTime | KeepAliveSetting::Interval => 8,
        KeepAliveSetting::Count => 4,
    }
}

/// Writes a getter's ok record into `record`, returning its length.
fn write_keep_alive_value(
    record: &mut [u8; 16],
    setting: KeepAliveSetting,
    effective: KeepAlive,
) -> usize {
    let offset = keep_alive_value_offset(setting);
    let mut put = |value: &[u8]| {
        record[offset..offset + value.len()].copy_from_slice(value);
        offset + value.len()
    };
    match setting {
        KeepAliveSetting::Enabled => put(&[u8::from(effective.enabled)]),
        KeepAliveSetting::IdleTime => put(&effective.idle_time.0.to_le_bytes()),
        KeepAliveSetting::Interval => put(&effective.interval.0.to_le_bytes()),
        KeepAliveSetting::Count => put(&effective.count.0.to_le_bytes()),
    }
}

fn return_pointer(arguments: &[u64]) -> Result<u32> {
    Ok(u32::try_from(
        *arguments.last().context("native result pointer missing")?,
    )?)
}

fn write_error(memory: &mut impl Memory, arguments: &[u64], offset: usize, code: u8) -> Result<()> {
    let mut bytes = [0; 16];
    bytes[0] = 1;
    bytes[offset] = code;
    memory.write(return_pointer(arguments)?, &bytes[..=offset])
}

fn destination(arguments: &[u64]) -> Result<SocketAddr> {
    let port = u16::try_from(arguments[3])?;
    match arguments[2] {
        0 => Ok(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(
                u8::try_from(arguments[4])?,
                u8::try_from(arguments[5])?,
                u8::try_from(arguments[6])?,
                u8::try_from(arguments[7])?,
            )),
            port,
        )),
        1 => {
            let mut segments = [0; 8];
            for (segment, value) in segments.iter_mut().zip(&arguments[5..13]) {
                *segment = u16::try_from(*value)?;
            }
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(segments),
                port,
                u32::try_from(arguments[4])?,
                u32::try_from(arguments[13])?,
            )))
        }
        _ => bail!("invalid canonical socket address family"),
    }
}
