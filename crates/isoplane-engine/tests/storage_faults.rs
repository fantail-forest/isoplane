//! Failed storage under a mapped template ends the operation that reads it,
//! never the process.
#![cfg(target_os = "linux")]
mod common;
#[path = "common/dispatch.rs"]
mod dispatch;

use std::path::Path;

use common::*;
use dispatch::http_dispatch;
use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{
    Completion, InstanceOps, IsoplaneEngine, OccupancyBudget, Template, TemplateStorageFailed,
    TrapKind,
};
use isoplane_test_support::mapped_backing::truncate;
use isoplane_test_support::ChildTest;

fn identity() -> PreparedIdentity {
    PreparedIdentity {
        input_hash: [1; 32],
        runtime_profile: [2; 32],
        initialization_hash: [3; 32],
    }
}

/// Maps a template published at `path`.
fn load(engine: &IsoplaneEngine, path: &Path) -> Template {
    // SAFETY: these tests change published files only to observe that the
    // resulting faults are contained.
    unsafe { engine.load_prepared(path, identity(), &[]) }.expect("map the published template")
}

/// Publishes at `path` a layered template whose initializer wrote the page
/// its `isoplane_gc_hint` reads.
fn publish_layer(engine: &IsoplaneEngine, path: &Path) {
    let wasm = guest(
        2,
        "",
        r#"
          (func (export "isoplane_initialize") (result i64)
            (i32.store8 (i32.const 100) (i32.const 42))
            (i64.const 0))
          (func (export "isoplane_gc_hint") (param i32)
            (drop (i32.load8_u (i32.const 100))))
        "#,
        DONE,
        DONE,
    );
    let source = engine.load_layer_source(&wasm).expect("layer source");
    let mut instance = activate(engine, &source);
    assert_eq!(
        instance
            .run_initialize(OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the initializer must complete before capture"
    );
    engine
        .prepare_initialized_template(
            &source,
            &mut instance,
            identity(),
            Default::default(),
            vec![],
        )
        .expect("capture the layer")
        .publish(path)
        .expect("publish the layer");
}

/// A mapped layer, an instance of it, and its baseline truncated beneath both.
fn failed_layer(
    engine: &IsoplaneEngine,
    directory: &Path,
) -> (Template, isoplane_engine::Instance) {
    let path = directory.join("layer");
    publish_layer(engine, &path);
    let template = load(engine, &path);
    let instance = activate(engine, &template);
    truncate(&path.join("baseline.bin"));
    (template, instance)
}

#[test]
fn a_guest_reading_failed_template_storage_traps_as_a_storage_fault() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (_template, mut instance) = failed_layer(&engine, directory.path());
    let outcome = instance.gc_hint(0);
    assert_eq!(
        outcome.trap.map(|trap| trap.kind),
        Some(TrapKind::StorageFault),
        "a guest read of failed storage must fail its own call, as the platform's fault"
    );
}

#[test]
fn host_code_writing_into_failed_template_storage_fails_the_dispatch() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (_template, mut instance) = failed_layer(&engine, directory.path());
    let outcome = instance.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        outcome.trap.map(|trap| trap.kind),
        Some(TrapKind::StorageFault),
        "copying a dispatch into failed storage must fail that dispatch, not the process"
    );
}

#[test]
fn a_template_whose_storage_failed_refuses_new_activations() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (template, mut instance) = failed_layer(&engine, directory.path());
    instance.gc_hint(0);
    let refused = engine
        .activate(&template, Box::new(TestBindings::new(Recorder::default())))
        .expect_err("a template whose storage failed must not activate again");
    assert!(
        refused.is::<TemplateStorageFailed>(),
        "the refusal must say the template's storage failed: {refused:#}"
    );
}

#[test]
fn other_templates_keep_serving_after_one_templates_storage_fails() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (_template, mut instance) = failed_layer(&engine, directory.path());
    instance.dispatch(&http_dispatch(1), OccupancyBudget::default());
    let healthy = engine
        .load_template(&guest(1, "", "", DONE, DONE))
        .expect("another template");
    assert_eq!(
        activate(&engine, &healthy)
            .dispatch(&http_dispatch(2), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "a storage fault in one template must leave the engine serving others"
    );
}

#[test]
fn comparing_a_page_with_a_failed_baseline_marks_its_template_failed() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (template, _instance) = failed_layer(&engine, directory.path());
    template
        .memory_image_arc()
        .page_eq(0, &[0; isoplane_engine::image::PAGE]);
    assert!(
        template.storage_failed(),
        "hibernation must be able to tell its comparison read failed storage"
    );
}

#[test]
fn an_instance_whose_memory_read_failed_storage_reports_it() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let (_template, mut instance) = failed_layer(&engine, directory.path());
    std::hint::black_box(
        instance
            .memory()
            .iter()
            .map(|&byte| u64::from(byte))
            .sum::<u64>(),
    );
    assert!(
        instance.storage_failed(),
        "hibernation must be able to tell its capture read failed storage"
    );
}

#[test]
fn a_mapped_template_keeps_running_after_its_native_payload_is_truncated() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("layer");
    publish_layer(&engine, &path);
    let template = load(&engine, &path);
    truncate(&path.join("template.cwasm"));
    assert_eq!(
        activate(&engine, &template)
            .dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "compiled code must not depend on its payload file once verified"
    );
}

#[test]
fn a_fault_outside_every_watch_still_terminates_the_process() {
    const CHILD: ChildTest =
        ChildTest::new("a_fault_outside_every_watch_still_terminates_the_process");
    if CHILD.child_value().is_some() {
        let _engine = test_engine();
        let file = tempfile::tempfile().expect("backing file");
        file.set_len(4096).expect("size backing file");
        // SAFETY: the mapping is private to this child, which exists to fault on it.
        let mapping = unsafe { memmap2::Mmap::map(&file) }.expect("map backing file");
        file.set_len(0).expect("truncate backing file");
        std::hint::black_box(mapping[0]);
        panic!("an unwatched read of failed storage returned");
    }
    let status = CHILD
        .command("1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run the faulting child");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGBUS),
        "a fault the engine does not own must end the process as it would without the engine"
    );
}
