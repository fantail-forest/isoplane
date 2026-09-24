//! Contains a failed page-in of mapped template storage that host code hits.
//!
//! Reading a file-backed page whose storage fails raises SIGBUS at the reading
//! instruction. Wasmtime traps the guest when that instruction is wasm code.
//! Anywhere else (a wasmtime libcall, a host function copying a guest buffer,
//! hibernation comparing pages) the reader is host code, which cannot unwind
//! from a signal. For a fault inside a watched range, the handler maps a zero
//! page over the failed one so the read completes, and records the fault for
//! the watch's owner to report when its operation ends. Wasmtime's handler
//! passes every fault it does not claim to the handler installed before it, so
//! this one is installed before the engine creates its first wasmtime engine.

use std::ops::Range;
use std::sync::atomic::{fence, AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// A range whose failed page-ins read as zeros and are recorded, until dropped.
pub(crate) struct Watch(&'static Slot);

/// What the owner of a watched range may do with a page mapped over a failed one.
#[derive(Clone, Copy)]
pub(crate) enum Access {
    Read,
    ReadWrite,
}

impl Watch {
    pub(crate) fn new(range: Range<usize>, access: Access) -> std::io::Result<Self> {
        install()?;
        let slot = acquire();
        slot.publish(range, matches!(access, Access::ReadWrite));
        Ok(Self(slot))
    }

    /// Whether a page-in inside the range failed since the watch began.
    pub(crate) fn faulted(&self) -> bool {
        self.0.faulted.load(Ordering::Acquire)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.publish(0..0, false);
        FREE.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.0);
    }
}

/// Installs the handler once per process, ahead of any wasmtime engine the
/// caller is about to create.
pub(crate) fn install() -> std::io::Result<()> {
    static INSTALLED: OnceLock<Result<(), i32>> = OnceLock::new();
    let installed = *INSTALLED.get_or_init(|| {
        // SAFETY: sysconf has no preconditions.
        let page_bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_bytes = usize::try_from(page_bytes).map_err(|_| libc::EINVAL)?;
        PAGE_BYTES.store(page_bytes, Ordering::Relaxed);
        // SAFETY: an all-zero sigaction is a valid value to fill in, and a
        // null new action only reads the current one.
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut previous) } != 0 {
            return Err(last_errno());
        }
        // Recorded before the handler can run, so it always has somewhere to pass
        // a fault it does not own.
        PREVIOUS.get_or_init(|| Previous(previous));
        // SAFETY: as above.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_bus_error as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER | libc::SA_ONSTACK;
        // SAFETY: the mask is owned and valid; the action points at a handler
        // that only touches process-lifetime statics and async-signal-safe calls.
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGBUS, &action, std::ptr::null_mut()) != 0 {
                return Err(last_errno());
            }
        }
        Ok(())
    });
    installed.map_err(std::io::Error::from_raw_os_error)
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

struct Previous(libc::sigaction);

// SAFETY: the action is written once before the handler is installed and only read afterwards.
unsafe impl Sync for Previous {}
// SAFETY: as above.
unsafe impl Send for Previous {}

static PREVIOUS: OnceLock<Previous> = OnceLock::new();
static PAGE_BYTES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_bus_error(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    // SAFETY: the kernel passes a valid siginfo to an SA_SIGINFO handler.
    let (code, address) = unsafe { ((*info).si_code, (*info).si_addr() as usize) };
    if code == libc::BUS_ADRERR && contain(address) {
        return;
    }
    // SAFETY: the arguments are the kernel's, passed through unchanged.
    unsafe { delegate(signal, info, context) }
}

/// Maps a zero page over `address` when a watch covers it. Async-signal-safe:
/// it reads atomics and calls mmap, nothing else.
fn contain(address: usize) -> bool {
    let page_bytes = PAGE_BYTES.load(Ordering::Relaxed);
    let mut chunk = HEAD.load(Ordering::Acquire);
    // SAFETY: published chunks are leaked, so every non-null pointer stays valid.
    while let Some(current) = unsafe { chunk.as_ref() } {
        for slot in &current.slots {
            let (range, writable) = slot.read();
            if !range.contains(&address) {
                continue;
            }
            let protection = if writable {
                libc::PROT_READ | libc::PROT_WRITE
            } else {
                libc::PROT_READ
            };
            // SAFETY: the page lies inside a range its owner mapped and keeps mapped
            // while watched; replacing it changes contents, never ownership.
            let mapped = unsafe {
                libc::mmap(
                    (address & !(page_bytes - 1)) as *mut libc::c_void,
                    page_bytes,
                    protection,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                    -1,
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return false;
            }
            slot.faulted.store(true, Ordering::Release);
            return true;
        }
        chunk = current.next.load(Ordering::Acquire);
    }
    false
}

/// Passes a fault on as if this handler were not installed.
unsafe fn delegate(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let Some(Previous(previous)) = PREVIOUS.get() else {
        // SAFETY: restoring the default action has no preconditions; the faulting
        // instruction then re-runs under it.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
        return;
    };
    // SAFETY: the previous action came from the kernel; a handler address is called with the
    // signature its SA_SIGINFO flag declares. Restoring a default action and returning re-runs
    // the faulting instruction under it, which terminates the process as it would have.
    unsafe {
        if previous.sa_flags & libc::SA_SIGINFO != 0 {
            let handler = std::mem::transmute::<
                usize,
                extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
            >(previous.sa_sigaction);
            handler(signal, info, context);
        } else if previous.sa_sigaction == libc::SIG_DFL || previous.sa_sigaction == libc::SIG_IGN {
            libc::sigaction(signal, previous, std::ptr::null_mut());
        } else {
            let handler =
                std::mem::transmute::<usize, extern "C" fn(libc::c_int)>(previous.sa_sigaction);
            handler(signal);
        }
    }
}

const CHUNK_SLOTS: usize = 256;

/// Watches in fixed chunks that are never freed, so the handler can walk them
/// without taking a lock.
struct Chunk {
    slots: [Slot; CHUNK_SLOTS],
    next: AtomicPtr<Chunk>,
}

static HEAD: AtomicPtr<Chunk> = AtomicPtr::new(std::ptr::null_mut());
static FREE: Mutex<Vec<&'static Slot>> = Mutex::new(Vec::new());

fn acquire() -> &'static Slot {
    let mut free = FREE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(slot) = free.pop() {
        return slot;
    }
    let chunk: &'static Chunk = Box::leak(Box::new(Chunk {
        slots: std::array::from_fn(|_| Slot::default()),
        next: AtomicPtr::new(HEAD.load(Ordering::Acquire)),
    }));
    // Publication happens under the free-list lock, so chunks are pushed one at a time.
    HEAD.store(std::ptr::from_ref(chunk).cast_mut(), Ordering::Release);
    free.extend(chunk.slots[1..].iter());
    &chunk.slots[0]
}

/// One watched range, written under a sequence lock so the handler reads it
/// consistently without blocking: the sequence is odd while it changes.
#[derive(Default)]
struct Slot {
    sequence: AtomicU64,
    start: AtomicUsize,
    end: AtomicUsize,
    writable: AtomicBool,
    faulted: AtomicBool,
}

impl Slot {
    fn publish(&self, range: Range<usize>, writable: bool) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        self.sequence.store(sequence + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.start.store(range.start, Ordering::Relaxed);
        self.end.store(range.end, Ordering::Relaxed);
        self.writable.store(writable, Ordering::Relaxed);
        self.faulted.store(false, Ordering::Relaxed);
        self.sequence.store(sequence + 2, Ordering::Release);
    }

    fn read(&self) -> (Range<usize>, bool) {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let range = self.start.load(Ordering::Relaxed)..self.end.load(Ordering::Relaxed);
            let writable = self.writable.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return (range, writable);
            }
        }
    }
}
