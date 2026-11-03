//! Egress policy for `http_fetch`, enforced host-side: private and link-local
//! ranges denied, optional per-app allowlists, per-instance connection limits,
//! and resolve-then-pin. The check resolves once, validates
//! every address it got back, and returns a [`PinnedTarget`] carrying the
//! `SocketAddr` the connection uses, so DNS cannot move the target between
//! the check and the connect.

mod additional_destinations;
mod attempts;
mod connection;
mod dns;
mod quota;
pub use additional_destinations::AdditionalDestinations;
pub use connection::{ConnectionError, ConnectionFactory, ConnectionOwner, LocalConnections};
pub use dns::{DnsError, Resolver, SystemResolver};
pub use quota::{
    ConnectionCeilings, ConnectionLimit, ConnectionLimitProblem, ConnectionLimits,
    ConnectionLimitsError,
};

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::Instrument;

use isoplane_abi::host::PinnedTarget;

/// What one application may reach through the node's egress policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reach<'a> {
    /// Hosts the application restricts itself to; `None` leaves the node's policy to decide.
    pub allowlist: Option<&'a [String]>,
    /// Exact peers an operator lets this application reach where the node's policy refuses.
    pub exceptions: &'a [SocketAddr],
}

/// Authorizes concrete peers after name resolution and before any application data.
pub trait DestinationPolicy: Send + Sync + 'static {
    /// Refuse addresses outside the node's supported outbound capability.
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason>;

    /// Authorize a DNS answer before the guest chooses a destination port.
    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        PublicDestinations.authorize(SocketAddr::new(address, 0))
    }
}

/// The production policy: guests cannot reach private or internal destinations.
pub struct PublicDestinations;

impl DestinationPolicy for PublicDestinations {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if is_forbidden(address.ip()) {
            Err(DenyReason::PrivateAddress(address.ip()))
        } else {
            Ok(())
        }
    }
}

/// A local development session's policy: every destination, because the
/// machine and its network are the developer's own. The node never selects it.
pub struct LocalDevelopmentDestinations;

impl DestinationPolicy for LocalDevelopmentDestinations {
    fn authorize(&self, _address: SocketAddr) -> Result<(), DenyReason> {
        Ok(())
    }

    fn authorize_resolution(&self, _address: IpAddr) -> Result<(), DenyReason> {
        Ok(())
    }
}

/// A destination refusal or a resolver failure before a connection is acquired.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The requested destination violates policy or quota.
    #[error("{0}")]
    Denied(#[from] DenyReason),
    /// The DNS service failed independently of destination policy.
    #[error("{0}")]
    Dns(#[from] DnsError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// HTTP fetch accepts only HTTP and HTTPS URLs.
    Scheme(String),
    Malformed(String),
    /// The resolved address is in a range the platform never dials.
    PrivateAddress(IpAddr),
    /// An allowlist is configured and this host is not on it.
    NotAllowlisted(String),
    /// The instance already holds its limit of open connections.
    OpenConnectionLimit {
        /// Tenant of the refused instance.
        tenant: String,
        /// Instance that attempted the refused connection.
        instance: crate::InstanceId,
        /// The instance's open connection limit.
        limit: u64,
    },
}

impl std::error::Error for DenyReason {}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyReason::Scheme(s) => write!(f, "scheme `{s}` is not http(s)"),
            DenyReason::Malformed(s) => write!(f, "malformed url: {s}"),
            DenyReason::PrivateAddress(ip) => {
                write!(f, "target {ip} is private/link-local; denied by default")
            }
            DenyReason::NotAllowlisted(h) => {
                write!(f, "host `{h}` is not on the app's fetch allowlist")
            }
            DenyReason::OpenConnectionLimit {
                tenant,
                instance,
                limit,
            } => write!(
                f,
                "instance {instance} of tenant `{tenant}` already holds its limit of {limit} \
                 open outbound connections and was refused another"
            ),
        }
    }
}

/// Every range the platform refuses to dial.
pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || is_shared_cgnat(v4)
                || is_benchmark(v4)
                || v4.octets()[0] == 0
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local(v6)
                || is_v6_link_local(v6)
                // Judged as the v4 address it carries, or ::ffff:127.0.0.1
                // walks straight in.
                || v6.to_ipv4_mapped().map(|v| is_forbidden(IpAddr::V4(v))).unwrap_or(false)
                || v6.to_ipv4().map(|v| is_forbidden(IpAddr::V4(v))).unwrap_or(false)
        }
    }
}

/// 100.64/10, carrier NAT.
fn is_shared_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// 198.18/15.
fn is_benchmark(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 198 && (o[1] == 18 || o[1] == 19)
}

/// fc00::/7.
fn is_unique_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// fe80::/10.
fn is_v6_link_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

/// Parses an HTTP(S) fetch URL, refusing credentials in its authority.
pub(crate) fn parse_url(url: &str) -> Result<url::Url, DenyReason> {
    // Empty userinfo is normalized away, but fetch policy refuses it too.
    let has_userinfo = std::cell::Cell::new(false);
    let on_violation = |violation| {
        if violation == url::SyntaxViolation::EmbeddedCredentials {
            has_userinfo.set(true);
        }
    };
    let parsed = url::Url::options()
        .syntax_violation_callback(Some(&on_violation))
        .parse(url)
        .map_err(|error| DenyReason::Malformed(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(DenyReason::Scheme(parsed.scheme().to_owned()));
    }
    if has_userinfo.get() {
        return Err(DenyReason::Malformed(
            "userinfo is not permitted in a fetch url".into(),
        ));
    }
    Ok(parsed)
}

pub struct HostEgressPolicy {
    connections_factory: Arc<dyn ConnectionFactory>,
    resolver: Box<dyn Resolver>,
    destinations: Arc<dyn DestinationPolicy>,
    open: quota::OpenConnections,
    pending: Arc<attempts::PendingConnections>,
    resolutions: AtomicU64,
}

/// Counts one open connection against its instance until the host socket is released.
pub(crate) struct ConnectionPermit {
    policy: Arc<HostEgressPolicy>,
    holding: quota::Holding,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.policy.open.release(&self.holding);
    }
}

/// A connection one instance has in progress, holding its open permit.
/// Dropping it releases both and lets the instance's next queued attempt
/// start; ending it once established releases only its place in the queue.
pub struct ConnectAttempt {
    policy: Arc<HostEgressPolicy>,
    owner: ConnectionOwner,
    // Before the slot, which drops after it: a failed attempt's permit is
    // returned before the next queued attempt is admitted to take one.
    permit: ConnectionPermit,
    _slot: attempts::Attempt,
}

impl ConnectAttempt {
    /// Opens host-owned TCP to an authorized destination.
    pub(crate) async fn connect(
        &self,
        destination: SocketAddr,
    ) -> Result<tokio::net::TcpStream, ConnectionError> {
        self.policy.connect(&self.owner, destination).await
    }

    /// Ends the attempt and returns its open permit, for the host socket to hold.
    pub(crate) fn established(self) -> ConnectionPermit {
        self.permit
    }
}

impl HostEgressPolicy {
    /// Resolve an allowed name and refuse the complete answer if any address is forbidden.
    pub(crate) async fn resolve_name(
        &self,
        name: &str,
        reach: Reach<'_>,
    ) -> Result<Vec<IpAddr>, EgressError> {
        if !Self::allowlisted(name, reach.allowlist) {
            return Err(DenyReason::NotAllowlisted(name.to_owned()).into());
        }
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addresses = self.resolver.resolve(name, 0).await?;
        if addresses.is_empty() {
            return Err(DnsError::NoAddress.into());
        }
        for address in &addresses {
            if !reach
                .exceptions
                .iter()
                .any(|exception| exception.ip() == address.ip())
            {
                self.destinations.authorize_resolution(address.ip())?;
            }
        }
        Ok(addresses.into_iter().map(|address| address.ip()).collect())
    }

    /// Select where both native TCP and HTTP sockets are opened.
    pub fn with_connections(mut self, connections: Arc<dyn ConnectionFactory>) -> Self {
        self.connections_factory = connections;
        self
    }

    /// Waits, first in, first out, until the owner's instance has fewer than its
    /// limit of connections in progress, then takes its open permit, refusing
    /// when the instance already holds its limit of open connections.
    ///
    /// A waiting attempt holds no open permit, and a refused one returns its
    /// place in the queue, so neither can hold anything an instance must wait
    /// for. Dropping the future while it waits leaves the queue.
    pub async fn begin_connect(
        self: &Arc<Self>,
        owner: ConnectionOwner,
    ) -> Result<ConnectAttempt, ConnectionError> {
        let slot = self
            .pending
            .begin(owner.instance())
            .instrument(tracing::info_span!("connection_queue"))
            .await
            .map_err(std::io::Error::other)?;
        let permit = ConnectionPermit {
            holding: self.open.acquire(&owner)?,
            policy: self.clone(),
        };
        Ok(ConnectAttempt {
            policy: self.clone(),
            owner,
            permit,
            _slot: slot,
        })
    }

    async fn connect(
        &self,
        owner: &ConnectionOwner,
        destination: SocketAddr,
    ) -> Result<tokio::net::TcpStream, ConnectionError> {
        self.authorize(destination, owner.exceptions())?;
        let stream = self.connections_factory.connect(owner, destination).await?;
        let peer = stream.peer_addr()?;
        if connection::canonical_destination(peer)?
            != connection::canonical_destination(destination)?
        {
            return Err(ConnectionError::PeerMismatch {
                expected: destination,
                actual: peer,
            });
        }
        self.authorize(peer, owner.exceptions())?;
        Ok(stream)
    }

    /// Select the address policy used by every checked destination.
    pub fn with_destinations(mut self, destinations: Arc<dyn DestinationPolicy>) -> Self {
        self.destinations = destinations;
        self
    }

    fn authorize(&self, address: SocketAddr, exceptions: &[SocketAddr]) -> Result<(), DenyReason> {
        if exceptions.contains(&address) {
            Ok(())
        } else {
            self.destinations.authorize(address)
        }
    }

    /// Checks a numeric socket destination, without performing DNS.
    pub(crate) fn check_socket(
        &self,
        address: SocketAddr,
        reach: Reach<'_>,
    ) -> Result<(), DenyReason> {
        self.authorize(address, reach.exceptions)?;
        let host = address.ip().to_string();
        if !Self::allowlisted(&host, reach.allowlist) {
            return Err(DenyReason::NotAllowlisted(host));
        }
        Ok(())
    }

    /// Creates an egress policy enforcing `limits`.
    pub fn new(resolver: Box<dyn Resolver>, limits: ConnectionLimits) -> Self {
        Self {
            connections_factory: Arc::new(LocalConnections),
            resolver,
            destinations: Arc::new(PublicDestinations),
            open: quota::OpenConnections::new(limits),
            pending: Arc::new(attempts::PendingConnections::new(
                limits.ceilings().in_progress,
            )),
            resolutions: AtomicU64::new(0),
        }
    }

    /// Creates an egress policy using the system DNS resolver.
    pub fn system(limits: ConnectionLimits) -> Result<Self, DnsError> {
        Ok(Self::new(Box::new(SystemResolver::new()?), limits))
    }

    pub fn resolutions(&self) -> u64 {
        self.resolutions.load(Ordering::Relaxed)
    }

    /// Open outbound connections `instance` holds.
    pub fn instance_connections(&self, instance: crate::InstanceId) -> u64 {
        self.open.instance(instance)
    }

    fn allowlisted(host: &str, allowlist: Option<&[String]>) -> bool {
        let Some(list) = allowlist else { return true };
        list.iter().any(|pat| {
            let pat = pat.trim().to_ascii_lowercase();
            match pat.strip_prefix("*.") {
                Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
                None => match (host.parse::<IpAddr>(), pat.parse::<IpAddr>()) {
                    (Ok(address), Ok(allowed)) => address.to_canonical() == allowed.to_canonical(),
                    _ => host == pat,
                },
            }
        })
    }

    /// Checks a URL's destination and pins the address the connection must use.
    pub(crate) async fn check_url(
        &self,
        url: &str,
        reach: Reach<'_>,
    ) -> Result<PinnedTarget, EgressError> {
        let parsed = parse_url(url)?;
        let host = match parsed.host() {
            Some(url::Host::Domain(domain)) => domain.to_owned(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Ipv6(address)) => address.to_string(),
            None => return Err(DenyReason::Malformed("empty host".into()).into()),
        };
        if !Self::allowlisted(&host, reach.allowlist) {
            return Err(DenyReason::NotAllowlisted(host).into());
        }
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| DenyReason::Malformed("missing port".into()))?;
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addrs = self.resolver.resolve(&host, port).await?;
        if addrs.is_empty() {
            return Err(DnsError::NoAddress.into());
        }
        // Every address, not the first: a client that falls back to a second
        // address would otherwise reach a private one.
        for address in &addrs {
            self.authorize(*address, reach.exceptions)?;
        }
        Ok(PinnedTarget {
            url: url.to_string(),
            addr: addrs[0],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::net::Ipv4Addr;

    /// Answers differently on each call, the shape of a DNS rebinding attack.
    struct Rebinder {
        answers: Mutex<Vec<Vec<SocketAddr>>>,
        calls: AtomicU64,
    }
    impl Rebinder {
        fn new(answers: Vec<Vec<SocketAddr>>) -> Self {
            Self {
                answers: Mutex::new(answers),
                calls: AtomicU64::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl Resolver for Rebinder {
        async fn resolve(
            &self,
            _host: &str,
            _port: u16,
        ) -> Result<Vec<SocketAddr>, crate::egress::DnsError> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
            let a = self.answers.lock();
            Ok(a.get(n).or_else(|| a.last()).cloned().unwrap_or_default())
        }
    }

    struct Fixed(Vec<SocketAddr>);
    #[async_trait::async_trait]
    impl Resolver for Fixed {
        async fn resolve(
            &self,
            _h: &str,
            _p: u16,
        ) -> Result<Vec<SocketAddr>, crate::egress::DnsError> {
            Ok(self.0.clone())
        }
    }

    fn sa(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)
    }

    /// A reach restricted to `list`, with no exceptions.
    fn only(list: &[String]) -> Reach<'_> {
        Reach {
            allowlist: Some(list),
            exceptions: &[],
        }
    }

    fn policy(r: impl Resolver) -> HostEgressPolicy {
        HostEgressPolicy::new(
            Box::new(r),
            crate::egress::ConnectionLimits::uniform(100).expect("connection limits"),
        )
    }

    struct CountingFailure(Arc<AtomicU64>);
    #[async_trait::async_trait]
    impl Resolver for CountingFailure {
        async fn resolve(&self, host: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(DnsError::Lookup {
                host: host.to_owned(),
                kind: isoplane_abi::native_io::DnsFailure::Temporary,
                message: "DNS server returned SERVFAIL".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_disallowed_dns_name_is_refused_before_the_resolver_can_run() {
        let calls = Arc::new(AtomicU64::new(0));
        let policy = policy(CountingFailure(calls.clone()));
        let result = policy
            .resolve_name("forbidden.test", only(&["allowed.test".into()]))
            .await;
        assert!(
            matches!(result, Err(EgressError::Denied(DenyReason::NotAllowlisted(ref host))) if host == "forbidden.test"),
            "a forbidden hostname must be denied before a resolver failure could be exposed"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "denied names must never reach DNS"
        );
        assert_eq!(
            policy.resolutions(),
            0,
            "a denied name must start no lookup"
        );
    }

    #[tokio::test]
    async fn an_allowed_name_preserves_servfail_after_the_name_policy_passes() {
        let calls = Arc::new(AtomicU64::new(0));
        let policy = policy(CountingFailure(calls.clone()));
        let result = policy
            .resolve_name("allowed.test", only(&["allowed.test".into()]))
            .await;
        assert!(
            matches!(result, Err(EgressError::Dns(error)) if error.kind() == isoplane_abi::native_io::DnsFailure::Temporary),
            "SERVFAIL for an allowed name must remain a retryable resolver failure"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "an allowed name must reach DNS exactly once"
        );
    }

    #[tokio::test]
    async fn private_and_link_local_are_denied() {
        for ip in [
            [10, 0, 0, 1],
            [172, 16, 5, 5],
            [192, 168, 1, 1],
            [127, 0, 0, 1],
            [169, 254, 169, 254],
            [100, 64, 0, 1],
            [0, 0, 0, 0],
        ] {
            let p = policy(Fixed(vec![sa(ip, 80)]));
            let err = p
                .check_url("http://target.example/", Reach::default())
                .await
                .expect_err("a private address must be refused");
            assert!(
                matches!(err, EgressError::Denied(DenyReason::PrivateAddress(_))),
                "{ip:?} was not denied: {err}"
            );
        }
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 80)]));
        assert!(
            p.check_url("http://example.com/", Reach::default())
                .await
                .is_ok(),
            "a public address is dialable"
        );
    }

    #[tokio::test]
    async fn ipv6_loopback_ula_and_v4_mapped_are_denied() {
        for lit in ["[::1]", "[fc00::1]", "[fe80::1]", "[::ffff:127.0.0.1]"] {
            let ip: IpAddr = lit.trim_matches(['[', ']']).parse().expect("literal");
            let p = policy(Fixed(vec![SocketAddr::new(ip, 80)]));
            let err = p
                .check_url(&format!("http://{lit}/"), Reach::default())
                .await
                .expect_err("a private v6 address must be refused");
            assert!(
                matches!(err, EgressError::Denied(DenyReason::PrivateAddress(_))),
                "{lit} allowed: {err}"
            );
        }
    }

    #[tokio::test]
    async fn dns_rebinding_cannot_move_the_target_after_the_check() {
        let p = policy(Rebinder::new(vec![
            vec![sa([93, 184, 216, 34], 443)],
            vec![sa([127, 0, 0, 1], 443)],
        ]));
        let pinned = p
            .check_url("https://rebind.example/", Reach::default())
            .await
            .expect("the first answer is public");
        assert_eq!(
            pinned.addr,
            sa([93, 184, 216, 34], 443),
            "the pinned address is the one that was checked"
        );
        assert_eq!(p.resolutions(), 1, "the policy must resolve exactly once");
        assert_ne!(
            pinned.addr.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "the second DNS answer has nothing left to move"
        );
    }

    #[tokio::test]
    async fn a_mixed_answer_is_denied_entirely() {
        let p = policy(Fixed(vec![
            sa([93, 184, 216, 34], 80),
            sa([192, 168, 0, 5], 80),
        ]));
        let err = p
            .check_url("http://mixed.example/", Reach::default())
            .await
            .expect_err("an answer with a private address must be refused");
        assert!(
            matches!(err, EgressError::Denied(DenyReason::PrivateAddress(ip)) if ip == IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))),
            "the answer is refused outright, not narrowed to the public address"
        );
    }

    #[tokio::test]
    async fn only_http_and_https_egress_exists() {
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 80)]));
        for url in ["file:///etc/passwd", "gopher://x/", "ftp://x/"] {
            assert!(
                matches!(
                    p.check_url(url, Reach::default()).await,
                    Err(EgressError::Denied(DenyReason::Scheme(_)))
                ),
                "{url} is not dialable"
            );
        }
        assert!(
            matches!(
                p.check_url("http://user:pw@example.com/", Reach::default())
                    .await,
                Err(EgressError::Denied(DenyReason::Malformed(_)))
            ),
            "userinfo in the authority is refused"
        );
    }

    #[tokio::test]
    async fn malformed_authorities_and_credentials_are_refused_before_dns() {
        let calls = Arc::new(AtomicU64::new(0));
        let policy = policy(CountingFailure(calls.clone()));
        for url in [
            "http://[::1]junk:1234/",
            "http://[::1]junk/",
            "http://[not-an-ip]:1234/",
            "http://[::1/",
            "http://example.com:65536/",
            "http://user:password@example.com/",
            "http://user@example.com/",
            "http://@example.com/",
            "http://:@example.com/",
        ] {
            let result = policy.check_url(url, Reach::default()).await;
            assert!(
                matches!(result, Err(EgressError::Denied(DenyReason::Malformed(_)))),
                "{url} must be refused as malformed before DNS: {result:?}"
            );
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "invalid URLs must never reach DNS"
        );
    }

    #[tokio::test]
    async fn normalized_url_hosts_and_ports_reach_the_allowlist_and_resolver() {
        struct RecordingResolver(Arc<Mutex<Vec<(String, u16)>>>);

        #[async_trait::async_trait]
        impl Resolver for RecordingResolver {
            async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError> {
                self.0.lock().push((host.to_owned(), port));
                Ok(vec![sa([8, 8, 8, 8], port)])
            }
        }

        let queries = Arc::new(Mutex::new(Vec::new()));
        let policy = policy(RecordingResolver(queries.clone()));
        let mut expected = Vec::new();
        for (url, host, port) in [
            ("https://EXAMPLE.com/", "example.com", 443),
            (
                "http://[2001:4860:4860::8888]:8080/",
                "2001:4860:4860::8888",
                8080,
            ),
            ("http://0x08080808/", "8.8.8.8", 80),
        ] {
            let target = policy
                .check_url(url, only(&[host.to_owned()]))
                .await
                .expect("the normalized host is allowlisted");
            assert_eq!(
                target.addr,
                sa([8, 8, 8, 8], port),
                "the URL's port must be pinned"
            );
            expected.push((host.to_owned(), port));
        }
        assert_eq!(
            *queries.lock(),
            expected,
            "policy and DNS must use the same normalized hosts, with IPv6 brackets removed"
        );
    }

    #[test]
    fn numeric_allowlists_match_only_the_same_ipv4_or_mapped_address() {
        let policy = policy(Fixed(vec![]));
        for (allowed, destination) in [
            ("93.184.216.34", "::ffff:93.184.216.34"),
            ("::ffff:93.184.216.34", "93.184.216.34"),
        ] {
            let address = SocketAddr::new(destination.parse().expect("fixture IP"), 443);
            assert!(policy.check_socket(address, only(&[allowed.to_owned()])).is_ok(), "numeric allowlist must admit its exact IPv4/mapped equivalent: {allowed} -> {destination}");
        }
        for destination in [
            "93.184.216.35",
            "::ffff:93.184.216.35",
            "::93.184.216.34",
            "64:ff9b::5db8:d822",
        ] {
            let address = SocketAddr::new(destination.parse().expect("fixture IP"), 443);
            assert!(matches!(policy.check_socket(address, only(&["93.184.216.34".to_owned()])), Err(DenyReason::NotAllowlisted(_))),
                "numeric equivalence must not authorize a different or merely embedded address: {destination}");
        }
    }

    #[test]
    fn numeric_allowlist_equivalence_cannot_authorize_private_destinations() {
        let policy = policy(Fixed(vec![]));
        let destination = "[::ffff:127.0.0.1]:443"
            .parse()
            .expect("fixture socket address");
        assert!(
            matches!(
                policy.check_socket(destination, only(&["127.0.0.1".to_owned()])),
                Err(DenyReason::PrivateAddress(_))
            ),
            "an equivalent numeric allowlist must not bypass the private destination policy"
        );
    }

    #[tokio::test]
    async fn allowlist_supports_exact_and_wildcard_hosts() {
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 443)]));
        let list = vec!["api.example.com".to_string(), "*.cdn.example".to_string()];
        assert!(
            p.check_url("https://api.example.com/x", only(&list))
                .await
                .is_ok(),
            "an exact entry matches"
        );
        assert!(
            p.check_url("https://a.cdn.example/x", only(&list))
                .await
                .is_ok(),
            "a wildcard matches a subdomain"
        );
        assert!(
            p.check_url("https://cdn.example/x", only(&list))
                .await
                .is_ok(),
            "a wildcard matches its bare suffix"
        );
        let err = p
            .check_url("https://evil.example/x", only(&list))
            .await
            .expect_err("a host off the list is refused");
        assert!(
            matches!(err, EgressError::Denied(DenyReason::NotAllowlisted(ref host)) if host == "evil.example"),
            "the refusal names the host"
        );
        assert!(
            p.check_url("https://notcdn.example/x", only(&list))
                .await
                .is_err(),
            "`*.cdn.example` is not a raw suffix match"
        );
    }

    fn owner(tenant: &str, instance: u64) -> ConnectionOwner {
        ConnectionOwner::new(tenant.into(), crate::InstanceId(instance)).expect("fixture owner")
    }

    fn limited(in_progress: u64, open: u64) -> Arc<HostEgressPolicy> {
        Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec![])),
            ConnectionLimits::new(ConnectionCeilings { in_progress, open })
                .expect("fixture limits"),
        ))
    }

    fn refused(tenant: &str, instance: u64, limit: u64) -> DenyReason {
        DenyReason::OpenConnectionLimit {
            tenant: tenant.into(),
            instance: crate::InstanceId(instance),
            limit,
        }
    }

    fn denial<T>(result: Result<T, ConnectionError>) -> Option<DenyReason> {
        match result {
            Err(ConnectionError::Denied(reason)) => Some(reason),
            Ok(_) | Err(ConnectionError::Io(_) | ConnectionError::PeerMismatch { .. }) => None,
        }
    }

    #[tokio::test]
    async fn an_instance_at_its_open_limit_is_refused_and_another_instance_is_not() {
        let policy = limited(2, 2);
        let established = policy
            .begin_connect(owner("a", 1))
            .await
            .expect("the instance's first connection")
            .established();
        let in_progress = policy
            .begin_connect(owner("a", 1))
            .await
            .expect("the instance's second connection");
        assert_eq!(
            denial(policy.begin_connect(owner("a", 1)).await),
            Some(refused("a", 1, 2)),
            "the instance's third open connection is over its limit"
        );
        assert_eq!(
            policy.instance_connections(crate::InstanceId(1)),
            2,
            "a refusal takes no open permit"
        );
        let mut again = std::pin::pin!(policy.begin_connect(owner("a", 1)));
        assert!(
            matches!(poll_once(again.as_mut()), std::task::Poll::Ready(Err(ConnectionError::Denied(_)))),
            "a refused attempt returns its place in the queue, so the next is refused at once instead of waiting"
        );
        let sibling = policy
            .begin_connect(owner("a", 2))
            .await
            .expect("another instance of the same tenant has its own limit")
            .established();
        let stranger = policy
            .begin_connect(owner("b", 3))
            .await
            .expect("another tenant's instance has its own limit")
            .established();
        drop(established);
        assert_eq!(policy.instance_connections(crate::InstanceId(1)), 1);
        let replacement = policy
            .begin_connect(owner("a", 1))
            .await
            .expect("a released permit admits the instance again");
        drop((in_progress, replacement, sibling, stranger));
        for instance in [1, 2, 3] {
            assert_eq!(
                policy.instance_connections(crate::InstanceId(instance)),
                0,
                "every permit of instance {instance} is returned"
            );
        }
        assert_eq!(
            policy.open.tracked(),
            0,
            "instances without connections keep no entry"
        );
        assert_eq!(policy.pending.tracked(), 0, "and no queue");
    }

    #[tokio::test]
    async fn application_connection_caps_are_bounded_by_the_host_and_include_pending_attempts() {
        for (application_limit, effective_limit) in [(1, 1), (3, 2)] {
            let policy = limited(2, 2);
            let limits = isoplane_abi::config::Limits {
                instance_open_connections: application_limit,
                ..Default::default()
            };
            let owner = owner("a", 1).with_application_limits(&limits);
            let mut held = Vec::new();
            for _ in 0..effective_limit {
                held.push(
                    policy
                        .begin_connect(owner.clone())
                        .await
                        .expect("within cap"),
                );
            }
            // Release the attempt queue slots while retaining open permits.
            let established: Vec<_> = held
                .into_iter()
                .map(|permit| permit.established())
                .collect();
            assert_eq!(
                denial(policy.begin_connect(owner.clone()).await),
                Some(refused("a", 1, effective_limit))
            );
            drop(established);
            let pending = policy
                .begin_connect(owner.clone())
                .await
                .expect("released capacity");
            assert_eq!(
                policy.instance_connections(owner.instance()),
                1,
                "an unfinished attempt already counts"
            );
            drop(pending);
            assert_eq!(
                policy.instance_connections(owner.instance()),
                0,
                "cancellation frees capacity"
            );
        }
    }

    #[test]
    fn refusals_name_the_instance_its_tenant_and_the_limit() {
        assert_eq!(
            refused("acme", 7, 32).to_string(),
            "instance i7 of tenant `acme` already holds its limit of 32 open outbound \
             connections and was refused another"
        );
    }

    #[tokio::test]
    async fn cancelling_a_task_that_holds_an_open_permit_releases_it() {
        let policy = limited(1, 1);
        let attempt = policy
            .begin_connect(owner("a", 1))
            .await
            .expect("the first connection");
        let (started, running) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _attempt = attempt;
            started
                .send(())
                .expect("the test must observe connection ownership");
            std::future::pending::<()>().await;
        });
        running.await.expect("the connection task must start");
        assert_eq!(
            policy.instance_connections(crate::InstanceId(1)),
            1,
            "the live attempt must hold its permit"
        );
        task.abort();
        let error = task.await.expect_err("aborted connection work must end");
        assert!(
            error.is_cancelled(),
            "connection cancellation must not panic"
        );
        assert_eq!(
            policy.instance_connections(crate::InstanceId(1)),
            0,
            "cancellation must release the open permit"
        );
        assert_eq!(policy.pending.tracked(), 0, "and the place in the queue");
    }

    /// Holds each connect until the test opens its gate, announcing it in arrival order.
    struct Gated(tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>);

    #[async_trait::async_trait]
    impl ConnectionFactory for Gated {
        async fn connect(
            &self,
            _: &ConnectionOwner,
            destination: SocketAddr,
        ) -> std::io::Result<tokio::net::TcpStream> {
            let (open, gate) = tokio::sync::oneshot::channel();
            self.0.send(open).map_err(std::io::Error::other)?;
            gate.await.map_err(std::io::Error::other)?;
            tokio::net::TcpStream::connect(destination).await
        }
    }

    fn poll_once<F: std::future::Future>(
        future: std::pin::Pin<&mut F>,
    ) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    async fn open(
        policy: Arc<HostEgressPolicy>,
        owner: ConnectionOwner,
        destination: SocketAddr,
    ) -> Result<(tokio::net::TcpStream, ConnectionPermit), ConnectionError> {
        let attempt = policy.begin_connect(owner).await?;
        let stream = attempt.connect(destination).await?;
        Ok((stream, attempt.established()))
    }

    #[tokio::test]
    async fn an_attempt_beyond_the_pending_limit_waits_for_one_to_finish() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback peer");
        let peer = listener.local_addr().expect("peer address");
        let (arrivals, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let policy = Arc::new(
            HostEgressPolicy::new(
                Box::new(Fixed(vec![])),
                ConnectionLimits::new(ConnectionCeilings {
                    in_progress: 6,
                    open: 16,
                })
                .expect("fixture limits"),
            )
            .with_connections(Arc::new(Gated(arrivals)))
            .with_destinations(Arc::new(LocalDevelopmentDestinations)),
        );
        let mut attempts: Vec<_> = (0..7)
            .map(|_| Box::pin(open(policy.clone(), owner("a", 1), peer)))
            .collect();
        for attempt in &mut attempts {
            assert!(poll_once(attempt.as_mut()).is_pending());
        }
        let mut gates: Vec<_> = (0..6)
            .map(|index| {
                arrived
                    .try_recv()
                    .unwrap_or_else(|_| panic!("attempt {index} is in progress"))
            })
            .collect();
        assert!(
            arrived.try_recv().is_err(),
            "the seventh attempt waits instead of connecting"
        );
        assert_eq!(
            policy.instance_connections(crate::InstanceId(1)),
            6,
            "a waiting attempt holds no open permit"
        );
        let mut other = Box::pin(open(policy.clone(), owner("a", 2), peer));
        assert!(poll_once(other.as_mut()).is_pending());
        let other_gate = arrived
            .try_recv()
            .expect("another instance's attempt is not queued behind this one's");
        gates
            .remove(0)
            .send(())
            .expect("the first attempt waits at its gate");
        let first = attempts.remove(0);
        let first = first.await.expect("the first attempt connects");
        assert!(poll_once(attempts[5].as_mut()).is_pending());
        let seventh_gate = arrived
            .try_recv()
            .expect("a finished attempt lets the seventh start");
        let mut established = vec![first];
        for gate in gates.into_iter().chain([seventh_gate, other_gate]) {
            gate.send(()).expect("an attempt waits at its gate");
        }
        for attempt in attempts.into_iter().chain([other]) {
            established.push(attempt.await.expect("every queued attempt connects"));
        }
        let mut next = Box::pin(open(policy.clone(), owner("a", 1), peer));
        assert!(poll_once(next.as_mut()).is_pending());
        assert!(
            arrived.try_recv().is_ok(),
            "established connections stop counting, so {} of them do not delay a new attempt",
            established.len()
        );
    }

    #[test]
    fn urls_parse_the_way_the_policy_assumes() {
        let parsed = parse_url("https://example.com/a?b=c").expect("parses");
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("example.com"));
        assert_eq!(parsed.port_or_known_default(), Some(443));
        assert_eq!(
            &parsed[url::Position::BeforePath..url::Position::AfterQuery],
            "/a?b=c"
        );
        assert_eq!(
            parse_url("http://example.com:8080").expect("parses").port(),
            Some(8080),
            "an explicit port is kept"
        );
        assert_eq!(
            parse_url("http://[::1]:9/").expect("parses").host(),
            Some(url::Host::Ipv6(Ipv6Addr::LOCALHOST)),
            "an IPv6 host remains an address rather than its bracketed serialization"
        );
        assert_eq!(
            parse_url("http://EXAMPLE.com/").expect("parses").host_str(),
            Some("example.com"),
            "hosts are lower-cased"
        );
    }
}
