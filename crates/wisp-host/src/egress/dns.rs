//! Cancellable DNS using the node's startup configuration and hosts records.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::config::{ResolveHosts, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::{RuntimeProvider, Spawn, TokioRuntimeProvider};
use hickory_resolver::Hosts;
use parking_lot::Mutex;
use tokio::task::JoinSet;

/// DNS configuration or lookup failure.
#[derive(Debug, thiserror::Error)]
pub enum DnsError {
    /// The resolver returned no addresses for the name.
    #[error("host resolved to no addresses")]
    NoAddress,
    /// The node's resolver settings cannot be loaded.
    #[error("cannot initialize system DNS: {0}")]
    Configuration(String),
    /// The node's hosts records cannot be loaded.
    #[error("cannot load system hosts: {0}")]
    Hosts(#[source] std::io::Error),
    /// The configured DNS service could not resolve a name.
    #[error("DNS lookup for {host} failed ({kind:?}): {message}")]
    Lookup {
        /// Name requested by the caller.
        host: String,
        /// Portable classification supplied by the resolver implementation.
        kind: wisp_abi::native_io::DnsFailure,
        /// Backend diagnostic retained without exposing its implementation through the resolver trait.
        message: String,
    },
}

impl DnsError {
    /// Resolver failure classification, independent of destination policy and DNS backend.
    pub fn kind(&self) -> wisp_abi::native_io::DnsFailure {
        use wisp_abi::native_io::DnsFailure;
        match self {
            Self::NoAddress => DnsFailure::NotFound,
            Self::Configuration(_) | Self::Hosts(_) => DnsFailure::Permanent,
            Self::Lookup { kind, .. } => *kind,
        }
    }
}

fn classify(error: &hickory_resolver::net::NetError) -> wisp_abi::native_io::DnsFailure {
    use hickory_resolver::net::{DnsError as SemanticError, NetError};
    use hickory_resolver::proto::op::ResponseCode;
    use wisp_abi::native_io::DnsFailure;
    match error {
        NetError::Timeout => DnsFailure::Timeout,
        NetError::Dns(SemanticError::ResponseCode(ResponseCode::ServFail)) => DnsFailure::Temporary,
        NetError::Dns(SemanticError::NoRecordsFound(records)) => match records.response_code {
            ResponseCode::NXDomain | ResponseCode::NoError => DnsFailure::NotFound,
            ResponseCode::ServFail => DnsFailure::Temporary,
            _ => DnsFailure::Permanent,
        },
        NetError::Busy | NetError::NoConnections | NetError::Io(_) => DnsFailure::Temporary,
        // Other dependency errors represent malformed or unsupported resolver operations.
        _ => DnsFailure::Permanent,
    }
}

/// Resolves names without an operating-system blocking resolver call.
#[async_trait::async_trait]
pub trait Resolver: Send + Sync + 'static {
    /// Resolve a name; dropping this future cancels all work owned by its lookup.
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError>;
}

/// Startup-loaded system DNS settings, search domains, and hosts records.
pub struct SystemResolver {
    config: ResolverConfig,
    options: ResolverOpts,
    hosts: Arc<Hosts>,
}

impl SystemResolver {
    /// Load and validate the node's DNS configuration before acquiring service resources.
    pub fn new() -> Result<Self, DnsError> {
        let (config, mut options) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|error| DnsError::Configuration(error.to_string()))?;
        let hosts = Arc::new(Hosts::from_system().map_err(DnsError::Hosts)?);
        options.use_hosts_file = ResolveHosts::Never;
        hickory_resolver::Resolver::builder_with_config(
            config.clone(),
            TokioRuntimeProvider::default(),
        )
        .with_options(options.clone())
        .build()
        .map_err(|error| DnsError::Configuration(error.to_string()))?;
        Ok(Self {
            config,
            options,
            hosts,
        })
    }
}

#[async_trait::async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError> {
        let scope = LookupScope::default();
        self.resolve_in_scope(host, port, &scope).await
    }
}

impl SystemResolver {
    async fn resolve_in_scope(
        &self,
        host: &str,
        port: u16,
        scope: &LookupScope,
    ) -> Result<Vec<SocketAddr>, DnsError> {
        let provider = LookupRuntime {
            scope: scope.0.clone(),
            runtime: TokioRuntimeProvider::default(),
        };
        let mut resolver =
            hickory_resolver::Resolver::builder_with_config(self.config.clone(), provider)
                .with_options(self.options.clone())
                .build()
                .map_err(|error| DnsError::Configuration(error.to_string()))?;
        resolver.set_hosts(self.hosts.clone());
        let answer = resolver
            .lookup_ip(host)
            .await
            .map_err(|source| DnsError::Lookup {
                host: host.to_owned(),
                kind: classify(&source),
                message: source.to_string(),
            })?;
        Ok(answer
            .iter()
            .map(|address| SocketAddr::new(address, port))
            .collect())
    }
}

#[derive(Default)]
struct LookupTasks {
    closed: bool,
    jobs: JoinSet<()>,
}

#[derive(Default)]
struct LookupScope(Arc<Mutex<LookupTasks>>);

impl Drop for LookupScope {
    fn drop(&mut self) {
        let mut tasks = self.0.lock();
        tasks.closed = true;
        tasks.jobs.abort_all();
    }
}

#[derive(Clone)]
struct LookupRuntime {
    scope: Arc<Mutex<LookupTasks>>,
    runtime: TokioRuntimeProvider,
}

impl Spawn for LookupRuntime {
    fn spawn_bg(&mut self, future: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.scope.lock();
        if tasks.closed {
            return;
        }
        while let Some(result) = tasks.jobs.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "DNS background task failed");
            }
        }
        tasks.jobs.spawn(future);
    }
}

impl RuntimeProvider for LookupRuntime {
    type Handle = Self;
    type Timer = <TokioRuntimeProvider as RuntimeProvider>::Timer;
    type Udp = <TokioRuntimeProvider as RuntimeProvider>::Udp;
    type Tcp = <TokioRuntimeProvider as RuntimeProvider>::Tcp;

    fn create_handle(&self) -> Self::Handle {
        self.clone()
    }

    fn connect_tcp(
        &self,
        server: SocketAddr,
        bind: Option<SocketAddr>,
        timeout: Option<Duration>,
    ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Tcp>>>> {
        self.runtime.connect_tcp(server, bind, timeout)
    }

    fn bind_udp(
        &self,
        local: SocketAddr,
        server: SocketAddr,
    ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Udp>>>> {
        self.runtime.bind_udp(local, server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::config::{ConnectionConfig, LookupIpStrategy, NameServerConfig};
    use hickory_resolver::proto::op::{Message, MessageType};
    use hickory_resolver::proto::rr::{rdata::A, RData, Record};

    async fn configured_server() -> (SystemResolver, tokio::net::UdpSocket) {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("the owned DNS server must bind");
        let peer = server
            .local_addr()
            .expect("the DNS server must have an endpoint");
        let mut connection = ConnectionConfig::udp();
        connection.port = peer.port();
        let mut options = ResolverOpts::default();
        options.ip_strategy = LookupIpStrategy::Ipv4Only;
        options.use_hosts_file = ResolveHosts::Never;
        let resolver = SystemResolver {
            config: ResolverConfig::from_name_servers(vec![NameServerConfig::new(
                peer.ip(),
                true,
                vec![connection],
            )]),
            options,
            hosts: Arc::new(Hosts::default()),
        };
        (resolver, server)
    }

    #[tokio::test]
    async fn the_resolver_reads_an_answer_from_its_configured_dns_server() {
        let (resolver, server) = configured_server().await;
        let lookup = tokio::spawn(async move { resolver.resolve("owned.test.", 443).await });
        let mut bytes = vec![0; 4096];
        let (length, source) =
            tokio::time::timeout(Duration::from_secs(60), server.recv_from(&mut bytes))
                .await
                .expect("the DNS query must arrive before the test deadline")
                .expect("the configured DNS server must receive the query");
        let mut response =
            Message::from_vec(&bytes[..length]).expect("the resolver must send valid DNS");
        assert_eq!(
            response.queries[0].name().to_ascii(),
            "owned.test.",
            "the resolver must preserve the requested DNS name"
        );
        response.metadata.message_type = MessageType::Response;
        response.metadata.recursion_available = true;
        response.add_answer(Record::from_rdata(
            response.queries[0].name().clone(),
            60,
            RData::A(A(std::net::Ipv4Addr::new(93, 184, 216, 34))),
        ));
        server
            .send_to(
                &response.to_vec().expect("the DNS answer must encode"),
                source,
            )
            .await
            .expect("the owned DNS server must return its answer");
        let addresses = tokio::time::timeout(Duration::from_secs(60), lookup)
            .await
            .expect("DNS must finish before the test deadline")
            .expect("the DNS task must not panic")
            .expect("the valid DNS answer must resolve");
        assert_eq!(
            addresses,
            vec![SocketAddr::from(([93, 184, 216, 34], 443))],
            "the async resolver must return the actual server answer with the caller's port"
        );
    }

    async fn refused_lookup(code: hickory_resolver::proto::op::ResponseCode) -> DnsError {
        let (mut resolver, server) = configured_server().await;
        resolver.options.attempts = 1;
        let lookup = tokio::spawn(async move { resolver.resolve("failure.test.", 443).await });
        let mut bytes = vec![0; 4096];
        tokio::time::timeout(Duration::from_secs(60), async {
            tokio::pin!(lookup);
            loop {
                tokio::select! {
                    result = &mut lookup => return result.expect("the lookup task must not panic")
                        .expect_err("an error DNS answer must fail lookup"),
                    received = server.recv_from(&mut bytes) => {
                        let (length, peer) = received.expect("the DNS server must receive a query");
                        let mut response = Message::from_vec(&bytes[..length]).expect("the DNS query must decode");
                        response.metadata.message_type = MessageType::Response;
                        response.metadata.recursion_available = true;
                        response.metadata.response_code = code;
                        server.send_to(&response.to_vec().expect("the DNS error must encode"), peer)
                            .await.expect("the server must send its DNS error");
                    }
                }
            }
        }).await.expect("an error DNS answer must finish before the test deadline")
    }

    #[tokio::test]
    async fn an_nxdomain_answer_is_a_missing_name() {
        let error = refused_lookup(hickory_resolver::proto::op::ResponseCode::NXDomain).await;
        assert_eq!(
            error.kind(),
            wisp_abi::native_io::DnsFailure::NotFound,
            "NXDOMAIN must remain a missing name instead of a policy denial: {error}"
        );
    }

    #[tokio::test]
    async fn a_servfail_answer_is_retryable() {
        let error = refused_lookup(hickory_resolver::proto::op::ResponseCode::ServFail).await;
        assert_eq!(
            error.kind(),
            wisp_abi::native_io::DnsFailure::Temporary,
            "SERVFAIL must remain a temporary resolver failure: {error}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_dns_query_expires_at_the_resolver_deadline() {
        let (mut resolver, server) = configured_server().await;
        resolver.options.attempts = 1;
        resolver.options.timeout = Duration::from_millis(100);
        let lookup = tokio::spawn(async move { resolver.resolve("timeout.test.", 443).await });
        let mut bytes = vec![0; 4096];
        server
            .recv_from(&mut bytes)
            .await
            .expect("the owned DNS server must observe the unanswered query");
        tokio::time::advance(Duration::from_millis(101)).await;
        let error = lookup
            .await
            .expect("the expired lookup must not panic")
            .expect_err("an unanswered DNS query must time out");
        assert_eq!(
            error.kind(),
            wisp_abi::native_io::DnsFailure::Timeout,
            "resolver expiry must retain its timeout classification: {error}"
        );
    }

    #[tokio::test]
    async fn ending_a_lookup_aborts_background_work_and_refuses_late_spawns() {
        let scope = LookupScope::default();
        let mut runtime = LookupRuntime {
            scope: scope.0.clone(),
            runtime: TokioRuntimeProvider::default(),
        };
        let (started, running) = tokio::sync::oneshot::channel();
        runtime.spawn_bg(async move {
            started
                .send(())
                .expect("the test must observe DNS work starting");
            std::future::pending::<()>().await;
        });
        running.await.expect("the DNS background work must start");
        drop(scope);
        let mut jobs = std::mem::take(&mut runtime.scope.lock().jobs);
        let result = jobs
            .join_next()
            .await
            .expect("the owned DNS task must be present")
            .expect_err("ending a lookup must cancel its background task");
        assert!(
            result.is_cancelled(),
            "the DNS task must be cancelled rather than panic"
        );
        runtime.spawn_bg(async { panic!("an abandoned DNS lookup must not start late work") });
        assert!(
            runtime.scope.lock().jobs.is_empty(),
            "closed lookup ownership must refuse background task registration"
        );
    }

    #[tokio::test]
    async fn a_late_dns_reply_cannot_restart_a_cancelled_lookup() {
        let (resolver, server) = configured_server().await;
        let scope = LookupScope::default();
        let tasks = scope.0.clone();
        let mut lookup = Box::pin(resolver.resolve_in_scope("late.test.", 443, &scope));
        let mut bytes = vec![0; 4096];
        let received = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::select! {
                result = &mut lookup => panic!("DNS must remain pending while the server withholds its answer: {result:?}"),
                received = server.recv_from(&mut bytes) => received,
            }
        }).await.expect("the query must reach the owned DNS server before the deadline")
            .expect("the DNS server must receive the pending query");
        drop(lookup);
        drop(scope);
        let mut jobs = std::mem::take(&mut tasks.lock().jobs);
        while let Some(result) = jobs.join_next().await {
            if let Err(error) = result {
                assert!(
                    error.is_cancelled(),
                    "DNS background work must end by cancellation rather than panic"
                );
            }
        }
        let mut answer =
            Message::from_vec(&bytes[..received.0]).expect("the pending query must be valid DNS");
        answer.metadata.message_type = MessageType::Response;
        answer.metadata.recursion_available = true;
        answer.add_answer(Record::from_rdata(
            answer.queries[0].name().clone(),
            60,
            RData::A(A(std::net::Ipv4Addr::new(93, 184, 216, 34))),
        ));
        server
            .send_to(
                &answer.to_vec().expect("the late answer must encode"),
                received.1,
            )
            .await
            .expect("the owned server must be able to send its late answer");
        let rebound = tokio::net::UdpSocket::bind(received.1)
            .await
            .expect("cancelling DNS must release the query's actual socket");
        assert_eq!(
            rebound
                .local_addr()
                .expect("the rebound socket must have an endpoint"),
            received.1,
            "the abandoned lookup must retain no DNS socket"
        );
        assert!(
            tasks.lock().closed,
            "a late reply must not reopen lookup ownership"
        );
    }
}
