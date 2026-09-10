//! An endpoint that refuses connections, for the behaviours a refusal drives.

use std::net::SocketAddr;

/// An endpoint that refuses connections: bound so nothing else can take it,
/// never listening.
pub fn refusing_endpoint() -> (socket2::Socket, SocketAddr) {
    let reservation = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .expect("the test must reserve a non-listening TCP endpoint");
    reservation
        .bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .expect("the non-listening endpoint must bind");
    let address = reservation
        .local_addr()
        .expect("the reservation has an endpoint")
        .as_socket()
        .expect("the reservation is an IP endpoint");
    (reservation, address)
}
