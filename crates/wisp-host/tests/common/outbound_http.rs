use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use hyper::body::Frame;
use tokio::sync::{mpsc, oneshot, Notify};
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::result::{FetchResponse, FutureError, ResultRecord};
use wisp_engine::{HostBindings, ImportCall};
use wisp_host::bindings::{CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink};
use wisp_host::egress::{DenyReason, DestinationPolicy, DnsError, HostEgressPolicy, Resolver};
use wisp_host::futures::FutureTable;
use wisp_host::meters::Meters;
use wisp_host::metrics::Metrics;
use wisp_host::{InstanceId, ModuleId, PinnedHttpFetcher};

pub async fn deadline<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(30), future)
        .await
        .expect("the HTTP ownership test must not hang")
}

struct Endpoint(SocketAddr);
#[async_trait::async_trait]
impl Resolver for Endpoint {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
        Ok(vec![self.0])
    }
}
impl DestinationPolicy for Endpoint {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if self.0 == address {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}

pub struct Exchange {
    pub headers: oneshot::Sender<()>,
    pub frames: mpsc::UnboundedSender<Result<Frame<Bytes>, std::io::Error>>,
}

pub struct Server {
    pub address: SocketAddr,
    pub requests: mpsc::UnboundedReceiver<Exchange>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the HTTP test server must bind");
        let address = listener
            .local_addr()
            .expect("the HTTP test server needs an address");
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("the HTTP test server must accept");
                        let requests = requests_tx.clone();
                        connections.spawn(async move {
                            let service = hyper::service::service_fn(move |_| {
                                let (headers, ready) = oneshot::channel();
                                let (frames, receiver) = mpsc::unbounded_channel();
                                requests.send(Exchange { headers, frames }).expect("the test must observe its request");
                                async move {
                                    ready.await.map_err(std::io::Error::other)?;
                                    let body = http_body_util::StreamBody::new(futures::stream::unfold(receiver, |mut receiver| async move {
                                        receiver.recv().await.map(|frame| (frame, receiver))
                                    }));
                                    Ok::<_, std::io::Error>(hyper::Response::new(body))
                                }
                            });
                            if let Err(error) = hyper::server::conn::http1::Builder::new()
                                .serve_connection(hyper_util::rt::TokioIo::new(socket), service).await {
                                tracing::debug!(%error, "HTTP test peer observed cancellation or a body error");
                            }
                        });
                    }
                    joined = connections.join_next(), if !connections.is_empty() => {
                        joined.expect("a connection was present").expect("the HTTP test server must not panic");
                    }
                }
            }
        });
        Self {
            address,
            requests,
            task,
        }
    }
}

pub fn services(address: SocketAddr) -> Arc<Services> {
    services_with_budget(address, 1 << 20)
}

pub fn services_with_budget(address: SocketAddr, max_bytes: u64) -> Arc<Services> {
    let policy = Arc::new(
        HostEgressPolicy::new(Box::new(Endpoint(address)), 1, max_bytes)
            .with_destinations(Arc::new(Endpoint(address))),
    );
    Arc::new(Services {
        fetcher: Arc::new(PinnedHttpFetcher::new(policy.clone(), 0)),
        egress: policy,
    })
}

#[derive(Default)]
struct Wake(Notify);
impl WakeSink for Wake {
    fn signal(&self, _: InstanceId) {
        self.0.notify_one();
    }
}

pub struct Rig {
    pub ctx: Arc<InstanceCtx>,
    pub bindings: CellBindings,
    pub runtime: Arc<CellRuntime>,
    wake: Arc<Wake>,
}
impl Rig {
    pub fn new(services: Arc<Services>, tenant: &str, instance: InstanceId) -> Self {
        let wake = Arc::new(Wake::default());
        let runtime = Arc::new(CellRuntime {
            runtime_configuration: Default::default(),
            services,
            futures: Arc::new(FutureTable::new()),
            metrics: Arc::new(Metrics::new()),
            wake: wake.clone(),
            reactor: tokio::runtime::Handle::current(),
        });
        let ctx = Arc::new(InstanceCtx::new(
            instance,
            Tenancy {
                module: ModuleId::new("http-owner", "v1"),
                tenant: tenant.into(),
                limits: Limits::default(),

                lifecycle: HandlerLifecycle::Reuse,
                allowlist: None,
            },
            Arc::new(Meters::new()),
            Some(runtime.clone()),
        ));
        ctx.begin_dispatch(0, 1);
        Self {
            bindings: CellBindings::new(ctx.clone()),
            ctx,
            runtime,
            wake,
        }
    }

    pub fn fetch(&mut self) -> u64 {
        let payload = postcard::to_allocvec(&wisp_abi::calls::FetchRequest {
            method: "GET".into(),
            url: "http://upstream.test/".into(),
            headers: vec![],
            body: vec![],
        })
        .expect("the fetch request must encode");
        self.bindings.call_future(ImportCall {
            import: "http_fetch",
            payload: &payload,
            scalars: [0; 3],
        })
    }

    pub fn read(&mut self, handle: u32, max_len: usize) -> u64 {
        self.bindings.call_future(ImportCall {
            import: "http_body_read",
            payload: &[],
            scalars: [u64::from(handle), max_len as u64, 0],
        })
    }

    pub async fn result<T: serde::de::DeserializeOwned>(&self, id: u64) -> Result<T, FutureError> {
        loop {
            if let Some((received, result)) = self.runtime.futures.take_ready(self.ctx.instance) {
                assert_eq!(
                    received, id,
                    "the host must deliver the requested operation's result"
                );
                return match result {
                    ResultRecord::Ok(bytes) => {
                        Ok(postcard::from_bytes(&bytes).expect("the guest result must decode"))
                    }
                    ResultRecord::Err(error) => Err(error),
                };
            }
            self.wake.0.notified().await;
        }
    }

    pub async fn response(&self, id: u64) -> FetchResponse {
        self.result(id)
            .await
            .expect("the response headers must arrive")
    }
}

pub fn cell(
    services: Arc<Services>,
    activator: Arc<dyn wisp_host::Activator>,
) -> (
    Arc<wisp_host::cell::Cell>,
    std::sync::mpsc::Receiver<wisp_host::cell::DispatchResult>,
) {
    let module = activator.module().clone();
    let cell = wisp_host::cell::Cell::new(
        wisp_host::cell::CellConfig::for_test("outbound-http"),
        services,
    )
    .expect("the HTTP cell must start");
    cell.register_module(
        "tenant",
        activator,
        wisp_abi::config::AppManifest {
            name: module.app.clone(),
            handlers: wisp_abi::config::HandlerConfig {
                lifecycle: HandlerLifecycle::Reuse,
            },
            bindings: Default::default(),
            limits: Limits::default(),
            init_visible_vars: vec![],
        },
    )
    .expect("the HTTP guest must register");
    let record = cell.http_record(
        &module,
        wisp_abi::dispatch::HttpRequest {
            method: "GET".into(),
            url: "https://app.test/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    );
    let response = cell
        .submit(
            &module,
            wisp_host::cell::Target::Handler { route: "/".into() },
            record,
        )
        .expect("the dispatch must be admitted");
    (cell, response)
}

pub async fn capture(
    cell: &Arc<wisp_host::cell::Cell>,
    instance: InstanceId,
) -> Result<u64, wisp_host::hibernate_policy::CaptureRefused> {
    let cell = cell.clone();
    tokio::task::spawn_blocking(move || cell.force_hibernate(instance))
        .await
        .expect("capture must not panic")
}

pub async fn completed(
    response: std::sync::mpsc::Receiver<wisp_host::cell::DispatchResult>,
) -> wisp_host::cell::DispatchResult {
    tokio::task::spawn_blocking(move || {
        response
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the Cell dispatch must complete before its hung-test deadline")
    })
    .await
    .expect("dispatch completion must not panic")
}
