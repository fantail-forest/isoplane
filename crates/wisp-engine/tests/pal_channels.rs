//! **The no-new-channels rule, as a test rather than a comment.**
//!
//! No WASI import may introduce a capability or information channel — time,
//! entropy, or otherwise — that is not already present, **at the same
//! fidelity**, in `wisp:platform`.
//!
//! A whitelist that admits `wasi:clocks/monotonic-clock` is only safe if the
//! thing behind it is the platform's coarse frozen clock and not the host's
//! real one. That is a claim about *behaviour*, so it is checked by running
//! wasm through the real adapter and reading what the guest actually got —
//! not by reading the delegation table and believing it.
//!
//! Each rule below is stated **once**, as a predicate, and then applied
//! twice: to the adapter (it must hold) and to a deliberately wrong
//! implementation (it must fail). The second half is not optional — this
//! project has already shipped harness bugs that both erred in the flattering
//! direction, and a rule whose test has never failed is a rule nobody has
//! tested.

use std::sync::atomic::{AtomicU64, Ordering};

use wasmtime::{Engine, Instance, Linker, Module, Store};
use wisp_abi::pal::CLOCK_QUANTUM_NS;
use wisp_engine::pal::{self, PalSource};

// ---------------------------------------------------------------------------
// the host side: exactly the two `wisp:platform` capabilities, no more
// ---------------------------------------------------------------------------

/// Controlled clock and distinct byte sequences for exercising the PAL
/// adapters. Tests advance the clock explicitly and count source calls;
/// this fixture does not model production entropy generation.
struct Host {
    slice_ms: u64,
    /// Every reach into the source. If the adapter ever cached a draw this
    /// stops tracking the guest's call count, which is the whole hazard.
    entropy_calls: usize,
    source_state: u64,
}

impl Host {
    fn new(slice_ms: u64) -> Self {
        Host {
            slice_ms,
            entropy_calls: 0,
            source_state: 0x243F_6A88_85A3_08D3,
        }
    }
}

impl PalSource for Host {
    fn slice_clock_ms(&mut self) -> u64 {
        self.slice_ms
    }
    fn entropy_fill(&mut self, out: &mut [u8]) -> wasmtime::Result<()> {
        self.entropy_calls += 1;
        for b in out.iter_mut() {
            self.source_state = self
                .source_state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (self.source_state >> 33) as u8;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// the guest: a module that imports the delegated set and calls it
// ---------------------------------------------------------------------------

/// Shaped like what NativeAOT-LLVM emits for these functions: the canonical
/// ABI's callee-allocates convention (`cabi_realloc`) and caller-supplied
/// return areas, so the adapter under test is the same code path a real .NET
/// module drives.
const GUEST: &str = r#"(module
  (import "wasi:clocks/monotonic-clock@0.2.0" "now"        (func $now (result i64)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "resolution" (func $res (result i64)))
  (import "wasi:random/random@0.2.0" "get-random-bytes"    (func $rand (param i64 i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment"   (func $env (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))

  (memory (export "memory") 4)
  (global $bump (mut i32) (i32.const 4096))

  ;; callee-allocates, as the canonical ABI requires: the bytes land in guest
  ;; memory through guest code, and no host pointer crosses the boundary.
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (global.get $bump) (local.get 3)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  ;; Burn guest cycles. Real time passes here; the delegated clock must not
  ;; notice, which is exactly what "frozen for the slice" means.
  (func $spin (local $i i32)
    (local.set $i (i32.const 200000))
    (block $done
      (loop $l
        (br_if $done (i32.eqz (local.get $i)))
        (local.set $i (i32.sub (local.get $i) (i32.const 1)))
        (br $l))))

  (func (export "now_twice") (result i64 i64)
    (call $now) (call $spin) (call $now))
  (func (export "resolution") (result i64) (call $res))
  (func (export "draw") (param i64 i32) (call $rand (local.get 0) (local.get 1)))
  (func (export "environ") (param i32) (call $env (local.get 0)))
  (func (export "stat") (param i32 i32) (call $stat (local.get 0) (local.get 1)))
)"#;

fn engine() -> Engine {
    Engine::default()
}

fn module(e: &Engine, wat: &str) -> Module {
    let wasm = wat::parse_str(wat).expect("fixture must assemble");
    Module::new(e, wasm).expect("fixture must compile")
}

/// Instantiate `GUEST` against **the real adapter** (`pal::link`).
fn with_adapter(slice_ms: u64) -> (Store<Host>, Instance) {
    let e = engine();
    let m = module(&e, GUEST);
    let mut linker: Linker<Host> = Linker::new(&e);
    let composed = pal::link(&mut linker, &m).expect("adapter must compose");
    // Sanity: the adapter composed every non-`wisp` import, i.e. nothing fell
    // through to a default.
    assert_eq!(composed.len(), 5);
    let mut store = Store::new(&e, Host::new(slice_ms));
    let instance = linker.instantiate(&mut store, &m).expect("instantiate");
    (store, instance)
}

fn read(store: &mut Store<Host>, instance: &Instance, ptr: usize, len: usize) -> Vec<u8> {
    let mem = instance.get_memory(&mut *store, "memory").unwrap();
    let mut buf = vec![0u8; len];
    mem.read(&*store, ptr, &mut buf).unwrap();
    buf
}

/// The `(ptr, len)` pair a canonical-ABI list lowers into a return area.
fn read_list(store: &mut Store<Host>, instance: &Instance, retptr: usize) -> (u32, u32) {
    let b = read(store, instance, retptr, 8);
    (
        u32::from_le_bytes(b[0..4].try_into().unwrap()),
        u32::from_le_bytes(b[4..8].try_into().unwrap()),
    )
}

// ---------------------------------------------------------------------------
// rule 1 — the clock, and the measurement primitive it must not be.
// ---------------------------------------------------------------------------

/// **The no-new-channels predicate for `monotonic-clock::now`.**
///
/// Guests get no fine clock, because it is the measurement primitive most
/// Spectre-class attacks need; the fidelity that IS offered is 1 ms, frozen
/// intra-slice. A delegation is channel-equivalent only
/// if it delivers exactly that and reports exactly that.
///
/// `readings` is a list of `(first, second)` pairs taken inside ONE dispatch
/// slice, with guest computation in between.
fn clock_adds_no_channel(readings: &[(i64, i64)], reported_resolution: i64) -> Result<(), String> {
    if reported_resolution != CLOCK_QUANTUM_NS {
        return Err(format!(
            "resolution() reports {reported_resolution} ns; the delegated clock delivers \
             {CLOCK_QUANTUM_NS} ns, and a reported fidelity finer than the delivered one is \
             an invitation to build a spin-until-tick timer"
        ));
    }
    for (a, b) in readings {
        if a % CLOCK_QUANTUM_NS != 0 || b % CLOCK_QUANTUM_NS != 0 {
            return Err(format!(
                "now() returned {a}/{b} ns, which is not a whole multiple of the coarse \
                 clock's {CLOCK_QUANTUM_NS} ns quantum — sub-millisecond bits are a finer \
                 timer than `clock_now_coarse` offers"
            ));
        }
        if a != b {
            return Err(format!(
                "two calls inside one dispatch slice returned {a} then {b}; the coarse clock \
                 is FROZEN intra-slice, so a guest that can see it move has a \
                 stopwatch"
            ));
        }
    }
    Ok(())
}

#[test]
fn the_delegated_monotonic_clock_adds_no_channel() {
    let mut readings = Vec::new();
    let mut resolution = 0;
    // A spread of slice values, so "always returns 0" cannot pass.
    for slice_ms in [0u64, 1, 999, 1_700_000_000_000] {
        let (mut store, instance) = with_adapter(slice_ms);
        let twice = instance
            .get_typed_func::<(), (i64, i64)>(&mut store, "now_twice")
            .unwrap();
        let (a, b) = twice.call(&mut store, ()).unwrap();
        assert_eq!(
            a,
            (slice_ms as i64) * CLOCK_QUANTUM_NS,
            "the delegated clock must be the coarse clock, widened to ns"
        );
        readings.push((a, b));
        resolution = instance
            .get_typed_func::<(), i64>(&mut store, "resolution")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
    }
    println!("readings (ns): {readings:?}, resolution {resolution} ns");
    clock_adds_no_channel(&readings, resolution).expect("the adapter must add no time channel");
}

/// The clock is frozen, not stopped: a new slice moves it. Without this, an
/// adapter that returned a constant would pass every test above and be
/// useless — and "useless" is how a runtime ends up spinning forever waiting
/// for a tick.
#[test]
fn the_delegated_clock_is_frozen_within_a_slice_but_advances_between_them() {
    let (mut store, instance) = with_adapter(1_000);
    let now = instance
        .get_typed_func::<(), (i64, i64)>(&mut store, "now_twice")
        .unwrap();

    let (a, b) = now.call(&mut store, ()).unwrap();
    assert_eq!(a, b, "frozen within the slice");

    // What the cell does at a wake boundary: the clock advances only at
    // host-call and wake boundaries.
    store.data_mut().slice_ms = 1_007;
    let (c, d) = now.call(&mut store, ()).unwrap();
    assert_eq!(c, d, "still frozen inside the new slice");
    assert_eq!(
        c,
        1_007 * CLOCK_QUANTUM_NS,
        "and it advanced with the slice"
    );
    assert!(c > a);
}

/// **Negative control.** The same predicate, applied to a delegation
/// that leaks the host's real resolution. It MUST fail — if it does not, the
/// check above proves nothing.
///
/// This is the exact mistake the rule exists to catch: `monotonic-clock::now`
/// is *supposed* to be nanoseconds, so the "obvious" adapter reads a real
/// clock, and every functional test still passes while the measurement
/// primitive is quietly handed back.
#[test]
fn negative_control_a_nanosecond_clock_fails_the_no_new_channels_check() {
    let e = engine();
    let m = module(&e, GUEST);
    let mut linker: Linker<Host> = Linker::new(&e);

    static TICK: AtomicU64 = AtomicU64::new(1_700_000_000_000_000_000);
    linker
        .func_wrap(
            "wasi:clocks/monotonic-clock@0.2.0",
            "now",
            |_: wasmtime::Caller<'_, Host>| {
                // A real monotonic clock: advances on every read, nanosecond bits
                // intact. Exactly what wasmtime-wasi would give you.
                TICK.fetch_add(37_913, Ordering::Relaxed) as i64
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "wasi:clocks/monotonic-clock@0.2.0",
            "resolution",
            |_: wasmtime::Caller<'_, Host>| 1i64,
        )
        .unwrap();
    // The rest of the imports are irrelevant to this control; trap them.
    for (module_name, func) in [
        ("wasi:random/random@0.2.0", "get-random-bytes"),
        ("wasi:cli/environment@0.2.0", "get-environment"),
        ("wasi:filesystem/types@0.2.0", "[method]descriptor.stat"),
    ] {
        let ty = m
            .imports()
            .find(|i| i.module() == module_name && i.name() == func)
            .and_then(|i| match i.ty() {
                wasmtime::ExternType::Func(t) => Some(t),
                _ => None,
            })
            .unwrap();
        linker
            .func_new(module_name, func, ty, |_, _, _| {
                Err(wasmtime::Error::msg("unused"))
            })
            .unwrap();
    }

    let mut store = Store::new(&e, Host::new(1_000));
    let instance = linker.instantiate(&mut store, &m).unwrap();
    let (a, b) = instance
        .get_typed_func::<(), (i64, i64)>(&mut store, "now_twice")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    let resolution = instance
        .get_typed_func::<(), i64>(&mut store, "resolution")
        .unwrap()
        .call(&mut store, ())
        .unwrap();

    let verdict = clock_adds_no_channel(&[(a, b)], resolution);
    println!("negative control verdict: {verdict:?}");
    assert!(
        verdict.is_err(),
        "a nanosecond-resolution monotonic clock MUST fail the no-new-channels check; if it \
         passes, the check is theatre"
    );
    // And it fails for the right reason first: the fidelity it advertises.
    assert!(verdict.unwrap_err().contains("resolution()"));
}

/// The other half of the negative control: quantized but **not frozen**. This
/// one advertises 1 ms and still hands the guest a stopwatch, because a
/// counter that ticks per call is a counting thread by another name.
#[test]
fn negative_control_a_quantized_but_unfrozen_clock_fails_too() {
    let unfrozen = [(1_000 * CLOCK_QUANTUM_NS, 1_001 * CLOCK_QUANTUM_NS)];
    let verdict = clock_adds_no_channel(&unfrozen, CLOCK_QUANTUM_NS);
    println!("negative control verdict: {verdict:?}");
    assert!(
        verdict.is_err(),
        "an unfrozen coarse clock is still a stopwatch"
    );
    assert!(verdict.unwrap_err().contains("FROZEN"));
}

// ---------------------------------------------------------------------------
// rule 2 — forwarding entropy requests to the supplied source.
// ---------------------------------------------------------------------------

/// The deterministic fixture supplies a distinct response to every call.
/// Observing those responses checks adapter forwarding, not the randomness
/// or uniqueness of values captured in guest memory.
fn adapter_preserves_fixture_draws(draws: &[Vec<u8>], source_calls: usize) -> Result<(), String> {
    if source_calls != draws.len() {
        return Err(format!(
            "{} guest draws reached the fixture source {source_calls} time(s); expected one PER CALL",
            draws.len()
        ));
    }
    for i in 0..draws.len() {
        for j in (i + 1)..draws.len() {
            if draws[i] == draws[j] {
                return Err(format!(
                    "draws {i} and {j} repeat ({:02x?}) despite distinct fixture responses",
                    &draws[i][..draws[i].len().min(8)]
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn each_entropy_request_reaches_the_source() {
    let (mut store, instance) = with_adapter(1_000);
    let draw = instance
        .get_typed_func::<(i64, i32), ()>(&mut store, "draw")
        .unwrap();

    let mut draws = Vec::new();
    // Same length, same slice, same instance: if anything were cached, these
    // would come back equal.
    for _ in 0..4 {
        draw.call(&mut store, (32, 64)).unwrap();
        let (ptr, len) = read_list(&mut store, &instance, 64);
        assert_eq!(len, 32);
        draws.push(read(&mut store, &instance, ptr as usize, len as usize));
    }
    let calls = store.data().entropy_calls;
    adapter_preserves_fixture_draws(&draws, calls)
        .expect("the adapter must preserve each fixture response");
}

/// A zero-length draw is legal and must not allocate or trap — `arc4random_buf`
/// reaches this path during .NET startup and wasi-libc turns a failure into
/// `_Exit`.
#[test]
fn a_zero_length_draw_is_the_empty_list() {
    let (mut store, instance) = with_adapter(1_000);
    instance
        .get_typed_func::<(i64, i32), ()>(&mut store, "draw")
        .unwrap()
        .call(&mut store, (0, 64))
        .unwrap();
    assert_eq!(read_list(&mut store, &instance, 64), (0, 0));
}

/// A deliberately cached fixture draw must fail the same predicate.
#[test]
fn negative_control_cached_or_repeated_fixture_draws_fail() {
    let cached = vec![vec![0xab; 32], vec![0xab; 32], vec![0xab; 32]];
    let verdict = adapter_preserves_fixture_draws(&cached, 1);
    println!("negative control verdict: {verdict:?}");
    assert!(
        verdict.is_err(),
        "a cached draw must fail the forwarding check"
    );
    assert!(verdict.unwrap_err().contains("PER CALL"));

    // And the subtler variant: the host is reached per call, but the source
    // repeats one fixture response instead of forwarding distinct responses.
    let repeated = vec![vec![7u8; 16], vec![7u8; 16]];
    assert!(adapter_preserves_fixture_draws(&repeated, 2).is_err());
}

// ---------------------------------------------------------------------------
// rule 3 — the environment.
// ---------------------------------------------------------------------------

/// **The no-new-channels predicate for `cli/environment::get-environment`.**
/// There is no ambient environment. Any non-empty answer is a channel
/// from the deploy-time configuration into the guest that the capability
/// manifest does not describe.
fn environment_adds_no_channel(ptr: u32, len: u32) -> Result<(), String> {
    if len != 0 {
        return Err(format!(
            "get-environment returned a list of {len} entries at {ptr:#x}; there \
             is no ambient environment, so the only channel-equivalent answer is empty"
        ));
    }
    Ok(())
}

#[test]
fn the_delegated_environment_adds_no_channel() {
    let (mut store, instance) = with_adapter(1_000);
    // Poison the return area first: an adapter that writes nothing would
    // otherwise "pass" on whatever happened to be zero.
    let mem = instance.get_memory(&mut store, "memory").unwrap();
    mem.write(&mut store, 64, &[0xff; 16]).unwrap();

    instance
        .get_typed_func::<i32, ()>(&mut store, "environ")
        .unwrap()
        .call(&mut store, 64)
        .unwrap();

    let (ptr, len) = read_list(&mut store, &instance, 64);
    println!("get-environment -> (ptr {ptr}, len {len})");
    environment_adds_no_channel(ptr, len).expect("the environment must be empty");
}

/// **Negative control.**
#[test]
fn negative_control_a_populated_environment_fails_the_no_new_channels_check() {
    let verdict = environment_adds_no_channel(0x1000, 3);
    println!("negative control verdict: {verdict:?}");
    assert!(verdict.is_err(), "an ambient environment MUST fail");
}

// ---------------------------------------------------------------------------
// rule 4 — the remainder. A declaration that is not a capability.
// ---------------------------------------------------------------------------

/// The point of a trap-on-call stub: the import *links* (so V1 passes and the
/// module deploys) and the capability is *absent* (so calling it fails the
/// dispatch). If it returned zeros instead, a guest would proceed as
/// though a filesystem existed, which is worse than either alternative.
#[test]
fn a_trap_on_call_stub_links_and_then_traps_naming_itself() {
    let (mut store, instance) = with_adapter(1_000);
    // It linked: instantiation above succeeded with the import present.
    let stat = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, "stat")
        .unwrap();
    let err = stat
        .call(&mut store, (0, 64))
        .expect_err("a denied capability must trap");
    let msg = format!("{err:?}");
    println!("trap: {msg}");
    assert!(msg.contains("wasi:filesystem/types@0.2.0::[method]descriptor.stat"));
    assert!(msg.contains("trap-on-call"));
    assert!(
        msg.contains("no filesystem"),
        "the trap must say why, not just that"
    );
}

/// **Negative control** for the trap rule: a stub that silently returns
/// instead of trapping would leave the return area as whatever was there, and
/// the guest would read a fabricated `descriptor-stat`. Assert the shape of
/// the check itself — a call that *succeeds* must be a test failure.
#[test]
fn negative_control_a_silent_stub_would_not_be_detected_as_a_trap() {
    let e = engine();
    let m = module(&e, GUEST);
    let mut linker: Linker<Host> = Linker::new(&e);
    pal::link(&mut linker, &m).unwrap();
    // Replace nothing; instead, verify that "call succeeded" is the failure
    // condition by constructing the silent stub in a second linker.
    let mut silent: Linker<Host> = Linker::new(&e);
    for import in m.imports() {
        let wasmtime::ExternType::Func(ty) = import.ty() else {
            continue;
        };
        let results: Vec<wasmtime::Val> = ty
            .results()
            .map(|t| match t {
                wasmtime::ValType::I32 => wasmtime::Val::I32(0),
                wasmtime::ValType::I64 => wasmtime::Val::I64(0),
                other => panic!("unexpected result type {other:?}"),
            })
            .collect();
        silent
            .func_new(import.module(), import.name(), ty, move |_, _, out| {
                out.clone_from_slice(&results);
                Ok(())
            })
            .unwrap();
    }
    let mut store = Store::new(&e, Host::new(1_000));
    let instance = silent.instantiate(&mut store, &m).unwrap();
    let outcome = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, "stat")
        .unwrap()
        .call(&mut store, (0, 64));
    println!("silent stub outcome: {outcome:?}");
    assert!(
        outcome.is_ok(),
        "the control is only meaningful if a silent stub really does succeed — which is why \
         the adapter must not build one"
    );
}

// ---------------------------------------------------------------------------
// the table, against the artifacts it was derived from
// ---------------------------------------------------------------------------

/// The adapter refuses to compose an import outside the whitelist, so a bug
/// that let one past the validator still cannot become a linked capability.
/// Same rule, two places, one definition.
#[test]
fn the_adapter_refuses_an_import_outside_the_whitelist() {
    let e = engine();
    let m = module(
        &e,
        r#"(module
             (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-connect"
                     (func (param i32 i32)))
             (memory (export "memory") 1))"#,
    );
    let mut linker: Linker<Host> = Linker::new(&e);
    let err = pal::link(&mut linker, &m).expect_err("must refuse");
    println!("{err}");
    assert!(format!("{err}").contains("not in the WASI PAL whitelist"));
}
