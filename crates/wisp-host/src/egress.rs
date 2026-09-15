//! Egress policy for `http_fetch`, enforced host-side: private and link-local
//! ranges denied, optional per-app allowlists, per-tenant connection and
//! bandwidth quotas, and resolve-then-pin. The check resolves once, validates
//! every address it got back, and returns a [`PinnedTarget`] carrying the
//! `SocketAddr` the connection uses, so DNS cannot move the target between
//! the check and the connect.

mod additional_destinations;
mod connection;
mod dns;
pub use additional_destinations::AdditionalDestinations;
pub use connection::{ConnectionError, ConnectionFactory, ConnectionOwner, LocalConnections};
pub use dns::{DnsError, Resolver, SystemResolver};

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wisp_abi::host::PinnedTarget;

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
    /// Per-tenant connection quota.
    ConnectionQuota {
        limit: u64,
    },
    /// Per-tenant bandwidth quota.
    BandwidthQuota {
        limit_bytes: u64,
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
            DenyReason::ConnectionQuota { limit } => {
                write!(f, "per-tenant connection quota of {limit} exhausted")
            }
            DenyReason::BandwidthQuota { limit_bytes } => {
                write!(
                    f,
                    "per-tenant bandwidth quota of {limit_bytes} bytes exhausted"
                )
            }
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
    max_connections: u64,
    /// Egress has a fair-use cap.
    max_bytes: u64,
    connections: AtomicU64,
    bytes: AtomicU64,
    resolutions: AtomicU64,
}

/// Holds connection quota until the corresponding host socket is released.
pub(crate) struct ConnectionPermit(Arc<HostEgressPolicy>);

pub(crate) struct ByteReservation {
    policy: Arc<HostEgressPolicy>,
    available: u64,
}

impl ConnectionPermit {
    pub(crate) fn reserve_bytes(
        &self,
        requested: wisp_abi::native_io::ByteCount,
    ) -> Result<ByteReservation, DenyReason> {
        let mut available = 0;
        self.0
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                available = requested.0.min(self.0.max_bytes.saturating_sub(used));
                (available != 0 || requested.0 == 0).then_some(used.saturating_add(available))
            })
            .map_err(|_| DenyReason::BandwidthQuota {
                limit_bytes: self.0.max_bytes,
            })?;
        Ok(ByteReservation {
            policy: self.0.clone(),
            available,
        })
    }
}

impl ByteReservation {
    pub(crate) fn available(&self) -> wisp_abi::native_io::ByteCount {
        wisp_abi::native_io::ByteCount(self.available)
    }

    pub(crate) fn commit(mut self, transferred: wisp_abi::native_io::ByteCount) {
        self.available = self.available.saturating_sub(transferred.0);
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.policy
            .bytes
            .fetch_sub(self.available, Ordering::Relaxed);
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.release_connection();
    }
}

/// A checked destination whose connection quota is released on cancellation too.
pub(crate) struct CheckedConnection {
    target: PinnedTarget,
    policy: Arc<HostEgressPolicy>,
}

impl CheckedConnection {
    /// The resolved destination the transport must use.
    pub(crate) fn target(&self) -> PinnedTarget {
        self.target.clone()
    }
}

impl Drop for CheckedConnection {
    fn drop(&mut self) {
        self.policy.release_connection();
    }
}

impl HostEgressPolicy {
    /// Resolve an allowed name and refuse the complete answer if any address is forbidden.
    pub(crate) async fn resolve_name(
        &self,
        name: &str,
        allowlist: Option<&[String]>,
    ) -> Result<Vec<IpAddr>, EgressError> {
        if !Self::allowlisted(name, allowlist) {
            return Err(DenyReason::NotAllowlisted(name.to_owned()).into());
        }
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addresses = self.resolver.resolve(name, 0).await?;
        if addresses.is_empty() {
            return Err(DnsError::NoAddress.into());
        }
        for address in &addresses {
            self.destinations.authorize_resolution(address.ip())?;
        }
        Ok(addresses.into_iter().map(|address| address.ip()).collect())
    }

    /// Select where both native TCP and HTTP sockets are opened.
    pub fn with_connections(mut self, connections: Arc<dyn ConnectionFactory>) -> Self {
        self.connections_factory = connections;
        self
    }

    pub(crate) async fn connect(
        &self,
        owner: &ConnectionOwner,
        destination: SocketAddr,
    ) -> Result<tokio::net::TcpStream, ConnectionError> {
        self.destinations.authorize(destination)?;
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
        self.destinations.authorize(peer)?;
        Ok(stream)
    }

    /// Select the address policy used by every checked destination.
    pub fn with_destinations(mut self, destinations: Arc<dyn DestinationPolicy>) -> Self {
        self.destinations = destinations;
        self
    }

    /// Reserve quota for a checked numeric socket destination, without performing DNS.
    pub(crate) fn check_socket(
        self: &Arc<Self>,
        address: SocketAddr,
        allowlist: Option<&[String]>,
    ) -> Result<ConnectionPermit, DenyReason> {
        self.destinations.authorize(address)?;
        let host = address.ip().to_string();
        if !Self::allowlisted(&host, allowlist) {
            return Err(DenyReason::NotAllowlisted(host));
        }
        if self.bytes.load(Ordering::Relaxed) >= self.max_bytes {
            return Err(DenyReason::BandwidthQuota {
                limit_bytes: self.max_bytes,
            });
        }
        self.connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value < self.max_connections).then_some(value + 1)
            })
            .map_err(|_| DenyReason::ConnectionQuota {
                limit: self.max_connections,
            })?;
        Ok(ConnectionPermit(self.clone()))
    }

    /// Acquires a checked destination and releases its quota when ownership ends.
    pub(crate) async fn check_owned(
        self: &Arc<Self>,
        url: &str,
        allowlist: Option<&[String]>,
    ) -> Result<CheckedConnection, EgressError> {
        self.check_typed(url, allowlist)
            .await
            .map(|target| CheckedConnection {
                target,
                policy: self.clone(),
            })
    }

    pub fn new(resolver: Box<dyn Resolver>, max_connections: u64, max_bytes: u64) -> Self {
        Self {
            connections_factory: Arc::new(LocalConnections),
            resolver,
            destinations: Arc::new(PublicDestinations),
            max_connections,
            max_bytes,
            connections: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            resolutions: AtomicU64::new(0),
        }
    }

    pub fn system(max_connections: u64, max_bytes: u64) -> Result<Self, DnsError> {
        Ok(Self::new(
            Box::new(SystemResolver::new()?),
            max_connections,
            max_bytes,
        ))
    }

    pub fn resolutions(&self) -> u64 {
        self.resolutions.load(Ordering::Relaxed)
    }
    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }
    pub fn record_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }
    pub fn release_connection(&self) {
        self.connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            })
            .expect("the update closure in this function always returns Some");
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

    pub async fn check_typed(
        &self,
        url: &str,
        allowlist: Option<&[String]>,
    ) -> Result<PinnedTarget, EgressError> {
        let parsed = parse_url(url)?;
        if !Self::allowlisted(&parsed.host, allowlist) {
            return Err(DenyReason::NotAllowlisted(parsed.host).into());
        }
        if self.bytes.load(Ordering::Relaxed) >= self.max_bytes {
            return Err(DenyReason::BandwidthQuota {
                limit_bytes: self.max_bytes,
            }
            .into());
        }
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addrs = self.resolver.resolve(&parsed.host, parsed.port).await?;
        if addrs.is_empty() {
            return Err(DnsError::NoAddress.into());
        }
        // Every address, not the first: a client that falls back to a second
        // address would otherwise reach a private one.
        for address in &addrs {
            self.destinations.authorize(*address)?;
        }
        let taken = self
            .connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                (v < self.max_connections).then_some(v + 1)
            });
        if taken.is_err() {
            return Err(DenyReason::ConnectionQuota {
                limit: self.max_connections,
            }
            .into());
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

    fn policy(r: impl Resolver) -> HostEgressPolicy {
        HostEgressPolicy::new(Box::new(r), 100, 1 << 30)
    }

    struct CountingFailure(Arc<AtomicU64>);
    #[async_trait::async_trait]
    impl Resolver for CountingFailure {
        async fn resolve(&self, host: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(DnsError::Lookup {
                host: host.to_owned(),
                kind: wisp_abi::native_io::DnsFailure::Temporary,
                message: "DNS server returned SERVFAIL".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_disallowed_dns_name_is_refused_before_the_resolver_can_run() {
        let calls = Arc::new(AtomicU64::new(0));
        let policy = policy(CountingFailure(calls.clone()));
        let result = policy
            .resolve_name("forbidden.test", Some(&["allowed.test".into()]))
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
            .resolve_name("allowed.test", Some(&["allowed.test".into()]))
            .await;
        assert!(
            matches!(result, Err(EgressError::Dns(error)) if error.kind() == wisp_abi::native_io::DnsFailure::Temporary),
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
                .check_typed("http://target.example/", None)
                .await
                .expect_err("a private address must be refused");
            assert!(
                matches!(err, EgressError::Denied(DenyReason::PrivateAddress(_))),
                "{ip:?} was not denied: {err}"
            );
        }
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 80)]));
        assert!(
            p.check_typed("http://example.com/", None).await.is_ok(),
            "a public address is dialable"
        );
    }

    #[tokio::test]
    async fn ipv6_loopback_ula_and_v4_mapped_are_denied() {
        for lit in ["[::1]", "[fc00::1]", "[fe80::1]", "[::ffff:127.0.0.1]"] {
            let ip: IpAddr = lit.trim_matches(['[', ']']).parse().expect("literal");
            let p = policy(Fixed(vec![SocketAddr::new(ip, 80)]));
            let err = p
                .check_typed(&format!("http://{lit}/"), None)
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
            .check_typed("https://rebind.example/", None)
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
            .check_typed("http://mixed.example/", None)
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
                    p.check_typed(url, None).await,
                    Err(EgressError::Denied(DenyReason::Scheme(_)))
                ),
                "{url} is not dialable"
            );
        }
        assert!(
            matches!(
                p.check_typed("http://user:pw@example.com/", None).await,
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
            let permit = policy.check_socket(address, Some(&[allowed.to_owned()]));
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
            assert!(matches!(policy.check_socket(address, Some(&["93.184.216.34".to_owned()])), Err(DenyReason::NotAllowlisted(_))),
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
                policy.check_socket(destination, Some(&["127.0.0.1".to_owned()])),
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
            p.check_typed("https://api.example.com/x", Some(&list))
                .await
                .is_ok(),
            "an exact entry matches"
        );
        assert!(
            p.check_typed("https://a.cdn.example/x", Some(&list))
                .await
                .is_ok(),
            "a wildcard matches a subdomain"
        );
        assert!(
            p.check_typed("https://cdn.example/x", Some(&list))
                .await
                .is_ok(),
            "a wildcard matches its bare suffix"
        );
        let err = p
            .check_typed("https://evil.example/x", Some(&list))
            .await
            .expect_err("a host off the list is refused");
        assert!(
            matches!(err, EgressError::Denied(DenyReason::NotAllowlisted(ref host)) if host == "evil.example"),
            "the refusal names the host"
        );
        assert!(
            p.check_typed("https://notcdn.example/x", Some(&list))
                .await
                .is_err(),
            "`*.cdn.example` is not a raw suffix match"
        );
    }

    #[tokio::test]
    async fn connection_quota_is_per_tenant_and_released() {
        let p = HostEgressPolicy::new(
            Box::new(Fixed(vec![sa([93, 184, 216, 34], 80)])),
            2,
            1 << 30,
        );
        assert!(
            p.check_typed("http://a.example/", None).await.is_ok(),
            "the first connection"
        );
        assert!(
            p.check_typed("http://a.example/", None).await.is_ok(),
            "the second connection"
        );
        assert!(
            matches!(
                p.check_typed("http://a.example/", None).await,
                Err(EgressError::Denied(DenyReason::ConnectionQuota {
                    limit: 2
                }))
            ),
            "the third is over the quota"
        );
        p.release_connection();
        assert!(
            p.check_typed("http://a.example/", None).await.is_ok(),
            "a released connection frees the slot"
        );
    }

    #[tokio::test]
    async fn a_denied_target_does_not_burn_quota() {
        let p = HostEgressPolicy::new(Box::new(Fixed(vec![sa([10, 0, 0, 1], 80)])), 1, 1 << 30);
        assert!(
            p.check_typed("http://internal/", None).await.is_err(),
            "the target is private"
        );
        assert_eq!(p.connections(), 0, "a refused target holds no slot");
    }

    #[tokio::test]
    async fn cancelling_a_checked_connection_releases_its_quota() {
        let policy = Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec![sa([93, 184, 216, 34], 80)])),
            1,
            1 << 30,
        ));
        let connection = policy
            .check_owned("http://example.test/", None)
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
            policy.connections(),
            1,
            "the live connection must hold its quota"
        );
        task.abort();
        let error = task.await.expect_err("aborted connection work must end");
        assert!(
            error.is_cancelled(),
            "connection cancellation must not panic"
        );
        assert_eq!(
            policy.connections(),
            0,
            "cancellation must release the connection quota"
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
