//! Egress policy for `http_fetch`, enforced host-side: private and link-local
//! ranges denied, optional per-app allowlists, connection quotas,
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
    ConnectionCeilings, ConnectionLevel, ConnectionLimit, ConnectionLimitProblem, ConnectionLimits,
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
    /// The owner's tenant, or the node, already holds its limit of open connections.
    ConnectionQuota {
        /// The level that is full.
        level: ConnectionLevel,
        /// Tenant of the refused connection.
        tenant: String,
        /// Instance that attempted the refused connection.
        instance: crate::InstanceId,
        /// The full level's limit.
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
            DenyReason::ConnectionQuota {
                level: ConnectionLevel::Tenant,
                tenant,
                instance,
                limit,
            } => write!(
                f,
                "tenant `{tenant}` already holds its limit of {limit} open outbound connections; \
                 instance {instance} was refused another"
            ),
            DenyReason::ConnectionQuota {
                level: ConnectionLevel::Node,
                tenant,
                instance,
                limit,
            } => write!(
                f,
                "the node already holds its limit of {limit} open outbound connections; \
                 instance {instance} of tenant `{tenant}` was refused another"
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path_and_query: String,
}

pub fn parse_url(url: &str) -> Result<ParsedUrl, DenyReason> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| DenyReason::Malformed("missing scheme".into()))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(DenyReason::Scheme(scheme));
    }
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(DenyReason::Malformed("empty authority".into()));
    }
    // Credentials in the authority buy a tenant nothing a header cannot.
    if authority.contains('@') {
        return Err(DenyReason::Malformed(
            "userinfo is not permitted in a fetch url".into(),
        ));
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (h, rest) = inner
            .split_once(']')
            .ok_or_else(|| DenyReason::Malformed("unterminated ipv6 literal".into()))?;
        let port = match rest.strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| DenyReason::Malformed("bad port".into()))?,
            None => default_port,
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse()
                    .map_err(|_| DenyReason::Malformed("bad port".into()))?,
            ),
            None => (authority.to_string(), default_port),
        }
    };
    if host.is_empty() {
        return Err(DenyReason::Malformed("empty host".into()));
    }
    Ok(ParsedUrl {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        path_and_query: path.to_string(),
    })
}

pub struct HostEgressPolicy {
    connections_factory: Arc<dyn ConnectionFactory>,
    resolver: Box<dyn Resolver>,
    destinations: Arc<dyn DestinationPolicy>,
    open: quota::OpenConnections,
    pending: Arc<attempts::PendingConnections>,
    resolutions: AtomicU64,
}

/// Holds an open connection's tenant and node permits until the host socket is released.
pub(crate) struct ConnectionPermit {
    policy: Arc<HostEgressPolicy>,
    holding: quota::Holding,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.policy.open.release(&self.holding);
    }
}

/// A checked destination whose permits are released on cancellation too.
pub(crate) struct CheckedConnection {
    target: PinnedTarget,
    _permit: ConnectionPermit,
}

impl CheckedConnection {
    /// The resolved destination the transport must use.
    pub(crate) fn target(&self) -> PinnedTarget {
        self.target.clone()
    }
}

/// A connection in progress for one owner. Dropping it, once the connection is
/// established or has failed, lets the instance's next queued attempt start.
pub(crate) struct ConnectAttempt {
    policy: Arc<HostEgressPolicy>,
    owner: ConnectionOwner,
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
    /// limit of connections in progress.
    pub(crate) async fn begin_connect(
        self: &Arc<Self>,
        owner: ConnectionOwner,
    ) -> Result<ConnectAttempt, ConnectionError> {
        let slot = self
            .pending
            .begin(owner.instance())
            .instrument(tracing::info_span!("connection_queue"))
            .await
            .map_err(std::io::Error::other)?;
        Ok(ConnectAttempt {
            policy: self.clone(),
            owner,
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

    /// Takes `owner`'s tenant and node permits for a checked numeric socket
    /// destination, without performing DNS.
    pub(crate) fn check_socket(
        self: &Arc<Self>,
        address: SocketAddr,
        owner: &ConnectionOwner,
        reach: Reach<'_>,
    ) -> Result<ConnectionPermit, DenyReason> {
        self.authorize(address, reach.exceptions)?;
        let host = address.ip().to_string();
        if !Self::allowlisted(&host, reach.allowlist) {
            return Err(DenyReason::NotAllowlisted(host));
        }
        self.permit(owner)
    }

    /// Checks a URL's destination and takes `owner`'s tenant and node permits,
    /// which are released when the returned connection is dropped.
    pub(crate) async fn check_owned(
        self: &Arc<Self>,
        url: &str,
        owner: &ConnectionOwner,
        reach: Reach<'_>,
    ) -> Result<CheckedConnection, EgressError> {
        let target = self.check_url(url, reach).await?;
        Ok(CheckedConnection {
            target,
            _permit: self.permit(owner)?,
        })
    }

    fn permit(self: &Arc<Self>, owner: &ConnectionOwner) -> Result<ConnectionPermit, DenyReason> {
        Ok(ConnectionPermit {
            holding: self.open.acquire(owner)?,
            policy: self.clone(),
        })
    }

    /// Creates an egress policy enforcing `limits`.
    pub fn new(resolver: Box<dyn Resolver>, limits: ConnectionLimits) -> Self {
        Self {
            connections_factory: Arc::new(LocalConnections),
            resolver,
            destinations: Arc::new(PublicDestinations),
            open: quota::OpenConnections::new(limits),
            pending: Arc::new(attempts::PendingConnections::new(
                limits.ceilings().instance_pending,
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

    /// Open outbound connections on the node.
    pub fn connections(&self) -> u64 {
        self.open.node()
    }

    /// Open outbound connections of `tenant`'s instances together.
    pub fn tenant_connections(&self, tenant: &str) -> u64 {
        self.open.tenant(tenant)
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

    async fn check_url(&self, url: &str, reach: Reach<'_>) -> Result<PinnedTarget, EgressError> {
        let parsed = parse_url(url)?;
        if !Self::allowlisted(&parsed.host, reach.allowlist) {
            return Err(DenyReason::NotAllowlisted(parsed.host).into());
        }
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addrs = self.resolver.resolve(&parsed.host, parsed.port).await?;
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
        assert_eq!(
            policy.connections(),
            0,
            "a denied name must acquire no connection"
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
        assert_eq!(
            policy.connections(),
            0,
            "a failed allowed lookup must acquire no connection"
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

    #[test]
    fn numeric_allowlists_match_only_the_same_ipv4_or_mapped_address() {
        let policy = Arc::new(policy(Fixed(vec![])));
        for (allowed, destination) in [
            ("93.184.216.34", "::ffff:93.184.216.34"),
            ("::ffff:93.184.216.34", "93.184.216.34"),
        ] {
            let address = SocketAddr::new(destination.parse().expect("fixture IP"), 443);
            let permit = policy.check_socket(address, &owner("a", 1), only(&[allowed.to_owned()]));
            assert!(permit.is_ok(), "numeric allowlist must admit its exact IPv4/mapped equivalent: {allowed} -> {destination}");
            drop(permit);
        }
        for destination in [
            "93.184.216.35",
            "::ffff:93.184.216.35",
            "::93.184.216.34",
            "64:ff9b::5db8:d822",
        ] {
            let address = SocketAddr::new(destination.parse().expect("fixture IP"), 443);
            assert!(matches!(policy.check_socket(address, &owner("a", 1), only(&["93.184.216.34".to_owned()])), Err(DenyReason::NotAllowlisted(_))),
                "numeric equivalence must not authorize a different or merely embedded address: {destination}");
        }
        assert_eq!(
            policy.connections(),
            0,
            "numeric policy checks must release all admitted permits"
        );
    }

    #[test]
    fn numeric_allowlist_equivalence_cannot_authorize_private_destinations() {
        let policy = Arc::new(policy(Fixed(vec![])));
        let destination = "[::ffff:127.0.0.1]:443"
            .parse()
            .expect("fixture socket address");
        assert!(
            matches!(
                policy.check_socket(destination, &owner("a", 1), only(&["127.0.0.1".to_owned()])),
                Err(DenyReason::PrivateAddress(_))
            ),
            "an equivalent numeric allowlist must not bypass the private destination policy"
        );
        assert_eq!(
            policy.connections(),
            0,
            "private destinations must acquire no connection permit"
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

    fn limited(resolver: impl Resolver, node: u64, tenant: u64, pending: u64) -> HostEgressPolicy {
        HostEgressPolicy::new(
            Box::new(resolver),
            ConnectionLimits::new(ConnectionCeilings {
                node,
                tenant,
                instance_pending: pending,
            })
            .expect("fixture limits"),
        )
    }

    fn refused(level: ConnectionLevel, tenant: &str, instance: u64, limit: u64) -> DenyReason {
        DenyReason::ConnectionQuota {
            level,
            tenant: tenant.into(),
            instance: crate::InstanceId(instance),
            limit,
        }
    }

    #[test]
    fn one_tenant_at_its_limit_does_not_stop_another_until_the_node_is_full() {
        let policy = Arc::new(limited(Fixed(vec![]), 3, 2, 1));
        let peer = sa([93, 184, 216, 34], 443);
        let first = policy
            .check_socket(peer, &owner("a", 1), Reach::default())
            .expect("the tenant's first connection");
        let second = policy
            .check_socket(peer, &owner("a", 2), Reach::default())
            .expect("another instance of the tenant shares its limit");
        assert_eq!(
            policy
                .check_socket(peer, &owner("a", 3), Reach::default())
                .err(),
            Some(refused(ConnectionLevel::Tenant, "a", 3, 2)),
            "the tenant's third connection is over its limit"
        );
        assert_eq!(
            (policy.connections(), policy.tenant_connections("a")),
            (2, 2),
            "a tenant refusal takes no node permit"
        );
        let other = policy
            .check_socket(peer, &owner("b", 4), Reach::default())
            .expect("a full tenant must not stop another tenant connecting");
        assert_eq!(
            policy
                .check_socket(peer, &owner("c", 5), Reach::default())
                .err(),
            Some(refused(ConnectionLevel::Node, "c", 5, 3)),
            "the node's limit still applies across tenants"
        );
        assert_eq!(
            (policy.connections(), policy.tenant_connections("c")),
            (3, 0),
            "a node refusal takes no tenant permit"
        );
        drop(first);
        assert_eq!(
            (policy.connections(), policy.tenant_connections("a")),
            (2, 1),
            "releasing a socket returns its permit at every level"
        );
        let again = policy
            .check_socket(peer, &owner("a", 3), Reach::default())
            .expect("a released permit admits the tenant again");
        drop((second, other, again));
        assert_eq!(
            (
                policy.connections(),
                policy.tenant_connections("a"),
                policy.tenant_connections("b")
            ),
            (0, 0, 0),
            "every permit is returned"
        );
    }

    #[tokio::test]
    async fn checked_urls_take_and_return_permits_at_every_level() {
        let policy = Arc::new(limited(Fixed(vec![sa([93, 184, 216, 34], 80)]), 2, 1, 1));
        let held = policy
            .check_owned("http://a.example/", &owner("a", 1), Reach::default())
            .await
            .expect("the first connection");
        assert!(
            matches!(
                policy.check_owned("http://a.example/", &owner("a", 1), Reach::default()).await,
                Err(EgressError::Denied(reason)) if reason == refused(ConnectionLevel::Tenant, "a", 1, 1)
            ),
            "the tenant's second connection is over its limit"
        );
        let other = policy
            .check_owned("http://a.example/", &owner("b", 2), Reach::default())
            .await
            .expect("another tenant connects");
        assert!(
            matches!(
                policy.check_owned("http://a.example/", &owner("c", 3), Reach::default()).await,
                Err(EgressError::Denied(reason)) if reason == refused(ConnectionLevel::Node, "c", 3, 2)
            ),
            "the node is full"
        );
        drop((held, other));
        assert_eq!(
            policy.connections(),
            0,
            "dropped connections return their permits"
        );
        assert_eq!(policy.tenant_connections("a"), 0);
    }

    #[test]
    fn refusals_name_their_level_tenant_instance_and_limit() {
        assert_eq!(
            refused(ConnectionLevel::Tenant, "acme", 7, 2).to_string(),
            "tenant `acme` already holds its limit of 2 open outbound connections; instance i7 was refused another"
        );
        assert_eq!(
            refused(ConnectionLevel::Node, "acme", 7, 64).to_string(),
            "the node already holds its limit of 64 open outbound connections; instance i7 of tenant `acme` was refused another"
        );
    }

    #[tokio::test]
    async fn a_denied_target_does_not_burn_quota() {
        let p = Arc::new(limited(Fixed(vec![sa([10, 0, 0, 1], 80)]), 1, 1, 1));
        assert!(
            p.check_owned("http://internal/", &owner("a", 1), Reach::default())
                .await
                .is_err(),
            "the target is private"
        );
        assert_eq!(p.connections(), 0, "a refused target holds no slot");
    }

    #[tokio::test]
    async fn cancelling_a_checked_connection_releases_its_quota() {
        let policy = Arc::new(limited(Fixed(vec![sa([93, 184, 216, 34], 80)]), 1, 1, 1));
        let connection = policy
            .check_owned("http://example.test/", &owner("a", 1), Reach::default())
            .await
            .expect("the public destination must acquire connection quota");
        let (started, running) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _connection = connection;
            started
                .send(())
                .expect("the test must observe connection ownership");
            std::future::pending::<()>().await;
        });
        running.await.expect("the connection task must start");
        assert_eq!(
            (policy.connections(), policy.tenant_connections("a")),
            (1, 1),
            "the live connection must hold its quota"
        );
        task.abort();
        let error = task.await.expect_err("aborted connection work must end");
        assert!(
            error.is_cancelled(),
            "connection cancellation must not panic"
        );
        assert_eq!(
            (policy.connections(), policy.tenant_connections("a")),
            (0, 0),
            "cancellation must release the connection quota at every level"
        );
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
    ) -> Result<tokio::net::TcpStream, ConnectionError> {
        let attempt = policy.begin_connect(owner).await?;
        attempt.connect(destination).await
    }

    #[tokio::test]
    async fn an_attempt_beyond_the_pending_limit_waits_for_one_to_finish() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback peer");
        let peer = listener.local_addr().expect("peer address");
        let (arrivals, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let policy = Arc::new(
            limited(Fixed(vec![]), 16, 16, 6)
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

    #[tokio::test]
    async fn urls_parse_the_way_the_policy_assumes() {
        assert_eq!(
            parse_url("https://example.com/a?b=c").expect("parses"),
            ParsedUrl {
                scheme: "https".into(),
                host: "example.com".into(),
                port: 443,
                path_and_query: "/a?b=c".into()
            },
            "scheme, host, default port and path are split"
        );
        assert_eq!(
            parse_url("http://example.com:8080").expect("parses").port,
            8080,
            "an explicit port is kept"
        );
        assert_eq!(
            parse_url("http://[::1]:9/").expect("parses").host,
            "::1",
            "a v6 literal loses its brackets"
        );
        assert_eq!(
            parse_url("http://EXAMPLE.com/").expect("parses").host,
            "example.com",
            "hosts are lower-cased"
        );
    }
}
