//! DNS-derived authority survives exact IPv4-mapped representations only.

use isoplane_abi::native_io::{AddressFamily, Error, Request, ResourceHandle, Response};
use isoplane_engine::HostBindings;
use isoplane_harness::Reactor;
use isoplane_host::bindings::CellBindings;
use isoplane_host::egress::{ConnectionFactory, ConnectionOwner, DnsError};
use isoplane_host::{InstanceId, Resolver};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const PUBLIC: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

struct Dns(IpAddr);
#[async_trait::async_trait]
impl Resolver for Dns {
    async fn resolve(&self, name: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError> {
        assert_eq!(name, "allowed.test", "unapproved names must not reach DNS");
        Ok(vec![SocketAddr::new(self.0, port)])
    }
}

struct Connector(mpsc::UnboundedSender<SocketAddr>);
#[async_trait::async_trait]
impl ConnectionFactory for Connector {
    async fn connect(
        &self,
        _: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        self.0
            .send(destination)
            .expect("the test must observe the exact admitted destination");
        Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
    }
}

struct Probe {
    reactor: Reactor,
    bindings: CellBindings,
    network: ResourceHandle,
    connections: mpsc::UnboundedReceiver<SocketAddr>,
}

fn handle(response: Response) -> ResourceHandle {
    match response {
        Response::Handle(handle) => handle,
        other => panic!("native resource creation must return a handle: {other:?}"),
    }
}

impl Probe {
    fn new(answer: IpAddr, allowlist: &str) -> Self {
        let (tx, connections) = mpsc::unbounded_channel();
        let mut reactor = Reactor::with_network(
            InstanceId::alloc(),
            isoplane_harness::Network::with_resolver(Box::new(Dns(answer)))
                .with_connections(Arc::new(Connector(tx))),
        );
        Arc::get_mut(&mut reactor.context)
            .expect("the new reactor context must be unshared")
            .allowlist = Some(vec![allowlist.into()]);
        reactor.context.begin_dispatch(1, 1);
        let mut bindings = CellBindings::new(reactor.context.clone());
        let network = handle(
            bindings
                .native_io(Request::Network)
                .expect("the instance must acquire its own network handle"),
        );
        Self {
            reactor,
            bindings,
            network,
            connections,
        }
    }

    async fn resolve(&mut self, name: &str) -> Result<Response, Error> {
        let addresses = handle(
            self.bindings
                .native_io(Request::Resolve {
                    network: self.network,
                    name: name.into(),
                })
                .expect("DNS must allocate its asynchronous result"),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self
                .reactor
                .futures
                .has_ready(self.reactor.context.instance)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the controlled resolver must complete");
        self.bindings.native_io(Request::NextAddress(addresses))
    }

    fn connect(&mut self, address: IpAddr) -> Result<Response, Error> {
        let family = match address {
            IpAddr::V4(_) => AddressFamily::V4,
            IpAddr::V6(_) => AddressFamily::V6,
        };
        let socket = handle(
            self.bindings
                .native_io(Request::CreateTcp(family))
                .expect("the requested socket family must be available"),
        );
        self.bindings.native_io(Request::Connect {
            socket,
            network: self.network,
            destination: SocketAddr::new(address, 443),
        })
    }

    async fn admitted(&mut self, address: IpAddr) {
        assert!(
            matches!(self.connect(address), Ok(Response::Done)),
            "the DNS-authorized exact address representation must start connecting"
        );
        let actual = tokio::time::timeout(Duration::from_secs(10), self.connections.recv())
            .await
            .expect("an admitted connection must reach its provider")
            .expect("the provider must report its destination");
        assert_eq!(
            actual,
            SocketAddr::new(address, 443),
            "provenance recognition must not change the dialed destination"
        );
    }

    fn denied(&mut self, address: IpAddr) {
        assert!(
            matches!(self.connect(address), Err(Error::Denied(_))),
            "an address without allowed authority must be refused before connecting"
        );
        assert!(
            matches!(
                self.connections.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "a refused address must never reach the connection provider"
        );
    }
}

async fn approved(probe: &mut Probe, address: IpAddr) {
    assert!(
        matches!(probe.resolve("allowed.test").await, Ok(Response::Address(Some(answer))) if answer == address),
        "the approved DNS answer must be delivered and recorded unchanged"
    );
}

#[tokio::test]
async fn an_approved_ipv4_dns_answer_authorizes_its_exact_mapped_address() {
    let address = IpAddr::V4(PUBLIC);
    let mut probe = Probe::new(address, "allowed.test");
    approved(&mut probe, address).await;
    probe.admitted(IpAddr::V6(PUBLIC.to_ipv6_mapped())).await;
}

#[tokio::test]
async fn an_approved_mapped_dns_answer_authorizes_its_exact_ipv4_address() {
    let address = IpAddr::V6(PUBLIC.to_ipv6_mapped());
    let mut probe = Probe::new(address, "allowed.test");
    approved(&mut probe, address).await;
    probe.admitted(IpAddr::V4(PUBLIC)).await;
}

#[tokio::test]
async fn dns_authority_does_not_extend_to_a_different_mapped_address() {
    let address = IpAddr::V4(PUBLIC);
    let mut probe = Probe::new(address, "allowed.test");
    approved(&mut probe, address).await;
    probe.denied(IpAddr::V6(Ipv4Addr::new(93, 184, 216, 35).to_ipv6_mapped()));
}

#[tokio::test]
async fn an_unresolved_mapped_address_has_no_dns_authority() {
    let mut probe = Probe::new(IpAddr::V4(PUBLIC), "allowed.test");
    probe.denied(IpAddr::V6(PUBLIC.to_ipv6_mapped()));
}

#[tokio::test]
async fn a_denied_name_does_not_grant_mapped_address_authority() {
    let mut probe = Probe::new(IpAddr::V4(PUBLIC), "allowed.test");
    assert!(
        matches!(probe.resolve("denied.test").await, Err(Error::Denied(_))),
        "a denied name must fail before recording its answer"
    );
    probe.denied(IpAddr::V6(PUBLIC.to_ipv6_mapped()));
}

#[tokio::test]
async fn a_private_dns_answer_and_its_mapped_address_remain_denied() {
    let private = Ipv4Addr::new(127, 0, 0, 1);
    let mut probe = Probe::new(IpAddr::V4(private), "allowed.test");
    assert!(
        matches!(probe.resolve("allowed.test").await, Err(Error::Denied(_))),
        "DNS must not grant authority to a forbidden private address"
    );
    probe.denied(IpAddr::V6(private.to_ipv6_mapped()));
}

#[tokio::test]
async fn ipv4_compatible_ipv6_is_not_treated_as_ipv4_mapped_authority() {
    let address = IpAddr::V4(PUBLIC);
    let mut probe = Probe::new(address, "allowed.test");
    approved(&mut probe, address).await;
    probe.denied(
        "::93.184.216.34"
            .parse()
            .expect("the test's IPv4-compatible literal must parse"),
    );
}

#[tokio::test]
async fn literal_ipv4_allowlist_admits_only_its_exact_mapped_equivalent_without_dns() {
    let mut probe = Probe::new(IpAddr::V4(PUBLIC), &PUBLIC.to_string());
    probe.denied(IpAddr::V6(Ipv4Addr::new(93, 184, 216, 35).to_ipv6_mapped()));
    probe.admitted(IpAddr::V6(PUBLIC.to_ipv6_mapped())).await;
    probe.admitted(IpAddr::V4(PUBLIC)).await;
}
