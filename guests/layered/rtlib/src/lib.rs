//! The **shared runtime layer** of spec.md §4.6, in the smallest form that can
//! actually be built and measured today.
//!
//! §4.6 says the platform publishes a pinned runtime component
//! (`wisp:dotnet-runtime@<major>.<minor>`) — GC, scheduler, wake machinery,
//! core BCL — AOT-compiled once per engine version per µarch and
//! pre-initialized once per machine into a base template, with tenant modules
//! compiling against its exported ABI and linking at instantiation.
//!
//! We cannot build that for .NET (see `docs/layered-runtime.md` §5). What we
//! can build is the same *mechanism* with a Rust runtime layer: this crate is
//! the runtime's body — an allocator, the `wisp:platform` dispatch codec, and
//! a library surface (JSON, regex, hashing, codecs, compression, text,
//! formatting, collections) chosen to be big enough and diverse enough that
//! "each tenant reaches a different subset of it" is true, which is the
//! property the whole §4.6 trade turns on.
//!
//! It is compiled two ways, and the difference between them is the experiment:
//!
//! * with `--features export-abi` into `guests/layered/base` — every entry
//!   point is a `#[no_mangle]` root, so LTO can trim nothing. This is §4.6's
//!   "the shared runtime ships un-trimmed".
//! * without it, as an ordinary rlib linked into a self-contained app, where
//!   fat LTO trims it to the subset that app reaches. This is §4.2's
//!   whole-program trimming, i.e. what NativeAOT does and what
//!   `docs/dotnet-runtime-sharing.md` measured the consequences of.
//!
//! **This is a Rust demonstration of the mechanism. It is not a .NET result.**

#![allow(clippy::missing_safety_doc)]

pub mod abi;
pub mod heap;
