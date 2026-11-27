//! Native platform operations consumed by managed local development tooling.

mod error;
mod interop;
mod local;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod process;
#[cfg(windows)]
#[path = "process_windows.rs"]
mod process;
mod process_records;
mod runtime_cache;
mod session;
mod session_events;

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Version of the native entry points and the records they exchange.
pub const ABI_VERSION: u32 = 6;

/// Outcome of a native operation.
#[repr(C)]
pub enum NativeStatus {
    /// The operation succeeded.
    Success,
    /// The supplied input could not be read.
    InvalidInput,
    /// An owned build-tool process could not be started, waited for or stopped.
    ProcessError,
    /// A native operation failed, or an unexpected panic was contained.
    InternalError,
}

/// An operation's outcome and, when it failed, its UTF-8 error message,
/// released once with `isoplane_result_free`.
#[repr(C)]
pub struct NativeResult {
    /// Whether the operation succeeded, and if not, how it failed.
    pub status: NativeStatus,
    /// Read-only error message allocated by this library; empty on success.
    pub data: *mut u8,
    /// Error message length in bytes.
    pub length: usize,
}

impl NativeResult {
    fn new(status: NativeStatus, payload: Vec<u8>) -> Self {
        let mut payload = payload.into_boxed_slice();
        let result = Self {
            status,
            data: payload.as_mut_ptr(),
            length: payload.len(),
        };
        std::mem::forget(payload);
        result
    }
}

/// Reports the ABI version before the managed caller uses any other entry point.
#[no_mangle]
pub extern "C" fn isoplane_abi_version() -> u32 {
    ABI_VERSION
}

fn boundary(operation: impl FnOnce() -> Result<(), error::Error>) -> NativeResult {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => NativeResult::new(NativeStatus::Success, Vec::new()),
        Ok(Err(error)) => {
            let status = match &error {
                error::Error::Process(_) => NativeStatus::ProcessError,
                error::Error::Buffer
                | error::Error::Text
                | error::Error::DebugEndpoint
                | error::Error::Settings(_)
                | error::Error::CacheDirectory => NativeStatus::InvalidInput,
                error::Error::Session(_)
                | error::Error::Cache(_)
                | error::Error::RuntimePreparation(_) => NativeStatus::InternalError,
            };
            NativeResult::new(status, error.to_string().into_bytes())
        }
        Err(payload) => {
            // A foreign panic payload may itself panic on drop. Keep it from
            // unwinding through the ABI after the original panic was contained.
            std::mem::forget(payload);
            NativeResult::new(
                NativeStatus::InternalError,
                b"Native operation failed unexpectedly; retry and report the diagnostics.".to_vec(),
            )
        }
    }
}

/// The record the caller lends at `pointer`.
///
/// # Safety
/// A non-null pointer refers to a record that stays alive and unchanged for `'a`.
unsafe fn record<'a, T>(pointer: *const T) -> Result<&'a T, error::Error> {
    // SAFETY: forwarded from the caller; null is refused.
    unsafe { pointer.as_ref() }.ok_or(error::Error::Buffer)
}

/// Where the caller receives a report.
///
/// # Safety
/// A non-null pointer refers to writable storage for one `T` for `'a`.
unsafe fn output<'a, T>(pointer: *mut T) -> Result<&'a mut MaybeUninit<T>, error::Error> {
    // SAFETY: forwarded from the caller; null is refused.
    unsafe { pointer.cast::<MaybeUninit<T>>().as_mut() }.ok_or(error::Error::Buffer)
}

/// Releases the error message of a result returned by this library.
///
/// # Safety
/// Pass an unchanged result exactly once, after all message readers finish.
/// The managed wrapper calls this in a finally block after copying the message.
#[no_mangle]
pub unsafe extern "C" fn isoplane_result_free(result: NativeResult) {
    // SAFETY: this allocation was created from a boxed slice in NativeResult::new;
    // the caller transfers sole ownership back with its original pointer and length.
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            result.data,
            result.length,
        )));
    }
}

/// Creates a local session lifetime. Free it only after its run call returns.
#[no_mangle]
pub extern "C" fn isoplane_session_create() -> *mut c_void {
    Box::into_raw(Box::new(session::Session::new())).cast()
}

/// Runs a local application's native host until stopped, including across
/// debug requests, and reports each of its events to `callback`, which may
/// read the event only during the call.
///
/// # Safety
/// The session comes from `isoplane_session_create` and remains alive for the call.
/// The module and options stay pinned and readable. The callback and context stay alive
/// until this call returns; callbacks may be concurrent and cannot throw across FFI.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_run(
    session: *mut c_void,
    wasm: *const u8,
    wasm_length: usize,
    options: *const session::NativeSessionOptions,
    callback: extern "C" fn(*mut c_void, *const session_events::NativeEvent),
    context: *mut c_void,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session and pins the module and options.
        let (session, wasm, options) = unsafe {
            (
                record(session.cast::<session::Session>())?,
                interop::lent(wasm, wasm_length)?,
                record(options)?.read()?,
            )
        };
        session
            .run(
                wasm,
                options,
                session_events::Events::new(callback, context),
            )
            .map_err(error::Error::Session)
    })
}

/// Offers a newly published module to the running local host.
///
/// # Safety
/// The session remains alive, and the module and options remain pinned, for this call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_reload(
    session: *mut c_void,
    wasm: *const u8,
    wasm_length: usize,
    options: *const session::NativeSessionOptions,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session and pins the module and options.
        let (session, wasm, options) = unsafe {
            (
                record(session.cast::<session::Session>())?,
                interop::lent(wasm, wasm_length)?,
                record(options)?.read()?,
            )
        };
        session.reload(wasm, options).map_err(error::Error::Session)
    })
}

/// Requests cooperative shutdown; `isoplane_session_run` joins owned work before returning.
///
/// # Safety
/// The session remains alive throughout the call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_stop(session: *mut c_void) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session for this call.
        let session = unsafe { record(session.cast::<session::Session>())? };
        session.cancellation.cancel();
        Ok(())
    })
}

/// Releases a local session after its run and all other calls have returned.
///
/// # Safety
/// The pointer is the unchanged result of `isoplane_session_create`, freed once.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_free(session: *mut c_void) {
    // SAFETY: the managed owner joins the run task before transferring sole ownership.
    unsafe { drop(Box::from_raw(session.cast::<session::Session>())) };
}

/// Launches an owned build-tool process group and transfers its pipe descriptors.
///
/// # Safety
/// The settings stay pinned and readable, and `started` writable, for this call. The
/// managed owner closes every returned descriptor and joins the wait call before
/// freeing the handle.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_start(
    settings: *const process_records::NativeProcessStart,
    started: *mut process_records::NativeProcessStarted,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed caller pins the settings and the report's storage.
        let (settings, started) = unsafe { (record(settings)?.read()?, output(started)?) };
        started.write(process::start(settings)?);
        Ok(())
    })
}

/// Waits for an owned process, cleans its group before reaping the leader, and
/// writes its exit code to `exit_code`.
///
/// # Safety
/// The handle comes from process_start, remains alive, and has exactly one waiter.
/// `exit_code` is writable for the call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_wait(
    handle: *mut c_void,
    exit_code: *mut i32,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the process and the report's storage until this call finishes.
        let (process, exit_code) = unsafe {
            (
                record(handle.cast::<process::OwnedChild>())?,
                output(exit_code)?,
            )
        };
        exit_code.write(process.wait()?);
        Ok(())
    })
}

/// Terminates an unreaped owned process group, without affecting other sessions.
///
/// # Safety
/// The handle comes from process_start and remains alive for the call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_stop(handle: *mut c_void) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the process during cancellation.
        let process = unsafe { record(handle.cast::<process::OwnedChild>())? };
        process.terminate()?;
        Ok(())
    })
}

/// Releases the process after its wait and all other calls have returned.
///
/// # Safety
/// The handle is unchanged, exclusively owned, and freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_free(handle: *mut c_void) {
    // SAFETY: the managed owner joins the waiter before transferring sole ownership.
    unsafe { drop(Box::from_raw(handle.cast::<process::OwnedChild>())) };
}

/// Reports the compiled runtimes in the per-user runtime cache at the
/// absolute UTF-8 `directory`: the cache's byte budget and each runtime's
/// size and last use. A directory that does not exist is an empty cache and
/// is not created. Release the report with `isoplane_runtime_cache_inventory_free`.
///
/// # Safety
/// The nonempty buffer points to readable memory for its length and remains
/// alive and unchanged for the call, and `inventory` is writable for the call.
/// The managed wrapper pins both.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_inventory(
    directory: *const u8,
    directory_length: usize,
    inventory: *mut runtime_cache::NativeCacheInventory,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins the supplied path and the report's storage.
        let (directory, inventory) = unsafe {
            (
                interop::lent(directory, directory_length)?,
                output(inventory)?,
            )
        };
        inventory.write(runtime_cache::inventory(directory)?.native());
        Ok(())
    })
}

/// Releases a report of `isoplane_runtime_cache_inventory`.
///
/// # Safety
/// Pass an unchanged report exactly once, after all its readers finish.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_inventory_free(
    inventory: runtime_cache::NativeCacheInventory,
) {
    // SAFETY: the caller returns the unchanged report this library allocated.
    unsafe { inventory.release() };
}

/// Removes every compiled runtime from the per-user runtime cache at the
/// absolute UTF-8 `directory`, including one a running session uses, and
/// reports the bytes removed, the runtimes that remain and why each could
/// not be removed. Release the report with `isoplane_runtime_cache_cleared_free`.
///
/// # Safety
/// As for `isoplane_runtime_cache_inventory`.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_clear(
    directory: *const u8,
    directory_length: usize,
    cleared: *mut runtime_cache::NativeCacheCleared,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins the supplied path and the report's storage.
        let (directory, cleared) = unsafe {
            (
                interop::lent(directory, directory_length)?,
                output(cleared)?,
            )
        };
        cleared.write(runtime_cache::clear(directory)?.native());
        Ok(())
    })
}

/// Releases a report of `isoplane_runtime_cache_clear`.
///
/// # Safety
/// Pass an unchanged report exactly once, after all its readers finish.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_cleared_free(
    cleared: runtime_cache::NativeCacheCleared,
) {
    // SAFETY: the caller returns the unchanged report this library allocated.
    unsafe { cleared.release() };
}

/// Compiles a shared runtime and its debug build into the runtime cache ahead
/// of the first session on them, as that session would, and reports whether
/// each had to be compiled.
///
/// # Safety
/// Both modules and the options stay pinned and readable, and `prepared`
/// writable, for this call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_prepare(
    wasm: *const u8,
    wasm_length: usize,
    debug_wasm: *const u8,
    debug_wasm_length: usize,
    options: *const runtime_cache::NativeRuntimeOptions,
    prepared: *mut local::RuntimePreparation,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins the modules, the options and the report's storage.
        unsafe {
            let wasm = interop::lent(wasm, wasm_length)?;
            let debug_wasm = interop::lent(debug_wasm, debug_wasm_length)?;
            let report = runtime_cache::prepare(wasm, debug_wasm, record(options)?)?;
            output(prepared)?.write(report);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_operation_returns_an_error_without_unwinding() {
        let result = boundary(|| panic!("test panic"));
        assert!(
            matches!(result.status, NativeStatus::InternalError),
            "a panic must be contained and reported to managed code"
        );
        // SAFETY: this test owns the unchanged result and has finished inspecting it.
        unsafe { isoplane_result_free(result) };
    }
}
