mod common;

use common::*;
use isoplane_engine::{Completion, EngineConfig, InstanceOps, IsoplaneEngine, OccupancyBudget};

fn echo_guest() -> Vec<u8> {
    guest(4, "", "", DONE, DONE)
}

#[test]
fn active_instances_keep_their_compilation_storage_leased() {
    use isoplane_cache::{Cache, CacheBytes, CacheKey};
    let root = tempfile::tempdir().expect("isolated cache");
    let cache = Cache::new(root.path().into(), CacheBytes(0)).expect("cache");
    let lease = std::sync::Arc::new(cache.lease(CacheKey([1; 32])).expect("lease"));
    let directory = lease.directory().to_path_buf();
    let engine = test_engine();
    // SAFETY: this test owns the directory and only the compiler populates it.
    let compilation =
        unsafe { isoplane_engine::CompilationCache::from_trusted_directory(directory.clone()) }
            .expect("compilation cache");
    let mut template = compilation
        .load(&engine, &echo_guest())
        .expect("compile")
        .template;
    template.retain_storage(lease);
    let instance = activate(&engine, &template);
    drop(template);
    cache.prune().expect("prune active cache");
    assert!(
        directory.exists(),
        "an instance must retain the backing files after its template owner exits"
    );
    drop(instance);
    assert!(
        !directory.exists(),
        "the last instance must release storage and trigger over-budget eviction"
    );
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
        }),
        cancel_future: 0,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 1_700_000_000_000,
    }
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
}

#[test]
fn a_mapped_artifact_is_equivalent_to_the_compiled_template() {
    let e = test_engine();
    let dir = tempfile::tempdir().expect("tempdir");
    let wasm = echo_guest();
    let t = e.load_template(&wasm).expect("template");

    // SAFETY: a fresh directory this test owns, written only by this engine.
    let first = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("first load");
    // SAFETY: as above; the directory now holds what the first load wrote.
    let second = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("second load");
    assert_eq!(
        first.compilation,
        isoplane_engine::Compilation::Compiled,
        "the first load has nothing to map and must compile"
    );
    assert_eq!(
        second.compilation,
        isoplane_engine::Compilation::Reused,
        "the second load must map the artifact the first wrote"
    );
    let (a, b) = (first.template, second.template);

    for m in [&a, &b] {
        assert_eq!(m.hash, t.hash, "the mapped template is the same module");
        assert_eq!(
            m.abi_rev, t.abi_rev,
            "the mapped template has the same ABI rev"
        );
        assert_eq!(
            m.memory_image_arc().to_dense(),
            t.memory_image_arc().to_dense(),
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
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "a mapped template dispatches like a compiled one"
    );

    let other = IsoplaneEngine::new(EngineConfig {
        guard_size_bytes: 4 << 20,
        ..small_engine_config()
    })
    .expect("engine");
    let paths = e
        .precompile_to_files(&wasm, dir.path())
        .expect("precompile");
    let meta = std::fs::read(&paths.meta).expect("read meta");
    // SAFETY: the metadata was written by `e` just above.
    let err = unsafe { other.deserialize_file(&meta, &paths.cwasm) }
        .err()
        .expect("a differently configured engine must refuse the artifact");
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
        .err()
        .expect("a .cwasm that does not match its metadata must be refused");
    assert!(
        format!("{err:#}").contains("does not match the hash"),
        "the refusal must say the hash failed: {err:#}"
    );
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
    let outcome = instance.dispatch(&http_dispatch(1), OccupancyBudget::default());
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
                .dispatch(&http_dispatch(1), OccupancyBudget::default())
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
            .dispatch(&http_dispatch(1), OccupancyBudget::default())
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
                .dispatch(&http_dispatch(1), OccupancyBudget::default())
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
    let directory = tempfile::tempdir().expect("owned cache fixture");
    let _shared = isoplane_test_support::TemporaryPermissions::mode(directory.path(), 0o770)
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
