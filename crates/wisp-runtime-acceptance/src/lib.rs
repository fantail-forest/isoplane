//! Acceptance tests for the source-built guest runtime live in `tests/`; the
//! runtime under test is the pack `guest-runtime/build.sh` publishes, and
//! every test drives the acceptance guest through the real host reactor.
#![forbid(unsafe_code)]
