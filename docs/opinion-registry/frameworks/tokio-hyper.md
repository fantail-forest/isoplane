# tokio and hyper

## Make the accept loop generic over the stream

Have the listener hand each connection to a serving function that takes anything implementing `AsyncRead + AsyncWrite`. Wrap the TCP stream in TLS before serving; routing, upgrades, and the socket hub never see the difference.

**Not this:** Serving on a concrete `TcpStream` so that adding TLS means touching routing.

**Exception:** None.

## Use a maintained crate for protocol framing

Take WebSocket framing, HTTP parsing, and TLS from crates whose job that is. In-tree framing that passes the RFC vectors today is a maintenance burden and a fuzzing target tomorrow, and it multiplies: one copy for the server, one for a test client, one in another crate's tests.

**Not this:** Three hand-rolled WebSocket clients across the workspace.

**Exception:** A fixture that must produce deliberately malformed frames may construct bytes by hand.
