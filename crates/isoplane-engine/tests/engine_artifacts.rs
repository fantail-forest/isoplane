//! Reset and re-sealing of globals and tables, the mapped-artifact path, and
//! the artifact metadata that guards it.

mod common;

use common::*;
use isoplane_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, IsoplaneEngine};

fn echo_guest() -> Vec<u8> {
    guest(4, "", "", DONE, DONE)
}

/// A `DispatchRecord` with nothing interesting in it.
fn http_dispatch(id: u64) -> isoplane_abi::dispatch::DispatchRecord {
    use isoplane_abi::dispatch::*;
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/x".into(),
            path: "/x".into(),
            query: vec![],
            headers: vec![("host".into(), "example.test".into())],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1_700_000_000_000,
    }
}

// ---------------------------------------------------------------------------
// activation and reset
// ---------------------------------------------------------------------------

#[test]
fn reset_restores_template_contents_byte_exactly() {
    let e = test_engine();
    let t = e.load_template(&echo_guest()).expect("template");
    let image = t.memory_image_arc().to_dense();

    let mut i = activate(&e, &t);
    assert_eq!(
        i.memory(),
        image.as_slice(),
        "a fresh activation must equal the template"
    );

    for off in [0usize, 4096, 65536, 30 * 4096, 60 * 4096] {
        i.memory()[off..off + 8].copy_from_slice(&[0xAB; 8]);
    }
    assert_ne!(
        i.memory(),
        image.as_slice(),
        "the writes must have changed memory"
    );

    i.reset().expect("reset");

    assert_eq!(
        i.memory(),
        image.as_slice(),
        "reset must restore template contents byte for byte"
    );
}

#[test]
fn activation_seals_globals_and_tables_from_the_template() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
  (table (export "t") 4 4 funcref)
  (elem (i32.const 0) $marker)
  (func $marker (result i32) (i32.const 7))
  (global $counter (export "g_counter") (mut i64) (i64.const 41))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let baseline = activate(&e, &t).capture_mutable_state().expect("capture");

    let mut a = activate(&e, &t);
    a.memory()[0..8].copy_from_slice(&[9u8; 8]);
    let mut img = baseline.clone();
    img.globals = img
        .globals
        .iter()
        .map(|g| match g {
            isoplane_engine::snapshot::GlobalValue::I64(x) => {
                isoplane_engine::snapshot::GlobalValue::I64(x + 1000)
            }
            other => *other,
        })
        .collect();
    a.restore_mutable_state(&img).expect("restore");
    let mutated = a.capture_mutable_state().expect("capture");
    assert_ne!(
        format!("{:?}", mutated.globals),
        format!("{:?}", baseline.globals),
        "the restore must have changed the globals"
    );

    let mut b = activate(&e, &t);
    let fresh = b.capture_mutable_state().expect("capture");
    assert_eq!(
        format!("{:?}", fresh.globals),
        format!("{:?}", baseline.globals),
        "a fresh activation must see the template's sealed globals"
    );
    assert_eq!(
        b.memory(),
        t.memory_image_arc().to_dense(),
        "a fresh activation must see the template's memory"
    );
    assert!(
        fresh.table_mutations.is_empty(),
        "a fresh activation's table must match the template baseline"
    );

    a.reset().expect("reset");
    let after = a.capture_mutable_state().expect("capture");
    assert_eq!(
        format!("{:?}", after.globals),
        format!("{:?}", baseline.globals),
        "reset must re-seal globals from the template"
    );
}

#[test]
fn a_mapped_artifact_is_equivalent_to_the_compiled_template() {
    let e = test_engine();
    let dir = tempfile::tempdir().expect("tempdir");
    let wasm = echo_guest();
    let t = e.load_template(&wasm).expect("template");

    // SAFETY: a fresh directory this test owns, written only by this engine.
    let (a, compiled_a) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("first load");
    let (b, compiled_b) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("second load");
    assert!(
        compiled_a,
        "the first load has nothing to map and must compile"
    );
    assert!(
        !compiled_b,
        "the second load must map the artifact the first wrote"
    );

    for m in [&a, &b] {
        assert_eq!(m.hash, t.hash, "the mapped template is the same module");
        assert_eq!(
            m.abi_rev, t.abi_rev,
            "the mapped template has the same ABI rev"
        );
        assert_eq!(
            *m.memory_image_arc(),
            *t.memory_image_arc(),
            "the mapped template has the same memory image"
        );
        assert_eq!(
            m.memory_pages(),
            t.memory_pages(),
            "the mapped template has the same memory size"
        );
    }

    let mut i = activate(&e, &b);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "a mapped template dispatches like a compiled one"
    );

    let other = IsoplaneEngine::new(EngineConfig {
        spectre_mitigations: false,
        ..small_engine_config()
    })
    .expect("engine");
    let paths = e
        .precompile_to_files(&wasm, dir.path())
        .expect("precompile");
    let meta = std::fs::read(&paths.meta).expect("read meta");
    // SAFETY: the metadata was written by `e` just above.
    let err = unsafe { other.deserialize_file(&meta, &paths.cwasm) }
        .expect_err("a differently configured engine must refuse the artifact");
    assert!(
        format!("{err:#}").contains("engine"),
        "the refusal must say the engine does not match: {err:#}"
    );
}

#[test]
fn a_tampered_artifact_is_refused_before_it_is_mapped() {
    let e = test_engine();
    let dir = tempfile::tempdir().expect("tempdir");
    let wasm = echo_guest();
    let paths = e
        .precompile_to_files(&wasm, dir.path())
        .expect("precompile");

    let mut cwasm = std::fs::read(&paths.cwasm).expect("read cwasm");
    let mid = cwasm.len() / 2;
    cwasm[mid] ^= 0xFF;
    std::fs::write(&paths.cwasm, &cwasm).expect("write cwasm");

    // SAFETY: the directory is this test's own; the point is that the tamper
    // is caught before wasmtime sees the file.
    let err = unsafe { e.load_mapped(&wasm, dir.path()) }
        .expect_err("a .cwasm that does not match its metadata must be refused");
    assert!(
        format!("{err:#}").contains("does not match the hash"),
        "the refusal must say the hash failed: {err:#}"
    );
}

#[test]
fn deferred_init_units_travel_in_the_template() {
    let e = test_engine();
    let mut wasm = echo_guest();
    let mut payload = 3u32.to_le_bytes().to_vec();
    for id in [7u32, 11, 13] {
        payload.extend_from_slice(&id.to_le_bytes());
    }
    append_custom_section(&mut wasm, isoplane_engine::DEFERRED_INIT_SECTION, &payload);

    let t = e.load_template(&wasm).expect("template");
    assert_eq!(
        t.deferred_init_units(),
        &[7, 11, 13],
        "the section names the deferred units"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    // SAFETY: a fresh directory this test owns.
    let (back, _) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("mapped load");
    assert_eq!(
        back.deferred_init_units(),
        &[7, 11, 13],
        "the deferred units survive the artifact cache"
    );
}

fn append_custom_section(wasm: &mut Vec<u8>, name: &str, data: &[u8]) {
    fn leb(mut v: u32, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    let mut body = Vec::new();
    leb(name.len() as u32, &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(data);
    wasm.push(0);
    leb(body.len() as u32, wasm);
    wasm.extend_from_slice(&body);
}

fn table_guest(change: &str) -> Vec<u8> {
    guest(
        2,
        "",
        r#"
        (table (export "table") 2 3 funcref)
        (elem (i32.const 0) $first)
        (elem declare func $second)
        (func $first)
        (func $second)
    "#,
        &format!("{change} (i64.const 0)"),
        DONE,
    )
}

#[test]
fn replacing_a_non_null_function_refuses_a_lossy_checkpoint() {
    let engine = test_engine();
    let template = engine
        .load_template(&table_guest("(table.set (i32.const 0) (ref.func $second))"))
        .expect("table fixture");
    let mut instance = activate(&engine, &template);
    let outcome = instance.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the table replacement must execute"
    );
    let error = instance
        .capture_mutable_state()
        .expect_err("a changed function cannot be silently omitted");
    assert!(
        matches!(
            error.downcast_ref::<isoplane_engine::snapshot::SnapshotError>(),
            Some(isoplane_engine::snapshot::SnapshotError::TableMutation { slot: Some(0), .. })
        ),
        "the capture refusal must identify the changed table slot through a typed error"
    );
}

#[test]
fn unchanged_function_and_null_slots_capture_across_independent_stores() {
    let engine = test_engine();
    let template = engine
        .load_template(&table_guest(
            "(table.set (i32.const 0) (ref.func $first)) (table.set (i32.const 1) (ref.null func))",
        ))
        .expect("table fixture");
    for _ in 0..2 {
        let mut instance = activate(&engine, &template);
        assert_eq!(
            instance
                .dispatch(&http_dispatch(1), CpuBudget::http())
                .completion,
            Completion::Done,
            "writing the original references must execute"
        );
        assert!(
            instance.capture_mutable_state().is_ok(),
            "each store must compare against its own original function identities"
        );
    }
}

#[test]
fn growing_a_table_refuses_an_unrepresentable_checkpoint() {
    let engine = test_engine();
    let template = engine
        .load_template(&table_guest(
            "(drop (table.grow (ref.null func) (i32.const 1)))",
        ))
        .expect("table fixture");
    let mut instance = activate(&engine, &template);
    assert_eq!(
        instance
            .dispatch(&http_dispatch(1), CpuBudget::http())
            .completion,
        Completion::Done,
        "table growth must execute within the declared maximum"
    );
    let error = instance
        .capture_mutable_state()
        .expect_err("table growth cannot be silently omitted");
    assert!(
        matches!(
            error.downcast_ref::<isoplane_engine::snapshot::SnapshotError>(),
            Some(isoplane_engine::snapshot::SnapshotError::TableMutation { slot: None, .. })
        ),
        "the capture refusal must identify changed table dimensions"
    );
}

#[test]
fn changing_a_slots_nullness_refuses_a_lossy_checkpoint() {
    let engine = test_engine();
    for (change, slot) in [
        ("(table.set (i32.const 0) (ref.null func))", 0),
        ("(table.set (i32.const 1) (ref.func $first))", 1),
    ] {
        let template = engine
            .load_template(&table_guest(change))
            .expect("table fixture");
        let mut instance = activate(&engine, &template);
        assert_eq!(
            instance
                .dispatch(&http_dispatch(1), CpuBudget::http())
                .completion,
            Completion::Done,
            "the nullness change must execute"
        );
        let error = instance
            .capture_mutable_state()
            .expect_err("changed nullness must not be omitted");
        assert!(
            matches!(error.downcast_ref::<isoplane_engine::snapshot::SnapshotError>(), Some(isoplane_engine::snapshot::SnapshotError::TableMutation { slot: Some(changed), .. }) if *changed == slot),
            "the typed refusal must identify the slot whose nullness changed"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_compilation_cache_refuses_group_writable_directories() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("owned cache fixture");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o770))
        .expect("shared writer fixture");
    // SAFETY: the directory is empty and test-owned; refusal occurs before native loading.
    let result = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(directory.path().into())
    };
    assert!(
        result.is_err(),
        "a shared-writer directory must not become a native compilation authority"
    );
}

#[cfg(unix)]
#[test]
fn a_compilation_cache_refuses_symlinked_metadata_even_when_the_target_is_valid() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("owned cache");
    let cache_directory = directory.path().join("cache");
    let external = tempfile::tempdir().expect("owned external metadata");
    let wasm = echo_guest();
    let paths = engine
        .precompile_to_files(&wasm, &cache_directory)
        .expect("trusted compilation");
    let metadata = external.path().join("valid.meta");
    std::fs::rename(&paths.meta, &metadata).expect("move valid metadata");
    std::os::unix::fs::symlink(&metadata, &paths.meta).expect("symlink fixture");
    // SAFETY: all bytes were created by this test's compiler; no other process writes them.
    let cache =
        unsafe { isoplane_engine::CompilationCache::from_trusted_directory(cache_directory) }
            .expect("owned directory");
    assert!(
        cache.load(&engine, &wasm).is_err(),
        "a valid metadata target must not allow redirection outside the compiler-owned cache"
    );
}

#[test]
fn a_cached_entry_for_another_source_is_refused_before_activation() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("owned cache");
    let cache_directory = directory.path().join("cache");
    let wasm = echo_guest();
    let expected = engine
        .precompile_to_files(&wasm, &cache_directory)
        .expect("expected compilation");
    let other = engine
        .precompile_to_files(&guest(2, "", "", DONE, DONE), &cache_directory)
        .expect("other trusted compilation");
    std::fs::copy(&other.meta, &expected.meta).expect("misplaced metadata fixture");
    std::fs::copy(&other.cwasm, &expected.cwasm).expect("misplaced native fixture");
    // SAFETY: both native files came from this compiler and have no live mappings;
    // the test checks refusal of a misplaced, otherwise valid artifact pair.
    let cache =
        unsafe { isoplane_engine::CompilationCache::from_trusted_directory(cache_directory) }
            .expect("owned cache");
    assert!(
        cache.load(&engine, &wasm).is_err(),
        "matching native digest and engine metadata must not substitute another portable source"
    );
}

#[cfg(unix)]
#[test]
fn newly_compiled_cache_entries_are_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("owned cache fixture");
    let cache_directory = directory.path().join("cache");
    let paths = test_engine()
        .precompile_to_files(&echo_guest(), &cache_directory)
        .expect("compile private cache entry");
    for path in [&cache_directory, &paths.meta, &paths.cwasm] {
        let mode = std::fs::metadata(path)
            .expect("published cache path")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "new native cache paths must grant no access to group or other users: {}",
            path.display()
        );
    }
}
