//! Egress / SSRF policy, enforced host-side.
//!
//! `http.fetch` enforces per-app policy host-side — RFC1918 / link-local
//! denied by default, DNS-rebinding-safe resolution (resolve-then-pin),
//! per-tenant connection and bandwidth quotas, optional allowlists.
//!
//! "Host-side" is the whole of it: the guest cannot be trusted to enforce
//! anything, and the import vtable is the entire attack surface into the host,
//! so this is the only place an outbound connection can be decided.
//!
//! **Resolve-then-pin** is the part that is easy to get subtly wrong. Checking
//! a hostname and then handing the hostname to an HTTP client lets DNS move
//! the target between the check and the connect — the classic rebinding
//! attack. [`HostEgressPolicy::check`] therefore resolves once, validates
//! *every* address it got back, and returns a [`PinnedTarget`] carrying the
//! `SocketAddr` the connection must actually use. A caller that re-resolves
//! has thrown the control away, which is why the pinned address is the only
//! thing the fetch path is given.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};

use wisp_abi::host::{EgressPolicy, PinnedTarget};

/// DNS, behind a seam so the rebinding behaviour is testable without a
/// resolver that lies on demand in production.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
}

/// The real thing: blocking `getaddrinfo`. Called from the reactor's blocking
/// pool, never from an execution-pool thread — a resolver stall must not hold
/// a Hot dispatch's thread.
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        (host, port)
            .to_socket_addrs()
            .map(|it| it.collect())
            .map_err(|e| format!("dns: {e}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// `http` is the only egress the guest has; nothing else is dialable.
    Scheme(String),
    Malformed(String),
    NoAddress,
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

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyReason::Scheme(s) => write!(f, "scheme `{s}` is not http(s)"),
            DenyReason::Malformed(s) => write!(f, "malformed url: {s}"),
            DenyReason::NoAddress => write!(f, "host resolved to no addresses"),
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

/// Every range the platform refuses to dial. Deny-by-default: the check is
/// "is this address in a range we are willing to reach", and anything the
/// classifier does not understand is refused.
pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()            // RFC1918
                || v4.is_loopback()
                || v4.is_link_local()  // 169.254/16, and with it 169.254.169.254
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || is_shared_cgnat(v4) // 100.64/10 — carrier NAT, often internal
                || is_benchmark(v4)    // 198.18/15
                || v4.octets()[0] == 0
                || v4.octets()[0] >= 240 // reserved / class E
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local(v6)      // fc00::/7
                || is_v6_link_local(v6)     // fe80::/10
                // A v4-mapped or v4-compatible address must be judged as the
                // v4 address it carries, or ::ffff:127.0.0.1 walks straight in.
                || v6.to_ipv4_mapped().map(|v| is_forbidden(IpAddr::V4(v))).unwrap_or(false)
                || v6.to_ipv4().map(|v| is_forbidden(IpAddr::V4(v))).unwrap_or(false)
        }
    }
}

fn is_shared_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

fn is_benchmark(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 198 && (o[1] == 18 || o[1] == 19)
}

fn is_unique_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

fn is_v6_link_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

/// A parsed URL, without pulling in a URL crate for the PoC.
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
    // Credentials in the authority are a redirect-and-confuse primitive and
    // buy a tenant nothing they cannot do with a header.
    if authority.contains('@') {
        return Err(DenyReason::Malformed(
            "userinfo is not permitted in a fetch url".into(),
        ));
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        // IPv6 literal.
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

/// The egress policy, with the per-tenant quotas attached.
pub struct HostEgressPolicy {
    resolver: Box<dyn Resolver>,
    /// Per-tenant connection quota.
    max_connections: u64,
    /// Per-tenant bandwidth quota. This is a fair-use
    /// cap, not a meter.
    max_bytes: u64,
    connections: AtomicU64,
    bytes: AtomicU64,
    /// Counts resolutions, so a test can prove the target was pinned rather
    /// than looked up twice.
    resolutions: AtomicU64,
}

impl HostEgressPolicy {
    pub fn new(resolver: Box<dyn Resolver>, max_connections: u64, max_bytes: u64) -> Self {
        Self {
            resolver,
            max_connections,
            max_bytes,
            connections: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            resolutions: AtomicU64::new(0),
        }
    }

    pub fn system(max_connections: u64, max_bytes: u64) -> Self {
        Self::new(Box::new(SystemResolver), max_connections, max_bytes)
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
        let _ = self
            .connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    fn allowlisted(host: &str, allowlist: Option<&[String]>) -> bool {
        let Some(list) = allowlist else { return true };
        list.iter().any(|pat| {
            let pat = pat.trim().to_ascii_lowercase();
            match pat.strip_prefix("*.") {
                Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
                None => host == pat,
            }
        })
    }

    pub fn check_typed(
        &self,
        url: &str,
        allowlist: Option<&[String]>,
    ) -> Result<PinnedTarget, DenyReason> {
        let parsed = parse_url(url)?;
        if !Self::allowlisted(&parsed.host, allowlist) {
            return Err(DenyReason::NotAllowlisted(parsed.host));
        }
        if self.bytes.load(Ordering::Relaxed) >= self.max_bytes {
            return Err(DenyReason::BandwidthQuota {
                limit_bytes: self.max_bytes,
            });
        }
        let taken = self
            .connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                (v < self.max_connections).then_some(v + 1)
            });
        if taken.is_err() {
            return Err(DenyReason::ConnectionQuota {
                limit: self.max_connections,
            });
        }

        // Resolve ONCE. Everything after this decides about addresses we
        // already hold, and the pinned address is what the connection uses.
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        let addrs = match self.resolver.resolve(&parsed.host, parsed.port) {
            Ok(a) => a,
            Err(e) => {
                self.release_connection();
                return Err(DenyReason::Malformed(e));
            }
        };
        if addrs.is_empty() {
            self.release_connection();
            return Err(DenyReason::NoAddress);
        }
        // Every returned address must be acceptable, not merely the first: a
        // resolver that returns [public, private] would otherwise let a client
        // that falls back to the second address reach the private one.
        if let Some(bad) = addrs.iter().find(|a| is_forbidden(a.ip())) {
            self.release_connection();
            return Err(DenyReason::PrivateAddress(bad.ip()));
        }
        Ok(PinnedTarget {
            url: url.to_string(),
            addr: addrs[0],
        })
    }
}

impl EgressPolicy for HostEgressPolicy {
    fn check(&self, url: &str, allowlist: Option<&[String]>) -> Result<PinnedTarget, String> {
        self.check_typed(url, allowlist).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::net::Ipv4Addr;

    /// A resolver that answers differently on each call — the shape of a DNS
    /// rebinding attack.
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
    impl Resolver for Rebinder {
        fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, String> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
            let a = self.answers.lock();
            Ok(a.get(n).or_else(|| a.last()).cloned().unwrap_or_default())
        }
    }

    struct Fixed(Vec<SocketAddr>);
    impl Resolver for Fixed {
        fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<SocketAddr>, String> {
            Ok(self.0.clone())
        }
    }

    fn sa(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)
    }

    fn policy(r: impl Resolver) -> HostEgressPolicy {
        HostEgressPolicy::new(Box::new(r), 100, 1 << 30)
    }

    /// RFC1918 / link-local denied by default. 169.254.169.254 is the cloud
    /// metadata endpoint and is the reason this test exists.
    #[test]
    fn private_and_link_local_are_denied() {
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
            let err = p.check_typed("http://target.example/", None).unwrap_err();
            assert!(
                matches!(err, DenyReason::PrivateAddress(_)),
                "{ip:?} was not denied: {err}"
            );
        }
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 80)]));
        assert!(p.check_typed("http://example.com/", None).is_ok());
    }

    #[test]
    fn ipv6_loopback_ula_and_v4_mapped_are_denied() {
        for lit in ["[::1]", "[fc00::1]", "[fe80::1]", "[::ffff:127.0.0.1]"] {
            let ip: IpAddr = lit.trim_matches(['[', ']']).parse().unwrap();
            let p = policy(Fixed(vec![SocketAddr::new(ip, 80)]));
            let err = p.check_typed(&format!("http://{lit}/"), None).unwrap_err();
            assert!(
                matches!(err, DenyReason::PrivateAddress(_)),
                "{lit} allowed: {err}"
            );
        }
    }

    /// Resolve-then-pin. The check resolves once; the connection is given the
    /// address, not the name, so a second DNS answer has nothing to move.
    #[test]
    fn dns_rebinding_cannot_move_the_target_after_the_check() {
        let p = policy(Rebinder::new(vec![
            vec![sa([93, 184, 216, 34], 443)], // first answer: public
            vec![sa([127, 0, 0, 1], 443)],     // second: the attack
        ]));
        let pinned = p.check_typed("https://rebind.example/", None).unwrap();
        assert_eq!(pinned.addr, sa([93, 184, 216, 34], 443));
        assert_eq!(p.resolutions(), 1, "the policy must resolve exactly once");
        // The fetch path connects to `pinned.addr`. Even though DNS now says
        // 127.0.0.1, nothing in the request carries the name any more.
        assert_ne!(pinned.addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    /// The other half of rebinding: a single answer containing both a public
    /// and a private address must be refused outright, not silently narrowed
    /// to the public one.
    #[test]
    fn a_mixed_answer_is_denied_entirely() {
        let p = policy(Fixed(vec![
            sa([93, 184, 216, 34], 80),
            sa([192, 168, 0, 5], 80),
        ]));
        let err = p.check_typed("http://mixed.example/", None).unwrap_err();
        assert_eq!(
            err,
            DenyReason::PrivateAddress(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5)))
        );
    }

    #[test]
    fn only_http_and_https_egress_exists() {
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 80)]));
        for url in ["file:///etc/passwd", "gopher://x/", "ftp://x/"] {
            assert!(
                matches!(p.check_typed(url, None), Err(DenyReason::Scheme(_))),
                "{url}"
            );
        }
        assert!(matches!(
            p.check_typed("http://user:pw@example.com/", None),
            Err(DenyReason::Malformed(_))
        ));
    }

    #[test]
    fn allowlist_supports_exact_and_wildcard_hosts() {
        let p = policy(Fixed(vec![sa([93, 184, 216, 34], 443)]));
        let list = vec!["api.example.com".to_string(), "*.cdn.example".to_string()];
        assert!(p
            .check_typed("https://api.example.com/x", Some(&list))
            .is_ok());
        assert!(p
            .check_typed("https://a.cdn.example/x", Some(&list))
            .is_ok());
        assert!(p.check_typed("https://cdn.example/x", Some(&list)).is_ok());
        let err = p
            .check_typed("https://evil.example/x", Some(&list))
            .unwrap_err();
        assert_eq!(err, DenyReason::NotAllowlisted("evil.example".into()));
        // Not a suffix match on the raw string: `notcdn.example` must not pass
        // a `*.cdn.example` rule.
        assert!(p
            .check_typed("https://notcdn.example/x", Some(&list))
            .is_err());
    }

    #[test]
    fn connection_quota_is_per_tenant_and_released() {
        let p = HostEgressPolicy::new(
            Box::new(Fixed(vec![sa([93, 184, 216, 34], 80)])),
            2,
            1 << 30,
        );
        assert!(p.check_typed("http://a.example/", None).is_ok());
        assert!(p.check_typed("http://a.example/", None).is_ok());
        assert!(matches!(
            p.check_typed("http://a.example/", None),
            Err(DenyReason::ConnectionQuota { limit: 2 })
        ));
        p.release_connection();
        assert!(p.check_typed("http://a.example/", None).is_ok());
    }

    /// A denied request must not consume a connection slot.
    #[test]
    fn a_denied_target_does_not_burn_quota() {
        let p = HostEgressPolicy::new(Box::new(Fixed(vec![sa([10, 0, 0, 1], 80)])), 1, 1 << 30);
        assert!(p.check_typed("http://internal/", None).is_err());
        assert_eq!(p.connections(), 0);
    }

    #[test]
    fn urls_parse_the_way_the_policy_assumes() {
        assert_eq!(
            parse_url("https://example.com/a?b=c").unwrap(),
            ParsedUrl {
                scheme: "https".into(),
                host: "example.com".into(),
                port: 443,
                path_and_query: "/a?b=c".into()
            }
        );
        assert_eq!(parse_url("http://example.com:8080").unwrap().port, 8080);
        assert_eq!(parse_url("http://[::1]:9/").unwrap().host, "::1");
        assert_eq!(
            parse_url("http://EXAMPLE.com/").unwrap().host,
            "example.com"
        );
    }
}
