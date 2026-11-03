//! Node-wide ordering over the node's cells.

use std::sync::{Arc, Weak};
use std::time::Duration;

use isoplane_host::cell::{ReclaimCandidate, ReclaimKind};
use isoplane_host::memory_pressure::{MemoryPressureSource, SystemMemoryPressure};

#[derive(Debug, Clone)]
struct Candidate {
    cell: String,
    instance: ReclaimCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Cursor {
    last_used_ms: u64,
    cell: String,
    instance: isoplane_host::InstanceId,
}

impl Candidate {
    fn cursor(&self) -> Cursor {
        Cursor {
            last_used_ms: self.instance.last_used_ms,
            cell: self.cell.clone(),
            instance: self.instance.instance,
        }
    }
}

/// Where the coordinator finds and reclaims instances: the node's cells.
#[async_trait::async_trait]
trait Instances: Send + Sync {
    /// The least recently used candidate of `kind` ordered after `after`.
    fn candidate(
        &self,
        kind: ReclaimKind,
        now_ms: u64,
        after: Option<&Cursor>,
    ) -> Option<Candidate>;
    async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool>;
}

struct Local(Weak<crate::real::RealCells>);

#[async_trait::async_trait]
impl Instances for Local {
    fn candidate(
        &self,
        kind: ReclaimKind,
        now_ms: u64,
        after: Option<&Cursor>,
    ) -> Option<Candidate> {
        let cells = self.0.upgrade()?;
        cells
            .cells()
            .iter()
            .flat_map(|cell| {
                cell.reclamation_candidates(now_ms)
                    .into_iter()
                    .filter(move |candidate| candidate.kind == kind)
                    .map(move |instance| Candidate {
                        cell: cell.cfg.name.clone(),
                        instance,
                    })
            })
            .filter(|candidate| after.is_none_or(|cursor| candidate.cursor() > *cursor))
            .min_by_key(Candidate::cursor)
    }

    async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool> {
        let Some(cells) = self.0.upgrade() else {
            return Ok(false);
        };
        let Some(cell) = cells
            .cells()
            .iter()
            .find(|cell| cell.cfg.name == candidate.cell)
            .cloned()
        else {
            return Ok(false);
        };
        tokio::task::spawn_blocking(move || cell.reclaim_candidate(candidate.instance)).await?
    }
}

/// One ordering authority for every cell a node owns.
pub(crate) struct Coordinator {
    instances: Arc<dyn Instances>,
    round: tokio::sync::Mutex<()>,
    pressure: Arc<dyn MemoryPressureSource>,
    policy: isoplane_host::hibernate_policy::HibernateConfig,
    reclaiming: std::sync::atomic::AtomicBool,
}

impl Coordinator {
    pub(crate) fn new(
        cells: &Arc<crate::real::RealCells>,
        policy: isoplane_host::hibernate_policy::HibernateConfig,
    ) -> Arc<Self> {
        for cell in cells.cells() {
            cell.set_node_coordinated_reclamation();
        }
        let coordinator = Arc::new(Self {
            instances: Arc::new(Local(Arc::downgrade(cells))),
            round: tokio::sync::Mutex::new(()),
            pressure: Arc::new(SystemMemoryPressure),
            policy,
            reclaiming: std::sync::atomic::AtomicBool::new(false),
        });
        for cell in cells.cells() {
            let weak = Arc::downgrade(&coordinator);
            let runtime = cell.reactor().clone();
            cell.set_storage_reclaimer(Arc::new(move || {
                let Some(coordinator) = weak.upgrade() else {
                    return Ok(false);
                };
                runtime.block_on(coordinator.reclaim_storage())
            }));
        }
        coordinator
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        interval: Duration,
        stop: tokio_util::sync::CancellationToken,
    ) {
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = ticks.tick() => {},
                }
                let Some(coordinator) = weak.upgrade() else {
                    return;
                };
                let result = tokio::select! {
                    _ = stop.cancelled() => return,
                    result = coordinator.reclaim_memory() => result,
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "node memory reclamation failed");
                }
            }
        });
    }

    #[tracing::instrument(name = "node_memory_reclamation", skip_all)]
    async fn reclaim_memory(&self) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        let now_ms = isoplane_host::now_ms();
        let mut idle = None;
        let mut active = None;
        loop {
            let _round = self.round.lock().await;
            let sample = self.pressure.sample()?;
            let reclaim = self
                .policy
                .pressure_requested(sample, self.reclaiming.load(Ordering::Relaxed));
            self.reclaiming.store(reclaim, Ordering::Relaxed);
            if !reclaim {
                return Ok(());
            }
            // Completed work goes before unfinished work, each oldest first.
            let candidate = match self
                .instances
                .candidate(ReclaimKind::Idle, now_ms, idle.as_ref())
            {
                Some(candidate) => {
                    idle = Some(candidate.cursor());
                    candidate
                }
                None => match self
                    .instances
                    .candidate(ReclaimKind::Active, now_ms, active.as_ref())
                {
                    Some(candidate) => {
                        active = Some(candidate.cursor());
                        candidate
                    }
                    None => return Ok(()),
                },
            };
            if let Err(error) = self.instances.reclaim(candidate).await {
                tracing::warn!(%error, "node reclamation candidate refused; continuing with later candidates");
            }
        }
    }

    #[tracing::instrument(name = "node_storage_reclamation", skip_all)]
    pub(crate) async fn reclaim_storage(&self) -> anyhow::Result<bool> {
        let _round = self.round.lock().await;
        let now_ms = isoplane_host::now_ms();
        let mut after = None;
        while let Some(candidate) =
            self.instances
                .candidate(ReclaimKind::HibernatedIdle, now_ms, after.as_ref())
        {
            after = Some(candidate.cursor());
            match self.instances.reclaim(candidate).await {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "node snapshot eviction candidate refused"),
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "../tests/common/runtime_inventory.rs"]
mod runtime_inventory;

#[cfg(test)]
mod tests {
    use super::runtime_inventory;
    use super::*;
    use isoplane_host::memory_pressure::{MemoryBytes, MemoryPressureError, MemorySample};

    struct Pressure;
    impl MemoryPressureSource for Pressure {
        fn sample(&self) -> Result<MemorySample, MemoryPressureError> {
            Ok(MemorySample {
                available: MemoryBytes(0),
                total: MemoryBytes(100),
            })
        }
    }

    struct TestInstances {
        candidates: parking_lot::Mutex<Vec<Candidate>>,
        attempts: parking_lot::Mutex<Vec<u64>>,
        refused: Option<u64>,
    }

    #[async_trait::async_trait]
    impl Instances for TestInstances {
        fn candidate(
            &self,
            kind: ReclaimKind,
            _now_ms: u64,
            after: Option<&Cursor>,
        ) -> Option<Candidate> {
            self.candidates
                .lock()
                .iter()
                .filter(|candidate| candidate.instance.kind == kind)
                .filter(|candidate| after.is_none_or(|cursor| candidate.cursor() > *cursor))
                .min_by_key(|candidate| candidate.cursor())
                .cloned()
        }

        async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool> {
            let id = candidate.instance.instance.0;
            self.attempts.lock().push(id);
            anyhow::ensure!(self.refused != Some(id), "candidate cannot be captured");
            let mut candidates = self.candidates.lock();
            let before = candidates.len();
            candidates.retain(|current| current.instance.instance != candidate.instance.instance);
            Ok(candidates.len() < before)
        }
    }

    fn candidate(id: u64, last_used_ms: u64, kind: ReclaimKind) -> Candidate {
        Candidate {
            cell: "cell".into(),
            instance: ReclaimCandidate {
                instance: isoplane_host::InstanceId(id),
                last_used_ms,
                parks: 0,
                kind,
            },
        }
    }

    fn coordinator(
        candidates: Vec<Candidate>,
        refused: Option<u64>,
    ) -> (Coordinator, Arc<TestInstances>) {
        let instances = Arc::new(TestInstances {
            candidates: parking_lot::Mutex::new(candidates),
            attempts: Default::default(),
            refused,
        });
        let coordinator = Coordinator {
            instances: instances.clone(),
            round: tokio::sync::Mutex::new(()),
            pressure: Arc::new(Pressure),
            policy: Default::default(),
            reclaiming: Default::default(),
        };
        (coordinator, instances)
    }

    #[tokio::test]
    async fn idle_candidates_precede_older_unfinished_work() {
        let (coordinator, instances) = coordinator(
            vec![
                candidate(1, 1, ReclaimKind::Active),
                candidate(2, 20, ReclaimKind::Idle),
                candidate(3, 10, ReclaimKind::Idle),
            ],
            None,
        );
        coordinator
            .reclaim_memory()
            .await
            .expect("reclaim under pressure");
        assert_eq!(
            *instances.attempts.lock(),
            vec![3, 2, 1],
            "node-wide LRU must drain completed work before unfinished work"
        );
    }

    #[tokio::test]
    async fn an_oldest_capture_refusal_does_not_starve_later_eligible_candidates() {
        let (coordinator, instances) = coordinator(
            vec![
                candidate(1, 1, ReclaimKind::Idle),
                candidate(2, 2, ReclaimKind::Idle),
                candidate(3, 0, ReclaimKind::Active),
            ],
            Some(1),
        );
        coordinator
            .reclaim_memory()
            .await
            .expect("continue after capture refusal");
        assert_eq!(*instances.attempts.lock(), vec![1, 2, 3], "a refused oldest candidate must be attempted once while later idle candidates retain priority");
    }

    #[tokio::test]
    async fn storage_reclamation_discards_only_the_oldest_idle_snapshot() {
        let (coordinator, instances) = coordinator(
            vec![
                candidate(1, 1, ReclaimKind::Active),
                candidate(2, 20, ReclaimKind::HibernatedIdle),
                candidate(3, 10, ReclaimKind::HibernatedIdle),
            ],
            None,
        );
        assert!(
            coordinator
                .reclaim_storage()
                .await
                .expect("reclaim snapshot capacity"),
            "an idle snapshot must release capacity for admission"
        );
        assert_eq!(
            *instances.attempts.lock(),
            vec![3],
            "storage admission may discard only the oldest completed snapshot"
        );
    }

    #[tokio::test]
    async fn a_real_http_request_wakes_after_pressure_driven_snapshot_capture() {
        use isoplane_abi::result::ResultRecord;
        use isoplane_host::LifecycleTier;

        let mut directory_builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            directory_builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = directory_builder
            .tempdir()
            .expect("create owned node snapshot directory");
        let mut config = crate::NodeConfig::new("acme");
        let app = "app".to_owned();
        let version = "v1".to_owned();
        let tenant = config.owner.clone();
        let hostname = "app.example".to_owned();
        let manifest: isoplane_abi::config::AppManifest =
            serde_json::from_value(serde_json::json!({
                "name": app,
                "guest_memory_bytes": 2 * isoplane_abi::WASM_PAGE_SIZE,
            }))
            .expect("fixture manifest must describe its memory ceiling");
        config.preparation = Some(toml::from_str("").unwrap());
        config.preparation.as_mut().unwrap().runtimes =
            Some(runtime_inventory::install(directory.path()));
        config.apps.push(crate::config::AppConfig {
            manifest: manifest.clone(),
            hosts: vec![hostname.clone()],
        });
        config.data_dir = directory.path().to_path_buf();
        config.engine.max_instances = 2;
        config.engine.memory_limit_mib = 16;
        config.engine.guard_mib = 16;
        config.host.hibernate.handler_after_ms = 0;
        let node = crate::Node::build(config).expect("assemble a real node");
        // This test drives reclamation itself, under a pressure it controls.
        node.background.cancel();
        let real = node.real.clone();
        let response = isoplane_abi::encode(&isoplane_abi::dispatch::ResponseHead {
            status: 200,
            headers: vec![],
            body: b"resumed".to_vec(),
            streaming: false,
        })
        .expect("encode the fixture HTTP response");
        let escaped = isoplane_test_support::wat_bytes(&response);
        let pending = isoplane_abi::status::PollStatus::Pending.encode();
        let wasm = wat::parse_str(format!(r#"(module
            (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
            (import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
            (memory (export "memory") 2 2)
            (global $phase (export "phase") (mut i32) (i32.const 0))
            (data (i32.const 1024) "{escaped}")
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 16384))
            (func (export "isoplane_handle") (param i32 i32) (result i64)
                (global.set $phase (i32.const 1))
                (i32.store (i32.const 8192) (i32.const 123456))
                (drop (call $timer (i64.const 600000)))
                (i64.const {pending}))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64)
                (if (i32.ne (global.get $phase) (i32.const 1)) (then unreachable))
                (if (i32.ne (i32.load (i32.const 8192)) (i32.const 123456)) (then unreachable))
                (i64.shl (i64.extend_i32_u (call $response (i32.const 1024) (i32.const {}))) (i64.const 32))))"#, response.len()))
            .expect("assemble the returned-stack continuation fixture");
        let template = real
            .engine()
            .load_template(&wasm)
            .expect("load real guest template");
        real.install_prepared(&app, &version, &tenant, manifest, template)
            .await
            .expect("register the real guest for local snapshot capture");
        node.routes
            .insert(
                isoplane_abi::host::RouteTarget {
                    app: app.clone(),
                    tenant: tenant.clone(),
                    version: version.clone(),
                    tier: isoplane_abi::host::CodeTier::Fast,
                },
                std::slice::from_ref(&hostname),
            )
            .expect("publish the fixture route");
        let cell = real.cells().first().expect("one real cell").clone();
        let pressure = Arc::new(ControlledPressure(std::sync::atomic::AtomicU64::new(100)));
        let coordinator = Coordinator {
            instances: Arc::new(Local(Arc::downgrade(&real))),
            round: tokio::sync::Mutex::new(()),
            pressure: pressure.clone(),
            policy: node.cfg.host.hibernate,
            reclaiming: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind owned HTTP listener");
        let address = listener.local_addr().expect("read owned HTTP endpoint");
        let serving = tokio::spawn(node.clone().serve(listener));
        let request = tokio::spawn(async move {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build local HTTP client")
                .get(format!("http://{address}/"))
                .header("host", hostname)
                .send()
                .await
                .expect("real HTTP dispatch must finish")
        });
        let (instance, future) = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let candidate = cell
                    .reclamation_candidates(isoplane_host::now_ms())
                    .into_iter()
                    .find(|candidate| candidate.kind == ReclaimKind::Active);
                let futures = cell.futures().live_guest_future_ids();
                if let (Some(candidate), [future]) = (candidate, futures.as_slice()) {
                    break (candidate.instance, *future);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the HTTP guest must return and wait on one host future");
        coordinator
            .reclaim_memory()
            .await
            .expect("sample healthy memory");
        assert_eq!(
            cell.instance_state(instance),
            Some(LifecycleTier::Parked),
            "healthy memory must retain the waiting guest's resident Store"
        );
        pressure.0.store(0, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(60), async {
            while cell.instance_state(instance) != Some(LifecycleTier::Hibernated) {
                coordinator
                    .reclaim_memory()
                    .await
                    .expect("pressure policy must capture the real candidate");
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pressure must release the waiting guest Store");
        assert_eq!(
            cell.futures().live_guest_future_ids(),
            vec![future],
            "automatic pressure capture must preserve the pending host wake source"
        );
        cell.complete_future(future, ResultRecord::Ok(vec![]));
        let response = tokio::time::timeout(Duration::from_secs(60), request)
            .await
            .expect("host completion must wake the HTTP request")
            .expect("join HTTP client");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "the restored continuation must finish the original HTTP response"
        );
        assert_eq!(
            response.bytes().await.expect("read HTTP body").as_ref(),
            b"resumed",
            "pressure restoration must preserve the guest's modified memory and global state"
        );
        assert_eq!(
            cell.metrics().hibernation_wakes,
            1,
            "the HTTP continuation must actually restore a reserved snapshot"
        );
        node.shutdown();
        serving
            .await
            .expect("join real listener")
            .expect("listener shutdown must succeed");
        drop(cell);
        drop(real);
        tokio::task::spawn_blocking(move || drop(node))
            .await
            .expect("drop the cell runtime outside its async executor");
    }

    struct ControlledPressure(std::sync::atomic::AtomicU64);
    impl MemoryPressureSource for ControlledPressure {
        fn sample(&self) -> Result<MemorySample, MemoryPressureError> {
            Ok(MemorySample {
                available: MemoryBytes(self.0.load(std::sync::atomic::Ordering::SeqCst)),
                total: MemoryBytes(100),
            })
        }
    }
}
