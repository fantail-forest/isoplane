//! The cell's socket seam over the proxy's hub, and the output gate's far
//! end: a released socket effect reaches a proxy-owned socket.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::host::DiId;
use wisp_durable::context::{EffectSink, EmittedEffect, ExternalEffect};
use wisp_host::durable_seam::SocketOps as _;

/// The cell's socket seam over the proxy's hub: the guest's `u32` handle
/// against the proxy's `u64` socket id. The map survives hibernation, since
/// the socket itself lives in the proxy and never hears about the instance.
pub struct ProxySocketOps {
    hub: Arc<wisp_proxy::SocketHub>,
    map: RwLock<HashMap<u32, Bound>>,
}

#[derive(Debug, Clone)]
struct Bound {
    proxy_socket_id: u64,
    tag: String,
    tenant: String,
}

impl ProxySocketOps {
    pub fn new(hub: Arc<wisp_proxy::SocketHub>) -> Arc<Self> {
        Arc::new(ProxySocketOps {
            hub,
            map: RwLock::new(HashMap::new()),
        })
    }

    pub fn bind(&self, tenant: &str, handle: u32, proxy_socket_id: u64, tag: &str) {
        self.map.write().insert(
            handle,
            Bound {
                proxy_socket_id,
                tag: tag.to_string(),
                tenant: tenant.to_string(),
            },
        );
    }

    pub fn proxy_socket_id(&self, handle: u32) -> Option<u64> {
        self.map.read().get(&handle).map(|b| b.proxy_socket_id)
    }

    fn entry(&self, handle: u32) -> Result<Bound, wisp_abi::FutureError> {
        self.map
            .read()
            .get(&handle)
            .cloned()
            .ok_or(wisp_abi::FutureError::NotFound)
    }
}

#[async_trait::async_trait]
impl wisp_host::durable_seam::SocketOps for ProxySocketOps {
    /// Idempotent: a wake or a re-termination re-binds the handle the guest
    /// already holds.
    fn bind(&self, tenant: &str, handle: u32, proxy_socket_id: u64, tag: &str) {
        ProxySocketOps::bind(self, tenant, handle, proxy_socket_id, tag)
    }
    fn accept(&self, _tenant: &str, handle: u32, tag: &str) -> Result<(), wisp_abi::FutureError> {
        match self.map.write().get_mut(&handle) {
            Some(b) => {
                b.tag = tag.to_string();
                Ok(())
            }
            None => Err(wisp_abi::FutureError::NotFound),
        }
    }

    async fn send(
        &self,
        _tenant: &str,
        handle: u32,
        frame: wisp_abi::SocketFrame,
    ) -> Result<(), wisp_abi::FutureError> {
        use wisp_proxy::SocketEgress as _;
        let b = self.entry(handle)?;
        self.hub
            .send(b.proxy_socket_id, frame)
            .map_err(|e| match e {
                wisp_proxy::EgressError::NoSuchSocket(_) => wisp_abi::FutureError::NotFound,
                wisp_proxy::EgressError::Closed(_) => {
                    wisp_abi::FutureError::Io("socket closed".into())
                }
                wisp_proxy::EgressError::Congested(_) => {
                    wisp_abi::FutureError::Io("socket backpressure".into())
                }
            })
    }

    /// Every socket the tenant holds, minus `except_tag`. Scoped by tenant
    /// because that is what the seam carries.
    async fn broadcast(
        &self,
        tenant: &str,
        req: wisp_abi::calls::SocketBroadcast,
    ) -> Result<u32, wisp_abi::FutureError> {
        use wisp_proxy::SocketEgress as _;
        let targets: Vec<u64> = self
            .map
            .read()
            .values()
            .filter(|b| b.tenant == tenant)
            .filter(|b| req.except_tag.as_deref() != Some(b.tag.as_str()))
            .map(|b| b.proxy_socket_id)
            .collect();
        let mut sent = 0u32;
        for id in targets {
            if self.hub.send(id, req.frame.clone()).is_ok() {
                sent += 1;
            }
        }
        Ok(sent)
    }

    fn close(&self, _tenant: &str, handle: u32) {
        use wisp_proxy::SocketEgress as _;
        if let Some(b) = self.map.write().remove(&handle) {
            if let Err(e) = self
                .hub
                .close(b.proxy_socket_id, 1000, "closed by instance")
            {
                tracing::debug!(handle, error = %e, "socket already gone at close");
            }
        }
    }

    fn detach_for_hibernation(
        &self,
        _tenant: &str,
        handles: &[u32],
    ) -> Vec<wisp_abi::manifest::SocketEntry> {
        let map = self.map.read();
        handles
            .iter()
            .filter_map(|h| {
                map.get(h).map(|b| wisp_abi::manifest::SocketEntry {
                    handle: *h,
                    tag: b.tag.clone(),
                    proxy_socket_id: b.proxy_socket_id,
                })
            })
            .collect()
    }

    fn reattach_on_wake(
        &self,
        tenant: &str,
        entries: &[wisp_abi::manifest::SocketEntry],
    ) -> Vec<u32> {
        let mut map = self.map.write();
        entries
            .iter()
            .map(|e| {
                map.insert(
                    e.handle,
                    Bound {
                        proxy_socket_id: e.proxy_socket_id,
                        tag: e.tag.clone(),
                        tenant: tenant.to_string(),
                    },
                );
                e.handle
            })
            .collect()
    }
}

/// The far end of the output gate: a released effect reaches a proxy-owned
/// socket, and nothing else in the node writes to a DI's sockets.
pub(super) struct GatedSocketSink {
    pub(super) sockets: Arc<ProxySocketOps>,
    pub(super) tenants: Arc<RwLock<HashMap<String, String>>>,
}

impl GatedSocketSink {
    fn tenant_for(&self, id: &DiId) -> String {
        self.tenants
            .read()
            .get(&id.app)
            .cloned()
            .unwrap_or_else(|| id.app.clone())
    }
}

#[async_trait::async_trait]
impl EffectSink for GatedSocketSink {
    async fn deliver(&self, id: &DiId, effect: &EmittedEffect) -> anyhow::Result<Vec<u8>> {
        let tenant = self.tenant_for(id);
        match &effect.effect {
            ExternalEffect::SocketSend { handle, frame } => {
                self.sockets
                    .send(&tenant, *handle, frame.clone())
                    .await
                    .map_err(anyhow::Error::new)?;
                wisp_abi::encode(&1u32).map_err(anyhow::Error::new)
            }
            ExternalEffect::SocketBroadcast { frame, except_tag } => {
                let n = self
                    .sockets
                    .broadcast(
                        &tenant,
                        wisp_abi::calls::SocketBroadcast {
                            frame: frame.clone(),
                            except_tag: except_tag.clone(),
                        },
                    )
                    .await
                    .map_err(anyhow::Error::new)?;
                wisp_abi::encode(&n).map_err(anyhow::Error::new)
            }
            // A DI's `fetch` and `queue_send` go through the cell's own egress
            // and store bindings, not this gate.
            other => anyhow::bail!(
                "the node's effect sink only delivers socket effects; \
                 `{}` from a DI is not routed through the output gate in this build",
                other.label()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::host::RouteTarget;
    use wisp_proxy::{ProxyMetrics, SocketConfig, SocketHub};

    fn hub_and_ops() -> (Arc<SocketHub>, Arc<ProxySocketOps>) {
        let hub = Arc::new(SocketHub::new(
            Arc::new(ProxyMetrics::new()),
            SocketConfig::default(),
        ));
        let ops = ProxySocketOps::new(hub.clone());
        (hub, ops)
    }

    fn target() -> RouteTarget {
        wisp_proxy::fake::fake_target()
    }

    #[tokio::test]
    async fn a_guest_send_reaches_the_proxy_owned_socket() {
        let (hub, ops) = hub_and_ops();
        let di = DiId {
            app: "quotes".into(),
            class: "Room".into(),
            name: "lobby".into(),
        };
        let (sock, _i, mut o) = hub.register(di, "chat".into(), target());
        ops.bind("acme", 7, sock.id, "chat");

        ops.send("acme", 7, wisp_abi::SocketFrame::Text("hi".into()))
            .await
            .unwrap();
        assert!(o.recv().await.is_some(), "the frame reached the socket");

        assert_eq!(
            ops.send("acme", 99, wisp_abi::SocketFrame::Text("x".into()))
                .await,
            Err(wisp_abi::FutureError::NotFound),
            "an unbound handle is refused, not dropped"
        );
    }

    #[tokio::test]
    async fn hibernation_does_not_disturb_the_socket() {
        let (hub, ops) = hub_and_ops();
        let di = DiId {
            app: "quotes".into(),
            class: "Room".into(),
            name: "lobby".into(),
        };
        let (sock, _i, mut o) = hub.register(di.clone(), "chat".into(), target());
        ops.bind("acme", 3, sock.id, "chat");

        let entries = ops.detach_for_hibernation("acme", &[3]);
        assert_eq!(
            (
                entries.len(),
                entries[0].proxy_socket_id,
                entries[0].tag.as_str()
            ),
            (1, sock.id, "chat"),
            "the manifest entry carries both namespaces and the tag"
        );
        assert_eq!(hub.live_count(), 1, "the socket outlives the instance");
        assert!(!sock.is_closed(), "and stays open");

        let handles = ops.reattach_on_wake("acme", &entries);
        assert_eq!(handles, vec![3], "the same handle comes back");
        assert_eq!(
            ops.proxy_socket_id(3),
            Some(sock.id),
            "pointing at the same socket"
        );
        ops.send("acme", 3, wisp_abi::SocketFrame::Text("after".into()))
            .await
            .unwrap();
        assert!(o.recv().await.is_some(), "a send after the wake reaches it");
    }

    #[tokio::test]
    async fn broadcast_is_tenant_scoped_and_honours_except_tag() {
        let (hub, ops) = hub_and_ops();
        let di = DiId {
            app: "quotes".into(),
            class: "Room".into(),
            name: "lobby".into(),
        };
        // A dropped receiver closes its socket.
        let mut keep = Vec::new();
        for h in 1..=3u32 {
            let (s, i, o) = hub.register(di.clone(), "chat".into(), target());
            ops.bind("acme", h, s.id, if h == 3 { "sender" } else { "chat" });
            keep.push((i, o));
        }
        let other = DiId {
            app: "other".into(),
            class: "Room".into(),
            name: "x".into(),
        };
        let (o, oi, oo) = hub.register(other, "chat".into(), target());
        ops.bind("bigco", 9, o.id, "chat");
        keep.push((oi, oo));

        let n = ops
            .broadcast(
                "acme",
                wisp_abi::calls::SocketBroadcast {
                    frame: wisp_abi::SocketFrame::Text("all".into()),
                    except_tag: Some("sender".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            n, 2,
            "one tenant, minus the excluded tag, and no cross-tenant leakage"
        );
        drop(keep);
    }

    #[tokio::test]
    async fn closing_from_the_guest_releases_the_handle() {
        let (hub, ops) = hub_and_ops();
        let di = DiId {
            app: "quotes".into(),
            class: "Room".into(),
            name: "lobby".into(),
        };
        let (sock, _i, _o) = hub.register(di, "chat".into(), target());
        ops.bind("acme", 5, sock.id, "chat");
        ops.close("acme", 5);
        assert_eq!(ops.proxy_socket_id(5), None, "the handle is unbound");
        assert_eq!(
            ops.send("acme", 5, wisp_abi::SocketFrame::Text("x".into()))
                .await,
            Err(wisp_abi::FutureError::NotFound),
            "a closed handle is refused"
        );
    }
}
