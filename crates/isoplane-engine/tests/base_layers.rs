//! Application templates captured as layers over a shared runtime baseline.
mod common;
#[path = "common/dispatch.rs"]
mod dispatch;

use std::path::Path;

use common::*;
use isoplane_engine::prepared::{PreparedError, PreparedIdentity};
use isoplane_engine::{Completion, InstanceOps, IsoplaneEngine, OccupancyBudget, Template};

fn identity(seed: u8) -> PreparedIdentity {
    PreparedIdentity {
        input_hash: [1; 32],
        runtime_profile: [2; 32],
        initialization_hash: [seed; 32],
    }
}

/// A guest whose runtime stage writes page 0 and whose application stage
/// writes `marker` into page 17 (offset 70000). Its handler overwrites both.
fn guest_writing(marker: i64) -> Vec<u8> {
    guest(
        2,
        "",
        &format!(
            r#"
          (data (i32.const 100) "unwritten")
          (func (export "isoplane_initialize_runtime") (result i64)
            (i64.store (i32.const 100) (i64.const 0x656d69746e7572))
            (i64.const 0))
          (func (export "isoplane_initialize") (result i64)
            (i64.store (i32.const 70000) (i64.const {marker}))
            (i64.const 0))
        "#
        ),
        "(i64.store (i32.const 100) (i64.const 7))
         (i64.store (i32.const 70000) (i64.const 7))
         (i64.const 0)",
        DONE,
    )
}

/// Publishes the runtime-only initialization of `wasm` and maps it back.
fn baseline(engine: &IsoplaneEngine, wasm: &[u8], directory: &Path) -> Template {
    let source = engine.load_layer_source(wasm).expect("layer source");
    let mut instance = activate(engine, &source);
    assert_eq!(
        instance
            .run_initialize_runtime(OccupancyBudget::http())
            .completion,
        Completion::Done,
        "the runtime stage must complete before capture"
    );
    engine
        .prepare_runtime_baseline(
            &source,
            &mut instance,
            identity(3),
            Default::default(),
            vec![],
        )
        .expect("capture the runtime baseline")
        .publish(directory)
        .expect("publish the baseline");
    // SAFETY: the test never changes the published files.
    unsafe { engine.load_prepared(directory, identity(3), &[]) }.expect("map the baseline")
}

/// Runs the application stage on `base`, publishes it as a layer over `base`,
/// and returns the fully initialized memory it was captured from.
fn layer(engine: &IsoplaneEngine, base: &Template, directory: &Path) -> Vec<u8> {
    let mut instance = activate(engine, base);
    let outcome = instance.run_initialize(OccupancyBudget::http());
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the application stage must complete on the baseline: {:?}",
        outcome.trap
    );
    engine
        .prepare_initialized_template_over(
            base,
            &mut instance,
            identity(4),
            Default::default(),
            vec![],
        )
        .expect("capture the application as a layer")
        .publish(directory)
        .expect("publish the layer");
    instance.memory().to_vec()
}

fn load_over(
    engine: &IsoplaneEngine,
    base: &Template,
    directory: &Path,
) -> Result<Template, PreparedError> {
    // SAFETY: the test never changes the published files.
    unsafe { engine.load_prepared_over(base, directory, identity(4), &[]) }
}

#[test]
fn a_layer_over_a_baseline_activates_to_the_fully_initialized_memory() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(
        &engine,
        &guest_writing(0x6170706c),
        &directory.path().join("base"),
    );
    let initialized = layer(&engine, &base, &directory.path().join("app"));
    let template =
        load_over(&engine, &base, &directory.path().join("app")).expect("load the layer");
    assert_eq!(
        activate(&engine, &template).memory(),
        initialized,
        "an activation must see the baseline's pages and the application's own"
    );
    assert_eq!(
        template.memory_image_arc().to_dense(),
        initialized,
        "hibernation must compare against the combined memory"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn distinct_layers_keep_their_mappings_and_release_private_pages_across_eviction() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(&engine, &guest_writing(1), &directory.path().join("base"));
    let mut templates = Vec::new();
    for marker in 1u8..=8 {
        let mut instance = initialize_layer(&engine, &base, marker);
        let product = engine
            .prepare_initialized_template_over(
                &base,
                &mut instance,
                identity(marker),
                Default::default(),
                vec![],
            )
            .expect("distinct layer capture");
        let path = directory.path().join(marker.to_string());
        product.publish(&path).expect("publish layer");
        // SAFETY: the fixture owns and keeps every published backing file unchanged.
        let template = unsafe { engine.load_prepared_over(&base, &path, identity(marker), &[]) }
            .expect("load distinct layer");
        let mapped = engine
            .map_activation(
                &template,
                isoplane_abi::config::GuestMemoryBytes::new(4 << 20).expect("valid ceiling"),
            )
            .expect("pre-map")
            .expect("layer supports retention");
        templates.push((marker, template, mapped));
    }
    for (marker, _template, mapped) in templates {
        let mut instance = mapped
            .activate(Box::new(TestBindings::new(Recorder::default())))
            .expect("activate mapped guest");
        let address = instance.memory().as_ptr() as usize;
        let mapping_lines = || {
            std::fs::read_to_string("/proc/self/maps")
                .expect("process mappings")
                .lines()
                .filter(|line| {
                    let range = line.split_whitespace().next().expect("address range");
                    let (start, end) = range.split_once('-').expect("range bounds");
                    let start = usize::from_str_radix(start, 16).expect("start");
                    let end = usize::from_str_radix(end, 16).expect("end");
                    start < address + 2 * 65536 && end > address
                })
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let mappings = mapping_lines();
        for _ in 0..3 {
            assert_eq!(
                instance.memory()[70000],
                marker,
                "each guest must retain its own overlay"
            );
            assert_eq!(
                &instance.memory()[100..107],
                b"runtime",
                "unchanged content must come from the shared baseline"
            );
            assert_eq!(
                instance
                    .dispatch(&dispatch::http_dispatch(1), OccupancyBudget::http())
                    .completion,
                Completion::Done,
                "guest must reach a capture boundary"
            );
            instance
                .capture_mutable_state()
                .expect("capture before evicting");
            let dormant = instance
                .evict()
                .expect("captured layered guest retains its mappings");
            assert_eq!(
                mapping_lines(),
                mappings,
                "eviction must preserve the actual VMAs, not only an address reservation"
            );
            use std::os::unix::fs::FileExt;
            let mut entry = [0u8; 8];
            std::fs::File::open("/proc/self/pagemap")
                .expect("page table visibility")
                .read_exact_at(&mut entry, (address as u64 / 4096) * 8)
                .expect("mapped page entry");
            assert_eq!(
                u64::from_ne_bytes(entry) & (1 << 63),
                0,
                "eviction must release resident pages while retaining their mappings"
            );
            instance = dormant
                .activate(Box::new(TestBindings::new(Recorder::default())))
                .expect("reactivate without remapping");
            assert_eq!(
                instance.memory().as_ptr() as usize,
                address,
                "activation must reuse the original address space"
            );
            assert_eq!(
                mapping_lines(),
                mappings,
                "activation must preserve template backing mappings"
            );
            instance.memory()[70000] = 255;
            instance
                .reset()
                .expect("fresh reset in the existing mapping");
            assert_eq!(
                instance.memory()[70000],
                marker,
                "fresh reset must discard private writes"
            );
            assert_eq!(
                instance.memory().as_ptr() as usize,
                address,
                "fresh reset must retain the address space"
            );
            assert_eq!(
                mapping_lines(),
                mappings,
                "fresh reset must retain the template mappings"
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn initialize_layer(
    engine: &IsoplaneEngine,
    base: &Template,
    marker: u8,
) -> isoplane_engine::Instance {
    let mut instance = activate(engine, base);
    assert_eq!(
        instance.run_initialize(OccupancyBudget::http()).completion,
        Completion::Done,
        "initializer must complete"
    );
    instance.memory()[70000] = marker;
    instance
}

#[test]
fn a_layer_stores_only_the_pages_its_application_changed_and_no_native_code() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(
        &engine,
        &guest_writing(0x6170706c),
        &directory.path().join("base"),
    );
    layer(&engine, &base, &directory.path().join("app"));
    let size = |name: &str| {
        std::fs::metadata(directory.path().join("app").join(name))
            .expect("published payload")
            .len()
    };
    assert_eq!(
        size("template.cwasm"),
        0,
        "a layer must use its baseline's native code"
    );
    let base_payload = std::fs::metadata(directory.path().join("base/baseline.bin"))
        .expect("baseline payload")
        .len();
    assert_eq!(
        size("baseline.bin"),
        65536 + isoplane_engine::PAGE as u64,
        "the layer must store one index block and the one page its application changed"
    );
    assert!(
        base_payload > size("baseline.bin"),
        "the baseline holds the memory the layer does not"
    );
}

#[test]
fn a_layer_refuses_to_load_without_or_over_another_base() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(
        &engine,
        &guest_writing(0x6170706c),
        &directory.path().join("base"),
    );
    layer(&engine, &base, &directory.path().join("app"));
    // SAFETY: the test never changes the published files.
    let alone = unsafe { engine.load_prepared(&directory.path().join("app"), identity(4), &[]) };
    assert!(
        matches!(alone, Err(PreparedError::Incompatible(_))),
        "a layer must refuse to load without its base"
    );
    let other = baseline(
        &engine,
        &guest_writing(0x6f74686572),
        &directory.path().join("other"),
    );
    assert!(
        matches!(
            load_over(&engine, &other, &directory.path().join("app")),
            Err(PreparedError::Incompatible(_))
        ),
        "a layer must refuse a base other than the one it was captured over"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_baseline_marks_every_layer_over_it_failed() {
    use std::os::unix::fs::PermissionsExt;
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(
        &engine,
        &guest_writing(0x6170706c),
        &directory.path().join("base"),
    );
    layer(&engine, &base, &directory.path().join("app"));
    let template =
        load_over(&engine, &base, &directory.path().join("app")).expect("load the layer");
    let payload = directory.path().join("base/baseline.bin");
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o600))
        .expect("make the baseline writable");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&payload)
        .and_then(|file| file.set_len(0))
        .expect("truncate the baseline");
    template
        .memory_image_arc()
        .page_eq(0, &[0; isoplane_engine::PAGE]);
    assert!(
        template.storage_failed(),
        "a layer must report the failure of the baseline it reads unchanged pages from"
    );
}

#[test]
fn an_instance_of_a_layer_keeps_its_writes_to_base_and_layer_pages_private() {
    let engine = test_engine();
    let directory = tempfile::tempdir().expect("directory");
    let base = baseline(
        &engine,
        &guest_writing(0x6170706c),
        &directory.path().join("base"),
    );
    let base_memory = activate(&engine, &base).memory().to_vec();
    let initialized = layer(&engine, &base, &directory.path().join("app"));
    let template =
        load_over(&engine, &base, &directory.path().join("app")).expect("load the layer");
    let mut writer = activate(&engine, &template);
    let mut concurrent = activate(&engine, &template);
    assert_eq!(
        writer
            .dispatch(&dispatch::http_dispatch(1), OccupancyBudget::http())
            .completion,
        Completion::Done,
        "the handler must overwrite a base page and a layer page"
    );
    let written = writer.memory();
    assert_eq!(
        (&written[100..108], &written[70000..70008]),
        (&7u64.to_le_bytes()[..], &7u64.to_le_bytes()[..]),
        "the writing instance must see its own writes"
    );
    assert!(
        concurrent.memory() == initialized.as_slice(),
        "a concurrent instance of the layer must not see another's writes"
    );
    drop(writer);
    assert!(
        activate(&engine, &template).memory() == initialized.as_slice(),
        "a later activation must start from the layer, not an earlier instance's writes"
    );
    assert!(
        activate(&engine, &base).memory() == base_memory.as_slice(),
        "an instance of a layer must not write through to its base"
    );
}
