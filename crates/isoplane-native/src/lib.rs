//! Native platform operations consumed by managed local development tooling.

mod error;
mod local;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod process;
#[cfg(windows)]
#[path = "process_windows.rs"]
mod process;
mod runtime_cache;
mod session;
mod session_events;

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Version of the native entry points and their report schema.
pub const ABI_VERSION: u32 = 5;

/// Outcome of a native operation.
#[repr(C)]
pub enum NativeStatus {
    /// The operation returned its report.
    Success,
    /// The supplied input could not be read or parsed.
    InvalidInput,
    /// An owned build-tool process could not be started, waited for or stopped.
    ProcessError,
    /// A native operation failed, or an unexpected panic was contained.
    InternalError,
}

/// An owned UTF-8 report or error, released once with `isoplane_result_free`.
#[repr(C)]
pub struct NativeResult {
    /// Interpretation of the payload.
    pub status: NativeStatus,
    /// Read-only payload allocated by this library.
    pub data: *mut u8,
    /// Payload length in bytes.
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

fn boundary(operation: impl FnOnce() -> Result<Vec<u8>, error::Error>) -> NativeResult {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(report)) => NativeResult::new(NativeStatus::Success, report),
        Ok(Err(error)) => {
            let status = match &error {
                error::Error::Process(_) => NativeStatus::ProcessError,
                error::Error::Buffer | error::Error::Settings(_) | error::Error::CacheDirectory => {
                    NativeStatus::InvalidInput
                }
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

unsafe fn input<'a>(data: *const u8, length: usize) -> Result<&'a [u8], error::Error> {
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() || length > isize::MAX as usize {
        return Err(error::Error::Buffer);
    }
    // SAFETY: the export's caller guarantees readable, pinned memory; the null
    // and addressable-length cases are checked above.
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

/// Releases the payload of a result returned by this library.
///
/// # Safety
/// Pass an unchanged result exactly once, after all payload readers finish.
/// The managed wrapper calls this in a finally block after copying the payload.
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
pub extern "C" fn isoplane_session_create() -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(session::Session::new())).cast()
}

/// Runs a local application's native host until stopped, including across debug requests.
///
/// # Safety
/// The session comes from `isoplane_session_create` and remains alive for the call.
/// Both input buffers stay pinned and readable. The callback and context stay alive
/// until this call returns; callbacks may be concurrent and cannot throw across FFI.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_run(
    session: *mut std::ffi::c_void,
    wasm: *const u8,
    wasm_length: usize,
    options: *const u8,
    options_length: usize,
    callback: extern "C" fn(*mut std::ffi::c_void, *const u8, usize),
    context: *mut std::ffi::c_void,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session and pins both input buffers.
        let (session, wasm, options) = unsafe {
            (
                session
                    .cast::<session::Session>()
                    .as_ref()
                    .ok_or(error::Error::Buffer)?,
                input(wasm, wasm_length)?,
                input(options, options_length)?,
            )
        };
        session
            .run(
                wasm,
                options,
                session_events::Events::new(callback, context),
            )
            .map_err(error::Error::Session)?;
        Ok(b"{}".to_vec())
    })
}

/// Offers a newly published module to the running local host.
///
/// # Safety
/// The session remains alive and both buffers remain pinned for this call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_reload(
    session: *mut std::ffi::c_void,
    wasm: *const u8,
    wasm_length: usize,
    options: *const u8,
    options_length: usize,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session and pins both input buffers.
        let (session, wasm, options) = unsafe {
            (
                session
                    .cast::<session::Session>()
                    .as_ref()
                    .ok_or(error::Error::Buffer)?,
                input(wasm, wasm_length)?,
                input(options, options_length)?,
            )
        };
        session
            .reload(wasm, options)
            .map_err(error::Error::Session)?;
        Ok(b"{}".to_vec())
    })
}

/// Requests cooperative shutdown; `isoplane_session_run` joins owned work before returning.
///
/// # Safety
/// The session remains alive throughout the call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_stop(session: *mut std::ffi::c_void) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed owner retains the session for this call.
        let session =
            unsafe { session.cast::<session::Session>().as_ref() }.ok_or(error::Error::Buffer)?;
        session.cancellation.cancel();
        Ok(b"{}".to_vec())
    })
}

/// Releases a local session after its run and all other calls have returned.
///
/// # Safety
/// The pointer is the unchanged result of `isoplane_session_create`, freed once.
#[no_mangle]
pub unsafe extern "C" fn isoplane_session_free(session: *mut std::ffi::c_void) {
    // SAFETY: the managed owner joins the run task before transferring sole ownership.
    unsafe { drop(Box::from_raw(session.cast::<session::Session>())) };
}

/// Launches an owned build-tool process group and transfers its pipe descriptors.
///
/// # Safety
/// The settings buffer is pinned and readable for this call. The managed owner
/// closes every returned descriptor and joins the wait call before freeing the handle.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_start(
    settings: *const u8,
    length: usize,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed caller pins the serialized process settings.
        let settings = unsafe { input(settings, length)? };
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            process::start(settings)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _settings = settings;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "local process ownership is supported on Linux, macOS and Windows",
            )
            .into())
        }
    })
}

/// Waits for an owned process and cleans its group before reaping the leader.
///
/// # Safety
/// The handle comes from process_start, remains alive, and has exactly one waiter.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_wait(handle: *mut std::ffi::c_void) -> NativeResult {
    boundary(|| {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            // SAFETY: the managed owner retains the process until this call finishes.
            let process = unsafe { handle.cast::<process::OwnedChild>().as_ref() }
                .ok_or(error::Error::Buffer)?;
            Ok(serde_json::json!({"exit_code":process.wait()?})
                .to_string()
                .into_bytes())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _handle = handle;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "local process ownership is supported on Linux, macOS and Windows",
            )
            .into())
        }
    })
}

/// Terminates an unreaped owned process group, without affecting other sessions.
///
/// # Safety
/// The handle comes from process_start and remains alive for the call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_stop(handle: *mut std::ffi::c_void) -> NativeResult {
    boundary(|| {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            // SAFETY: the managed owner retains the process during cancellation.
            let process = unsafe { handle.cast::<process::OwnedChild>().as_ref() }
                .ok_or(error::Error::Buffer)?;
            process.terminate()?;
            Ok(b"{}".to_vec())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _handle = handle;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "local process ownership is supported on Linux, macOS and Windows",
            )
            .into())
        }
    })
}

/// Releases the process after its wait and all other calls have returned.
///
/// # Safety
/// The handle is unchanged, exclusively owned, and freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn isoplane_process_free(handle: *mut std::ffi::c_void) {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    // SAFETY: the managed owner joins the waiter before transferring sole ownership.
    unsafe {
        drop(Box::from_raw(handle.cast::<process::OwnedChild>()))
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let _handle = handle;
}

/// Reports the compiled runtimes in the per-user runtime cache at the
/// absolute UTF-8 `directory`: the cache's byte budget and each runtime's
/// size and last use. A directory that does not exist is an empty cache and
/// is not created.
///
/// # Safety
/// The nonempty buffer points to readable memory for its length and remains
/// alive and unchanged for the call. The managed wrapper pins it.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_inventory(
    directory: *const u8,
    directory_length: usize,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins the supplied path throughout this call.
        runtime_cache::inventory(unsafe { input(directory, directory_length)? })
    })
}

/// Removes every compiled runtime from the per-user runtime cache at the
/// absolute UTF-8 `directory`, including one a running session uses, and
/// reports the bytes removed, the runtimes that remain and why each could
/// not be removed.
///
/// # Safety
/// As for `isoplane_runtime_cache_inventory`.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_cache_clear(
    directory: *const u8,
    directory_length: usize,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins the supplied path throughout this call.
        runtime_cache::clear(unsafe { input(directory, directory_length)? })
    })
}

/// Compiles a shared runtime and its debug build into the runtime cache ahead
/// of the first session on them, as that session would. The UTF-8 JSON
/// `options` give the application manifest, native library, runtime cache
/// directory, machine roots and the runtime's `runtime.json`. Reports
/// `baseline_compiled` and `debug_compiled`: whether each had to be compiled.
///
/// # Safety
/// All three buffers stay pinned and readable for this call.
#[no_mangle]
pub unsafe extern "C" fn isoplane_runtime_prepare(
    wasm: *const u8,
    wasm_length: usize,
    debug_wasm: *const u8,
    debug_wasm_length: usize,
    options: *const u8,
    options_length: usize,
) -> NativeResult {
    boundary(|| {
        // SAFETY: the managed wrapper pins all three buffers throughout this call.
        let (wasm, debug_wasm, options) = unsafe {
            (
                input(wasm, wasm_length)?,
                input(debug_wasm, debug_wasm_length)?,
                input(options, options_length)?,
            )
        };
        runtime_cache::prepare(wasm, debug_wasm, options)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_bindings_match_the_native_interface() {
        assert_eq!(
            include_str!(concat!(env!("OUT_DIR"), "/NativeMethods.g.cs")),
            include_str!("../../../tools/Isoplane.Native/NativeMethods.g.cs"),
            "regenerate bindings with ISOPLANE_GENERATE_BINDINGS=1 cargo build -p isoplane-native"
        );
    }

    #[test]
    fn invalid_buffer_descriptors_are_rejected_without_reading_memory() {
        for (pointer, length) in [
            (std::ptr::null(), 1),
            (
                std::ptr::NonNull::<u8>::dangling().as_ptr().cast_const(),
                usize::MAX,
            ),
        ] {
            // SAFETY: both descriptors fail the boundary checks before dereferencing.
            let result = unsafe { input(pointer, length) };
            assert!(
                matches!(result, Err(error::Error::Buffer)),
                "unreadable descriptors must return a managed input error"
            );
        }
    }

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
