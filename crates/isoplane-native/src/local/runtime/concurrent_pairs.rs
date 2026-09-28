//! Diagnostic-only repeated HTTP pairs over shared prepared templates.

use super::*;
use anyhow::Context;
use isoplane_host::Activator;
use rand::{seq::SliceRandom, SeedableRng};
use serde::Deserialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Specification {
    cases: Vec<Case>,
    pairs_per_case: usize,
    concurrency: usize,
    seed: u64,
    #[serde(default)]
    unique_routes: bool,
    storage: PathBuf,
    records: PathBuf,
}

#[derive(Deserialize)]
struct Case {
    app: String,
    variant: String,
    wasm: PathBuf,
    options: PathBuf,
    endpoint: String,
    body: String,
}

struct Trial {
    repetition: usize,
    lane: usize,
}

struct ReadyCase {
    case: Case,
    prepared: Prepared,
    configuration: isoplane_host::bindings::RuntimeConfiguration,
}

struct Alias {
    module: ModuleId,
    inner: Arc<EngineActivator>,
}

impl Activator for Alias {
    fn map_activation(
        &self,
        memory_limit_bytes: isoplane_abi::config::GuestMemoryBytes,
    ) -> anyhow::Result<Option<Box<dyn isoplane_engine::DormantInstance>>> {
        self.inner.map_activation(memory_limit_bytes)
    }
    fn module(&self) -> &ModuleId {
        &self.module
    }
    fn module_hash(&self) -> isoplane_engine::ModuleHash {
        self.inner.module_hash()
    }
    fn abi_rev(&self) -> u32 {
        self.inner.abi_rev()
    }
    fn snapshot_size_bound(&self, memory_bytes: u64) -> anyhow::Result<u64> {
        self.inner.snapshot_size_bound(memory_bytes)
    }
    fn imports(&self, module: &str, name: &str) -> bool {
        self.inner.imports(module, name)
    }
    fn template_image(&self) -> Arc<isoplane_engine::MemoryImage> {
        self.inner.template_image()
    }
    fn activate(
        &self,
        bindings: Box<dyn isoplane_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn isoplane_engine::InstanceOps + Send>> {
        self.inner.activate(bindings)
    }
}

struct Routes(RwLock<HashMap<String, RouteTarget>>);
impl isoplane_proxy::RouteResolver for Routes {
    fn resolve(&self, host: &str, _path: &str, _key: Option<&str>) -> Option<RouteTarget> {
        self.0.read().get(host).cloned()
    }
    fn generation(&self) -> u64 {
        1
    }
}

/// Runs sequential HTTP pairs with a bounded number of concurrent fresh guests.
/// The specification and all artifacts are private diagnostic inputs.
pub fn run_concurrent_pairs(path: &Path) -> anyhow::Result<()> {
    let specification: Specification = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        specification.concurrency > 0 && specification.pairs_per_case > 0,
        "positive concurrency and sample count required"
    );
    std::fs::create_dir_all(&specification.storage)?;
    std::fs::create_dir_all(&specification.records)?;
    let baselines = Baselines::default();
    let mut prepared = HashMap::<(String, PathBuf, PathBuf), Prepared>::new();
    let mut cases = Vec::new();
    for case in specification.cases {
        let options: crate::session::Options =
            serde_json::from_slice(&std::fs::read(&case.options)?)?;
        let inputs = options.inputs()?;
        let configuration = isoplane_host::bindings::RuntimeConfiguration::from_json(
            &serde_json::to_vec(&options.configuration)?,
        )?;
        let key = (case.app.clone(), case.wasm.clone(), case.options.clone());
        let value = match prepared.get(&key) {
            Some(value) => value.clone(),
            None => {
                let value = Prepared::build(
                    &inputs,
                    &std::fs::read(&case.wasm)?,
                    &configuration,
                    &baselines,
                )?;
                prepared.insert(key, value.clone());
                value
            }
        };
        anyhow::ensure!(
            matches!(
                value.manifest.handlers.lifecycle,
                isoplane_abi::config::HandlerLifecycle::Reuse
            ),
            "the pair experiment requires reuse handlers"
        );
        cases.push(ReadyCase {
            case,
            prepared: value,
            configuration,
        });
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(execute(
        cases,
        specification.pairs_per_case,
        specification.concurrency,
        specification.seed,
        specification.unique_routes,
        &specification.storage,
        &specification.records,
    ))
}

async fn execute(
    cases: Vec<ReadyCase>,
    samples: usize,
    concurrency: usize,
    seed: u64,
    unique_routes: bool,
    storage: &Path,
    records: &Path,
) -> anyhow::Result<()> {
    let first = cases
        .first()
        .context("at least one application case is required")?;
    let host =
        LocalApp::start(first.prepared.clone(), storage, first.configuration.clone()).await?;
    let routes = Arc::new(Routes(RwLock::new(HashMap::new())));
    let proxy = isoplane_proxy::Proxy::with_hub(
        routes.clone(),
        host.cell.clone(),
        host.proxy.metrics.clone(),
        Default::default(),
        host.proxy.hub.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_proxy = proxy.clone();
    let server = tokio::spawn(async move { server_proxy.serve(listener).await });
    let mut jobs = Vec::new();
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    for repetition in 0..samples {
        let mut block: Vec<_> = (0..cases.len()).collect();
        block.shuffle(&mut rng);
        jobs.extend(block.into_iter().map(|case| (case, repetition)));
    }
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicUsize::new(0));
    let cases = Arc::new(cases);
    #[cfg(target_os = "linux")]
    let before_mapping = mapping_memory()?;
    for lane in 0..if unique_routes { samples } else { concurrency } {
        for (case, ready) in cases.iter().enumerate() {
            register(
                &host.cell,
                &routes,
                &format!("lane-{lane}-case-{case}.local"),
                ready,
            )?;
        }
    }
    #[cfg(target_os = "linux")]
    std::fs::write(
        records.join("mapping-memory.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "before": before_mapping, "after": mapping_memory()?,
            "mapped_guests": if unique_routes { samples } else { concurrency } * cases.len(),
        }))?,
    )?;
    let start = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for lane in 0..concurrency {
        let (host, jobs, next, cases) = (host.clone(), jobs.clone(), next.clone(), cases.clone());
        let output = records.join(format!("lane-{lane:02}.jsonl"));
        workers.spawn(async move {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(120))
                .pool_max_idle_per_host(0)
                .build()?;
            let mut output = std::io::BufWriter::new(std::fs::File::create(output)?);
            loop {
                let job = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(case, repetition)) = jobs.get(job) else {
                    break;
                };
                let route = if unique_routes { repetition } else { lane };
                let hostname = format!("lane-{route}-case-{case}.local");
                trial(
                    &host.cell,
                    &client,
                    address,
                    &hostname,
                    &cases[case],
                    Trial { repetition, lane },
                    &mut output,
                )
                .await?;
            }
            output.flush()?;
            Ok::<_, anyhow::Error>(())
        });
    }
    let result = async {
        while let Some(worker) = workers.join_next().await {
            worker??;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    proxy.shutdown();
    server.await??;
    host.shutdown();
    result?;
    std::fs::write(
        records.join("completion.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"pairs":jobs.len(),"concurrency":concurrency,"elapsed_seconds":start.elapsed().as_secs_f64(),"seed":seed}),
        )?,
    )?;
    Ok(())
}

fn register(
    cell: &Arc<Cell>,
    routes: &Routes,
    hostname: &str,
    ready: &ReadyCase,
) -> anyhow::Result<ModuleId> {
    let module = ModuleId::new(hostname, &ready.prepared.version);
    let PreparedActivator::Ordinary(inner) = &ready.prepared.activator else {
        anyhow::bail!("the response experiment requires an ordinary runtime");
    };
    let mut manifest = ready.prepared.manifest.clone();
    manifest.name = hostname.into();
    cell.register_module(
        &ready.prepared.tenant,
        Arc::new(Alias {
            module: module.clone(),
            inner: inner.clone(),
        }),
        manifest.clone(),
    )?;
    cell.set_runtime_configuration(&ready.prepared.tenant, &module, ready.configuration.clone())?;
    routes.0.write().insert(
        hostname.into(),
        RouteTarget {
            app: hostname.into(),
            tenant: ready.prepared.tenant.clone(),
            version: ready.prepared.version.clone(),
            plan: manifest.limits,
            lifecycle: manifest.handlers.lifecycle,
            tier: isoplane_abi::host::CodeTier::Fast,
        },
    );
    Ok(module)
}

fn instance(cell: &Arc<Cell>, module: &ModuleId) -> anyhow::Result<isoplane_host::InstanceId> {
    let ids: Vec<_> = cell
        .resident_ids()
        .into_iter()
        .filter(|id| {
            cell.slot(*id)
                .is_some_and(|slot| slot.ctx.module == *module)
        })
        .collect();
    anyhow::ensure!(
        ids.len() == 1,
        "each trial must own exactly one resident instance, found {} for {module}",
        ids.len()
    );
    Ok(ids[0])
}

async fn request(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    hostname: &str,
    case: &Case,
) -> anyhow::Result<f64> {
    let start = Instant::now();
    let response = client
        .get(format!("http://{address}{}", case.endpoint))
        .header("host", hostname)
        .send()
        .await?;
    let status = response.status();
    let body = response.bytes().await?;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    anyhow::ensure!(
        status.as_u16() == 200 && body.as_ref() == case.body.as_bytes(),
        "{} {} must return the expected body with status 200, got {status}",
        case.app,
        case.variant
    );
    Ok(elapsed_ms)
}

async fn trial(
    cell: &Arc<Cell>,
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    hostname: &str,
    ready: &ReadyCase,
    trial: Trial,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    let Trial { repetition, lane } = trial;
    let module = ModuleId::new(hostname, &ready.prepared.version);
    anyhow::ensure!(
        cell.resident_ids()
            .into_iter()
            .all(|id| cell.slot(id).is_none_or(|slot| slot.ctx.module != module)),
        "a new trial must have no resident instance"
    );
    let first_ms = request(client, address, hostname, &ready.case).await?;
    wait_idle(cell, &module).await?;
    let first_instance = instance(cell, &module)?;
    let second_ms = request(client, address, hostname, &ready.case).await?;
    wait_idle(cell, &module).await?;
    let second_instance = instance(cell, &module)?;
    anyhow::ensure!(
        first_instance == second_instance,
        "the second response must use the first response's instance"
    );
    discard_idle(cell, second_instance).await?;
    serde_json::to_writer(
        &mut *output,
        &serde_json::json!({"app":ready.case.app,"variant":ready.case.variant,"repetition":repetition,"lane":lane,"instance":first_instance.0,"first_ms":first_ms,"second_ms":second_ms,"penalty_ms":first_ms-second_ms}),
    )?;
    writeln!(output)?;
    Ok(())
}

async fn wait_idle(cell: &Arc<Cell>, module: &ModuleId) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !cell
            .idle_instances(module)
            .is_some_and(|(warm, _)| warm == 1)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("the response must release its instance before identifying or discarding it")
}

async fn discard_idle(cell: &Arc<Cell>, instance: isoplane_host::InstanceId) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match cell.discard_instance(instance) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if cell.instance_state(instance) != Some(isoplane_host::LifecycleTier::Parked)
                    || Instant::now() >= deadline
                {
                    return Err(error).context("disposing the completed trial at an idle boundary");
                }
                // Idle maintenance may hold the execution lock between request pairs.
                tokio::task::yield_now().await;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn mapping_memory() -> anyhow::Result<serde_json::Value> {
    use std::io::BufRead;
    Ok(serde_json::json!({
        "smaps_rollup": std::fs::read_to_string("/proc/self/smaps_rollup")?,
        "status": std::fs::read_to_string("/proc/self/status")?,
        "vmas": std::io::BufReader::new(std::fs::File::open("/proc/self/maps")?).lines().try_fold(0usize, |count, line| line.map(|_| count + 1))?,
        "file_descriptors": std::fs::read_dir("/proc/self/fd")?.try_fold(0usize, |count, entry| entry.map(|_| count + 1))?,
    }))
}
