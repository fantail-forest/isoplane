use std::collections::BTreeMap;
use std::ffi::c_void;

use crate::interop::{NativeSetting, NativeText};

/// The build-tool process to start and own.
pub(crate) struct Start {
    pub executable: String,
    pub arguments: Vec<String>,
    pub directory: String,
    /// The child's whole environment; it inherits nothing.
    pub environment: BTreeMap<String, String>,
    pub capture_output: bool,
    pub capture_errors: bool,
    /// How long descendants may outlive the leader before its group is killed.
    pub cleanup_grace_ms: u64,
}

/// The build-tool process to start and own, as the managed owner lends it for the call.
#[repr(C)]
pub struct NativeProcessStart {
    pub executable: NativeText,
    pub arguments: *const NativeText,
    pub arguments_length: usize,
    pub directory: NativeText,
    /// The child's whole environment; it inherits nothing.
    pub environment: *const NativeSetting,
    pub environment_length: usize,
    pub capture_output: bool,
    pub capture_errors: bool,
    /// How long descendants may outlive the leader before its group is killed.
    pub cleanup_grace_ms: u64,
}

impl NativeProcessStart {
    /// # Safety
    /// Every text and both arrays are readable for their lengths for this call.
    pub(crate) unsafe fn read(&self) -> Result<Start, crate::error::Error> {
        // SAFETY: forwarded from the caller.
        unsafe {
            Ok(Start {
                executable: self.executable.read()?.into(),
                arguments: crate::interop::lent(self.arguments, self.arguments_length)?
                    .iter()
                    .map(|argument| argument.read().map(String::from))
                    .collect::<Result<_, _>>()?,
                directory: self.directory.read()?.into(),
                environment: crate::interop::settings(self.environment, self.environment_length)?,
                capture_output: self.capture_output,
                capture_errors: self.capture_errors,
                cleanup_grace_ms: self.cleanup_grace_ms,
            })
        }
    }
}

/// A started process, whose pipe ends the managed owner now closes.
#[repr(C)]
pub struct NativeProcessStarted {
    /// The owner to pass to the wait, stop and free entry points.
    pub handle: *mut c_void,
    pub pid: u32,
    /// The operating system's descriptor or handle for the child's standard input.
    pub stdin: i64,
    /// The descriptor or handle for captured standard output, or -1 when it is not captured.
    pub stdout: i64,
    /// The descriptor or handle for captured standard error, or -1 when it is not captured.
    pub stderr: i64,
}
