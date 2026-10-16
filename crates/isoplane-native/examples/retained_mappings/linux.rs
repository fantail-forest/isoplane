//! Internal mapping microbenchmark: unique overlays over one shared baseline.
#![forbid(unsafe_code)]
use anyhow::{ensure, Context, Result};
use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{
    Completion, DormantInstance, EngineConfig, HostBindings, InstanceOps, IsoplaneEngine,
    OccupancyBudget, Template,
};
use isoplane_host::bindings::{CellBindings, InstanceCtx, Tenancy};
use isoplane_host::{InstanceId, ModuleId};
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Instant;

const PLAN: Limits = Limits {
    module_bytes_compressed: 52428800,
    package_bytes: 52428800,
    instance_memory_max: 536870912,
    occupancy_ms_per_http_dispatch: 300000,
    subrequests_per_dispatch: 10000,
    pending_futures_per_dispatch: 4096,
};

fn initialization_budget() -> OccupancyBudget {
    OccupancyBudget {
        occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
        wall_ms_cap: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
        spent_ns: 0,
    }
}

fn request_budget() -> OccupancyBudget {
    OccupancyBudget {
        occupancy_ms: PLAN.limits().occupancy_ms_per_http_dispatch,
        wall_ms_cap: isoplane_host::CellConfig::default().wall_ms_cap,
        spent_ns: 0,
    }
}

fn bindings(id: usize) -> Box<dyn HostBindings> {
    CellBindings::boxed(Arc::new(InstanceCtx::new(
        InstanceId(id as u64),
        Tenancy {
            module: ModuleId::new(format!("unique-{id}"), "v1"),
            tenant: "mapping-experiment".into(),
            limits: PLAN,

            allowlist: None,
        },
        Arc::new(isoplane_host::meters::Meters::new()),
        None,
    )))
}

fn identity(id: usize) -> PreparedIdentity {
    PreparedIdentity {
        input_hash: [1; 32],
        runtime_profile: [2; 32],
        initialization_hash: *blake3::hash(&id.to_le_bytes()).as_bytes(),
    }
}

fn memory() -> Result<serde_json::Value> {
    Ok(
        serde_json::json!({"file_descriptors":std::fs::read_dir("/proc/self/fd")?.try_fold(0usize, |count, entry| entry.map(|_| count + 1))?, "smaps_rollup":std::fs::read_to_string("/proc/self/smaps_rollup")?,
        "status":std::fs::read_to_string("/proc/self/status")?, "vmas":std::fs::read_to_string("/proc/self/maps")?.lines().count()}),
    )
}

struct Guest {
    id: usize,
    template: Template,
    mapped: Option<Box<dyn DormantInstance>>,
}

fn templates(engine: &IsoplaneEngine, count: usize, storage: &Path) -> Result<Vec<Guest>> {
    let wasm = wat::parse_str(
        r#"(module
      (memory (export "memory") 2048 2048)
      (table (export "table") 32768 32768 funcref)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 134213632))
      (func (export "isoplane_initialize_runtime") (result i64)
        (memory.fill (i32.const 0) (i32.const 1) (i32.const 75497472)) (i64.const 0))
      (func (export "isoplane_initialize") (result i64) (i64.const 0))
      (func (export "isoplane_handle") (param i32 i32) (result i64)
        (local $p i32)
        (loop $touch
          (i32.store (local.get $p) (i32.add (i32.load (local.get $p)) (i32.const 1)))
          (local.set $p (i32.add (local.get $p) (i32.const 4096)))
          (br_if $touch (i32.lt_u (local.get $p) (i32.const 16777216)))) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )?;
    let source = engine.load_layer_source(&wasm)?;
    let mut initial = engine.activate(&source, bindings(count))?;
    ensure!(
        initial
            .run_initialize_runtime(initialization_budget())
            .completion
            == Completion::Done,
        "baseline initialization failed"
    );
    let product = engine.prepare_runtime_baseline(
        &source,
        &mut initial,
        identity(0),
        Default::default(),
        vec![],
    )?;
    drop(initial);
    let base = engine.load_prepared_product_in(&product, storage)?;
    let mut guests = Vec::with_capacity(count);
    for id in 1..=count {
        let mut instance = engine.activate(&base, bindings(id))?;
        ensure!(
            instance.run_initialize(initialization_budget()).completion == Completion::Done,
            "app initialization failed"
        );
        for offset in (0..72 * 1024 * 1024).step_by(512 * 1024) {
            instance.memory()[offset..offset + 4096].fill((id % 251) as u8);
            instance.memory()[offset..offset + 8].copy_from_slice(&(id as u64).to_le_bytes());
        }
        let product = engine.prepare_initialized_template_over(
            &base,
            &mut instance,
            identity(id),
            Default::default(),
            vec![],
        )?;
        drop(instance);
        let template = engine.load_prepared_product_over_in(&base, &product, storage)?;
        guests.push(Guest {
            id,
            template,
            mapped: None,
        });
    }
    Ok(guests)
}

fn run(
    engine: &IsoplaneEngine,
    guests: &mut [Guest],
    retain: bool,
    workers: usize,
) -> Result<Vec<serde_json::Value>> {
    let barrier = Barrier::new(workers.min(guests.len().div_ceil(guests.len().div_ceil(workers))));
    std::thread::scope(|scope| {
        let tasks: Vec<_> = guests.chunks_mut(guests.len().div_ceil(workers)).map(|chunk| {
            let barrier = &barrier;
            scope.spawn(move || -> Result<Vec<serde_json::Value>> {
                let record = DispatchRecord { dispatch_id: 1, kind: DispatchKind::Http(HttpRequest {
                    method: "GET".into(), url: "https://mapping.test/".into(), path: "/".into(), query: vec![], headers: vec![], body: vec![],
                }), trace: TraceContext::default(), cancel_future: 0, occupancy_budget_ms: request_budget().occupancy_ms, now_ms: 0 };
                barrier.wait();
                let mut rows = Vec::new();
                for guest in chunk {
                    let start = Instant::now();
                    let mut instance: Box<dyn InstanceOps + Send> = match guest.mapped.take() {
                        Some(mapped) => mapped.activate(bindings(guest.id))?,
                        None => Box::new(engine.activate(&guest.template, bindings(guest.id))?),
                    };
                    let activation_ms = start.elapsed().as_secs_f64()*1000.;
                    ensure!(u64::from_le_bytes(instance.memory()[..8].try_into()?) == guest.id as u64, "unique overlay identity lost");
                    ensure!(instance.dispatch(&record, request_budget()).completion == Completion::Done, "first dispatch failed");
                    let first_ms = start.elapsed().as_secs_f64()*1000.;
                    let second = Instant::now();
                    ensure!(instance.dispatch(&record, request_budget()).completion == Completion::Done, "second dispatch failed");
                    let second_ms = second.elapsed().as_secs_f64()*1000.;
                    if retain {
                        // This experiment deliberately resets to the template, with no continuation to preserve.
                        instance.capture_mutable_state()?;
                        guest.mapped = Some(instance.evict().context("eviction did not retain the mapping")?);
                    }
                    rows.push(serde_json::json!({"guest":guest.id,"activation_ms":activation_ms,"first_ms":first_ms,"second_ms":second_ms,"penalty_ms":first_ms-second_ms}));
                }
                Ok(rows)
            })
        }).collect();
        let mut rows = Vec::new();
        for task in tasks {
            rows.extend(
                task.join()
                    .map_err(|_| anyhow::anyhow!("measurement worker panicked"))??,
            );
        }
        Ok(rows)
    })
}

pub(super) fn measure() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let storage = Path::new(args.get(1).context("supply an owned output directory")?);
    let count: usize = args
        .get(2)
        .context("supply the unique guest count")?
        .parse()?;
    ensure!(count > 0, "guest count must be positive");
    std::fs::create_dir_all(storage)?;
    let engine = IsoplaneEngine::new(EngineConfig::default())?;
    let mut guests = templates(&engine, count, storage)?;
    let baseline = memory()?;
    let cold = run(&engine, &mut guests, false, 64)?;
    let before_mapping = memory()?;
    let start = Instant::now();
    for guest in &mut guests {
        guest.mapped = engine.map_activation(
            &guest.template,
            isoplane_abi::config::GuestMemoryBytes::new(128 << 20)?,
        )?;
    }
    let mapping_ms = start.elapsed().as_secs_f64() * 1000.;
    let mapped = memory()?;
    let retained = run(&engine, &mut guests, true, 64)?;
    let evicted = memory()?;
    let repeated = run(&engine, &mut guests, true, 64)?;
    let repeated_memory = memory()?;
    std::fs::write(
        storage.join("results.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "guests":count,"concurrency":64,"baseline":baseline,"before_mapping":before_mapping,"mapped":mapped,
            "mapping_ms":mapping_ms,"cold":cold,"retained":retained,"evicted":evicted,"repeated":repeated,"repeated_memory":repeated_memory,
        }))?,
    )?;
    Ok(())
}
