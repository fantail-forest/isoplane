//! Exact Host routing to one installed deployment per application.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_abi::host::RouteTarget;
use parking_lot::RwLock;

/// Resolves a request using host-owned application identity.
pub trait RouteResolver: Send + Sync + 'static {
    /// Returns the application selected for this request, or no route.
    fn resolve(&self, host: &str, path: &str) -> Option<RouteTarget>;
    /// Changes whenever the route selection changes.
    fn generation(&self) -> u64;
}

/// Invalid or ambiguous operator routing configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouteError {
    /// A configured host is not a hostname or bracketed IP address.
    #[error("invalid host `{0}`; supply a hostname without a scheme, path or port")]
    InvalidHost(String),
    /// Two applications cannot own the same expected Host.
    #[error("host `{host}` belongs to both `{first}` and `{second}`")]
    DuplicateHost {
        host: String,
        first: String,
        second: String,
    },
    /// Routing requires explicit application and owner attribution.
    #[error("a route requires nonempty application, owner and deployment identities and at least one host")]
    Identity,
}

/// Parses request authority without confusing an IPv6 colon for a port separator.
fn hostname(value: &str) -> Option<String> {
    if value.is_empty()
        || value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, b'@' | b'/' | b'?' | b'#'))
    {
        return None;
    }
    let authority: hyper::http::uri::Authority = value.parse().ok()?;
    let suffix = value.strip_prefix(authority.host())?;
    if !suffix.is_empty()
        && suffix
            .strip_prefix(':')
            .filter(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|port| port.parse::<u16>().ok())
            .is_none()
    {
        return None;
    }
    Some(authority.host().to_ascii_lowercase())
}

/// Validates a configured host and returns its normalized matching key.
pub fn configured_host(value: &str) -> Result<String, RouteError> {
    let normalized = hostname(value).ok_or_else(|| RouteError::InvalidHost(value.to_owned()))?;
    if normalized != value.to_ascii_lowercase() {
        return Err(RouteError::InvalidHost(value.to_owned()));
    }
    Ok(normalized)
}

#[derive(Clone, Default)]
struct Table {
    hosts: HashMap<String, RouteTarget>,
}

/// An immutable routing snapshot replaced atomically when an application is installed.
#[derive(Default)]
pub struct LocalRouteTable {
    table: RwLock<Arc<Table>>,
    generation: AtomicU64,
}

impl LocalRouteTable {
    /// Creates an empty table with no fallback application.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs one complete application route selection, replacing its previous selection.
    pub fn insert(&self, target: RouteTarget, hosts: &[String]) -> Result<(), RouteError> {
        if target.app.trim().is_empty()
            || target.tenant.trim().is_empty()
            || target.version.trim().is_empty()
            || hosts.is_empty()
        {
            return Err(RouteError::Identity);
        }
        let hosts: Vec<_> = hosts
            .iter()
            .map(|host| configured_host(host))
            .collect::<Result<_, _>>()?;
        let mut guard = self.table.write();
        let mut table = (**guard).clone();
        for host in &hosts {
            if let Some(other) = table
                .hosts
                .get(host)
                .filter(|other| other.app != target.app)
            {
                return Err(RouteError::DuplicateHost {
                    host: host.clone(),
                    first: other.app.clone(),
                    second: target.app.clone(),
                });
            }
        }
        table.hosts.retain(|_, old| old.app != target.app);
        for host in hosts {
            table.hosts.insert(host, target.clone());
        }
        *guard = Arc::new(table);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Current application identities, sorted for operator diagnostics.
    pub fn apps(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .table
            .read()
            .hosts
            .values()
            .map(|target| target.app.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

impl RouteResolver for LocalRouteTable {
    fn resolve(&self, host: &str, _path: &str) -> Option<RouteTarget> {
        self.table.read().hosts.get(&hostname(host)?).cloned()
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

impl<T: RouteResolver + ?Sized> RouteResolver for Arc<T> {
    fn resolve(&self, host: &str, path: &str) -> Option<RouteTarget> {
        (**self).resolve(host, path)
    }
    fn generation(&self) -> u64 {
        (**self).generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_abi::host::CodeTier;

    fn target(app: &str, version: &str) -> RouteTarget {
        RouteTarget {
            app: app.into(),
            tenant: "owner".into(),
            version: version.into(),
            tier: CodeTier::Fast,
        }
    }

    #[test]
    fn exact_hosts_route_to_independent_apps_and_accept_optional_ports() {
        let table = LocalRouteTable::new();
        table
            .insert(target("one", "a"), &["ONE.example".into(), "[::1]".into()])
            .unwrap();
        table
            .insert(target("two", "b"), &["two.example".into()])
            .unwrap();
        assert_eq!(
            table.resolve("one.example:8080", "/any").unwrap().app,
            "one"
        );
        assert_eq!(table.resolve("[::1]:8080", "/").unwrap().app, "one");
        assert_eq!(table.resolve("two.example", "*").unwrap().app, "two");
        for host in [
            "",
            "other.example",
            "one.example:invalid",
            "user@one.example",
            "https://one.example",
        ] {
            assert!(
                table.resolve(host, "/").is_none(),
                "invalid or unknown host {host} must not route"
            );
        }
    }

    #[test]
    fn duplicate_host_refusal_preserves_the_complete_previous_table() {
        let table = LocalRouteTable::new();
        table
            .insert(target("one", "a"), &["one.example".into()])
            .unwrap();
        assert!(matches!(
            table.insert(target("two", "b"), &["ONE.example".into()]),
            Err(RouteError::DuplicateHost { .. })
        ));
        assert_eq!(table.resolve("one.example", "/").unwrap().app, "one");
        table
            .insert(target("one", "c"), &["new.example".into()])
            .unwrap();
        assert!(table.resolve("one.example", "/").is_none());
        assert_eq!(table.resolve("new.example", "/").unwrap().version, "c");
    }
}
