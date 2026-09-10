//! The instance's `wasi:http` resources: header fields, an outgoing request
//! and the body it streams, the future response the guest parks on, and the
//! response and body it reads. Every exchange is issued by the host under
//! the egress policy through the cell's transport; the guest only ever holds
//! handles, and the readiness of each is a host future.

use super::*;
use crate::bindings::{BodyFrame, ExchangeFailure, OutboundRequest, RequestBody, TlsFailure};
use crate::bindings::{OutboundBody, ResponseBody};
use crate::egress::{CheckedConnection, DenyReason, EgressError};
use tokio::sync::mpsc;
use wisp_abi::native_io::http::{self, ErrorCode, Field, HeaderError, Method, Scheme};
use wisp_abi::native_io::ByteCount;

/// How many frames the request body channel holds before a write waits.
const BODY_FRAMES: usize = 8;
/// The most one body write accepts, and what check-write offers.
const BODY_WRITE_BYTES: u64 = 64 * 1024;
/// How much of a response body one host read fetches.
const BODY_READ_BYTES: usize = 64 * 1024;

/// A header field set; immutable once it belongs to a request or response.
pub(super) struct Fields {
    entries: Vec<Field>,
    mutable: bool,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn valid_value(value: &[u8]) -> bool {
    !value.contains(&b'\r') && !value.contains(&b'\n') && !value.contains(&0)
}

/// Headers the guest may not set; the transport owns framing and the host
/// owns the connection.
fn forbidden_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "transfer-encoding" | "connection" | "upgrade" | "keep-alive"
    )
}

impl Fields {
    fn check(&self, name: &str, values: &[Vec<u8>]) -> Result<(), HeaderError> {
        if !self.mutable {
            return Err(HeaderError::Immutable);
        }
        if !valid_name(name) || values.iter().any(|value| !valid_value(value)) {
            return Err(HeaderError::InvalidSyntax);
        }
        if forbidden_name(name) {
            return Err(HeaderError::Forbidden);
        }
        Ok(())
    }
}

/// The transport's end of the request body's channel, created when the
/// guest asks for the body and handed to the exchange when the request is
/// issued.
struct BodyChannel {
    receiver: Option<mpsc::Receiver<BodyFrame>>,
}

pub(super) struct OutgoingRequest {
    headers: Vec<Field>,
    method: Method,
    scheme: Option<Scheme>,
    authority: Option<String>,
    path: Option<String>,
    body: Option<BodyChannel>,
    body_taken: bool,
}

/// The guest's side of a request body: it hands frames to the transport and
/// ends the body with [`BodyFrame::End`].
pub(super) struct OutgoingBody {
    sender: mpsc::Sender<BodyFrame>,
    stream_taken: bool,
}

/// The output stream of a request body: bounded by the channel's capacity,
/// with a host future for the next free slot.
pub(super) struct HttpOutput {
    sender: mpsc::Sender<BodyFrame>,
    task: Option<ReactorWork>,
}

/// What an exchange came back with, held until the guest disposes of it.
struct Arrived {
    status: u16,
    headers: Vec<Field>,
    body: Option<OutboundBody>,
    /// The egress permit, released with the last resource of the exchange.
    permit: Arc<CheckedConnection>,
}

enum FutureState {
    Pending,
    Ready(Result<Arrived, ErrorCode>),
    Taken,
}

/// The exchange in flight on the reactor; the guest parks on its subscription.
pub(super) struct FutureResponse {
    state: Arc<Mutex<FutureState>>,
    _task: Option<ReactorWork>,
}

pub(super) struct IncomingResponse {
    status: u16,
    headers: Vec<Field>,
    body: Option<OutboundBody>,
    permit: Arc<CheckedConnection>,
}

pub(super) struct IncomingBody {
    body: Option<OutboundBody>,
    permit: Arc<CheckedConnection>,
}

/// The bytes of a response body as they arrive: one host read in flight at
/// a time, its chunk kept until the guest takes it.
struct BodySource {
    buffered: Vec<u8>,
    body: Option<Box<dyn ResponseBody>>,
    reading: bool,
    eof: bool,
    failed: Option<String>,
}

/// The input stream of a response body.
pub(super) struct HttpInput {
    source: Arc<Mutex<BodySource>>,
    task: Option<ReactorWork>,
    _permit: Arc<CheckedConnection>,
}

impl HttpInput {
    fn new(body: OutboundBody, permit: Arc<CheckedConnection>) -> Self {
        let source = match body {
            OutboundBody::Inline(bytes) => BodySource {
                buffered: bytes,
                body: None,
                reading: false,
                eof: true,
                failed: None,
            },
            OutboundBody::Stream(body) => BodySource {
                buffered: Vec::new(),
                body: Some(body),
                reading: false,
                eof: false,
                failed: None,
            },
        };
        Self {
            source: Arc::new(Mutex::new(source)),
            task: None,
            _permit: permit,
        }
    }

    pub(super) fn ready(&self) -> bool {
        let source = self.source.lock();
        !source.buffered.is_empty() || source.eof || source.failed.is_some()
    }

    pub(super) fn read(
        &mut self,
        length: ByteCount,
        bindings: &CellBindings,
    ) -> Result<Response, Error> {
        let length = usize::try_from(length.0)
            .unwrap_or(usize::MAX)
            .min(wisp_abi::imports::MAX_OUT_BUFFER_BYTES);
        let mut source = self.source.lock();
        if !source.buffered.is_empty() {
            let count = length.min(source.buffered.len());
            let bytes: Vec<u8> = source.buffered.drain(..count).collect();
            return Ok(Response::Bytes(bytes));
        }
        if let Some(message) = &source.failed {
            return Err(Error::Connection {
                kind: std::io::ErrorKind::ConnectionAborted,
                message: message.clone(),
            });
        }
        if source.eof {
            return Ok(Response::Closed);
        }
        if !source.reading {
            let Some(mut body) = source.body.take() else {
                source.eof = true;
                return Ok(Response::Closed);
            };
            source.reading = true;
            let shared = self.source.clone();
            let egress = bindings
                .rt()
                .ok_or(Error::Unsupported("response body without reactor"))?
                .services
                .egress
                .clone();
            self.task = Some(streams::watch(
                bindings,
                wisp_abi::native_io::canonical::HTTP_IMPORT,
                async move {
                    let chunk = body.read(BODY_READ_BYTES).await;
                    let mut source = shared.lock();
                    source.reading = false;
                    match chunk {
                        Ok(chunk) => {
                            egress.record_bytes(chunk.data.len() as u64);
                            source.buffered.extend_from_slice(&chunk.data);
                            if chunk.eof {
                                source.eof = true;
                            } else {
                                source.body = Some(body);
                            }
                        }
                        Err(error) => source.failed = Some(error.to_string()),
                    }
                },
            )?);
        }
        Ok(Response::Bytes(Vec::new()))
    }
}

impl HttpOutput {
    pub(super) fn ready(&self) -> bool {
        self.sender.capacity() > 0 || self.sender.is_closed()
    }

    pub(super) fn check_write(&mut self, bindings: &CellBindings) -> Result<Response, Error> {
        if self.sender.is_closed() {
            return Err(Error::Connection {
                kind: std::io::ErrorKind::ConnectionAborted,
                message: "the request this body belongs to is gone".into(),
            });
        }
        if self.sender.capacity() > 0 {
            return Ok(Response::Capacity(ByteCount(BODY_WRITE_BYTES)));
        }
        if self
            .task
            .as_ref()
            .is_none_or(|work| work.task.is_finished())
        {
            let sender = self.sender.clone();
            self.task = Some(streams::watch(
                bindings,
                wisp_abi::native_io::canonical::HTTP_IMPORT,
                async move {
                    // A reserved slot handed straight back is the readiness.
                    drop(sender.reserve().await);
                },
            )?);
        }
        Ok(Response::Capacity(ByteCount(0)))
    }

    pub(super) fn write(
        &mut self,
        bytes: Vec<u8>,
        bindings: &CellBindings,
    ) -> Result<Response, Error> {
        if bytes.len() as u64 > BODY_WRITE_BYTES {
            return Err(Error::InvalidState(
                "write exceeds what check-write admitted",
            ));
        }
        let count = bytes.len() as u64;
        match self.sender.try_send(BodyFrame::Data(bytes.into())) {
            Ok(()) => {
                if let Some(runtime) = bindings.rt() {
                    runtime.services.egress.record_bytes(count);
                }
                Ok(Response::Done)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(Error::InvalidState("write without a preceding check-write"))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Error::Connection {
                kind: std::io::ErrorKind::ConnectionAborted,
                message: "the request this body belongs to is gone".into(),
            }),
        }
    }
}

/// The `wasi:http` error code an exchange failure is reported as.
fn failure_code(failure: &ExchangeFailure) -> ErrorCode {
    use crate::egress::ConnectionError;
    match failure {
        ExchangeFailure::Uri(_) => ErrorCode::HttpRequestUriInvalid,
        ExchangeFailure::Connect(ConnectionError::Denied(reason)) => deny_code(reason),
        ExchangeFailure::Connect(ConnectionError::Io(error)) => match error.kind() {
            std::io::ErrorKind::ConnectionRefused => ErrorCode::ConnectionRefused,
            std::io::ErrorKind::TimedOut => ErrorCode::ConnectionTimeout,
            std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                ErrorCode::DestinationIpUnroutable
            }
            _ => ErrorCode::DestinationUnavailable,
        },
        ExchangeFailure::Connect(ConnectionError::PeerMismatch { .. }) => {
            ErrorCode::DestinationUnavailable
        }
        ExchangeFailure::Tls(TlsFailure::Certificate) => ErrorCode::TlsCertificateError,
        ExchangeFailure::Tls(TlsFailure::Alert) => ErrorCode::TlsAlertReceived,
        ExchangeFailure::Tls(TlsFailure::Protocol) => ErrorCode::TlsProtocolError,
        ExchangeFailure::Protocol(_) => ErrorCode::HttpProtocolError,
        ExchangeFailure::Internal(_) => ErrorCode::InternalError,
    }
}

fn deny_code(reason: &DenyReason) -> ErrorCode {
    match reason {
        DenyReason::Scheme(_) | DenyReason::Malformed(_) => ErrorCode::HttpRequestUriInvalid,
        DenyReason::PrivateAddress(_) => ErrorCode::DestinationIpProhibited,
        DenyReason::NotAllowlisted(_) | DenyReason::BandwidthQuota { .. } => {
            ErrorCode::HttpRequestDenied
        }
        DenyReason::ConnectionQuota { .. } => ErrorCode::ConnectionLimitReached,
    }
}

fn egress_code(error: &EgressError) -> ErrorCode {
    match error {
        EgressError::Denied(reason) => deny_code(reason),
        EgressError::Dns(_) => ErrorCode::DnsError,
    }
}

impl Resources {
    /// Whether an HTTP exchange is in flight or its response unread.
    pub(crate) fn has_http_exchange(&self) -> bool {
        self.entries.values().any(|resource| {
            matches!(
                resource,
                Resource::OutgoingBody(_)
                    | Resource::HttpOutput(_)
                    | Resource::FutureResponse(_)
                    | Resource::IncomingResponse(_)
                    | Resource::IncomingBody(_)
                    | Resource::HttpInput(_)
            )
        })
    }

    fn fields(&self, handle: ResourceHandle) -> Result<&Fields, Error> {
        match self.entries.get(&handle) {
            Some(Resource::Fields(fields)) => Ok(fields),
            _ => Err(Error::InvalidHandle(handle)),
        }
    }

    fn fields_mut(&mut self, handle: ResourceHandle) -> Result<&mut Fields, Error> {
        match self.entries.get_mut(&handle) {
            Some(Resource::Fields(fields)) => Ok(fields),
            _ => Err(Error::InvalidHandle(handle)),
        }
    }

    fn outgoing_request(&mut self, handle: ResourceHandle) -> Result<&mut OutgoingRequest, Error> {
        match self.entries.get_mut(&handle) {
            Some(Resource::OutgoingRequest(request)) => Ok(request),
            _ => Err(Error::InvalidHandle(handle)),
        }
    }

    fn new_fields(&mut self, entries: Vec<Field>, mutable: bool) -> Result<http::Response, Error> {
        self.insert(Resource::Fields(Fields { entries, mutable }))
            .map(http::Response::Handle)
    }

    /// Answer one `wasi:http` operation.
    pub(super) fn http(
        &mut self,
        request: http::Request,
        bindings: &CellBindings,
    ) -> Result<http::Response, Error> {
        match request {
            http::Request::FieldsNew => self.new_fields(Vec::new(), true),
            http::Request::FieldsFromList(entries) => {
                let scratch = Fields {
                    entries: Vec::new(),
                    mutable: true,
                };
                for (name, value) in &entries {
                    if let Err(error) = scratch.check(name, std::slice::from_ref(value)) {
                        return Ok(http::Response::HeaderError(error));
                    }
                }
                self.new_fields(entries, true)
            }
            http::Request::FieldsGet { fields, name } => {
                let fields = self.fields(fields)?;
                Ok(http::Response::Values(
                    fields
                        .entries
                        .iter()
                        .filter(|(key, _)| key.eq_ignore_ascii_case(&name))
                        .map(|(_, value)| value.clone())
                        .collect(),
                ))
            }
            http::Request::FieldsHas { fields, name } => {
                let fields = self.fields(fields)?;
                Ok(http::Response::Bool(
                    fields
                        .entries
                        .iter()
                        .any(|(key, _)| key.eq_ignore_ascii_case(&name)),
                ))
            }
            http::Request::FieldsSet {
                fields,
                name,
                values,
            } => {
                let fields = self.fields_mut(fields)?;
                if let Err(error) = fields.check(&name, &values) {
                    return Ok(http::Response::HeaderError(error));
                }
                fields
                    .entries
                    .retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
                fields
                    .entries
                    .extend(values.into_iter().map(|value| (name.clone(), value)));
                Ok(http::Response::Done)
            }
            http::Request::FieldsDelete { fields, name } => {
                let fields = self.fields_mut(fields)?;
                if let Err(error) = fields.check(&name, &[]) {
                    return Ok(http::Response::HeaderError(error));
                }
                fields
                    .entries
                    .retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
                Ok(http::Response::Done)
            }
            http::Request::FieldsAppend {
                fields,
                name,
                value,
            } => {
                let fields = self.fields_mut(fields)?;
                if let Err(error) = fields.check(&name, std::slice::from_ref(&value)) {
                    return Ok(http::Response::HeaderError(error));
                }
                fields.entries.push((name, value));
                Ok(http::Response::Done)
            }
            http::Request::FieldsEntries(handle) => Ok(http::Response::Entries(
                self.fields(handle)?.entries.clone(),
            )),
            http::Request::FieldsClone(handle) => {
                let entries = self.fields(handle)?.entries.clone();
                self.new_fields(entries, true)
            }
            http::Request::RequestNew { headers } => {
                let Some(Resource::Fields(fields)) = self.entries.remove(&headers) else {
                    return Err(Error::InvalidHandle(headers));
                };
                self.insert(Resource::OutgoingRequest(OutgoingRequest {
                    headers: fields.entries,
                    method: Method::Get,
                    scheme: None,
                    authority: None,
                    path: None,
                    body: None,
                    body_taken: false,
                }))
                .map(http::Response::Handle)
            }
            http::Request::RequestBody(handle) => {
                self.reserve(ResourceCount(1))?;
                let request = self.outgoing_request(handle)?;
                if request.body_taken {
                    return Ok(http::Response::Refused);
                }
                let (sender, receiver) = mpsc::channel(BODY_FRAMES);
                request.body = Some(BodyChannel {
                    receiver: Some(receiver),
                });
                request.body_taken = true;
                self.insert(Resource::OutgoingBody(OutgoingBody {
                    sender,
                    stream_taken: false,
                }))
                .map(http::Response::Handle)
            }
            http::Request::RequestSetMethod { request, method } => {
                let valid = match &method {
                    Method::Other(token) => valid_name(token),
                    _ => true,
                };
                if !valid {
                    return Ok(http::Response::Refused);
                }
                self.outgoing_request(request)?.method = method;
                Ok(http::Response::Done)
            }
            http::Request::RequestSetScheme { request, scheme } => {
                if let Some(Scheme::Other(_)) = scheme {
                    return Ok(http::Response::Refused);
                }
                self.outgoing_request(request)?.scheme = scheme;
                Ok(http::Response::Done)
            }
            http::Request::RequestSetAuthority { request, authority } => {
                if authority
                    .as_deref()
                    .is_some_and(|authority| authority.contains(['/', '?', '#', '@', ' ']))
                {
                    return Ok(http::Response::Refused);
                }
                self.outgoing_request(request)?.authority = authority;
                Ok(http::Response::Done)
            }
            http::Request::RequestSetPath { request, path } => {
                if path
                    .as_deref()
                    .is_some_and(|path| !path.starts_with('/') || path.contains([' ', '#']))
                {
                    return Ok(http::Response::Refused);
                }
                self.outgoing_request(request)?.path = path;
                Ok(http::Response::Done)
            }
            http::Request::RequestHeaders(handle) => {
                let headers = self.outgoing_request(handle)?.headers.clone();
                self.new_fields(headers, false)
            }
            http::Request::OutgoingBodyWrite(handle) => {
                self.reserve(ResourceCount(1))?;
                let Some(Resource::OutgoingBody(body)) = self.entries.get_mut(&handle) else {
                    return Err(Error::InvalidHandle(handle));
                };
                if body.stream_taken {
                    return Ok(http::Response::Refused);
                }
                body.stream_taken = true;
                let sender = body.sender.clone();
                self.insert(Resource::HttpOutput(HttpOutput { sender, task: None }))
                    .map(http::Response::Handle)
            }
            http::Request::OutgoingBodyFinish { body, trailers } => {
                let Some(Resource::OutgoingBody(outgoing)) = self.entries.remove(&body) else {
                    return Err(Error::InvalidHandle(body));
                };
                if let Some(trailers) = trailers {
                    // Trailers are not sent; the fields are consumed all the same.
                    self.entries.remove(&trailers);
                }
                Ok(match outgoing.sender.try_send(BodyFrame::End) {
                    Ok(()) => http::Response::Done,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        http::Response::Failed(ErrorCode::InternalError)
                    }
                    // The request is gone; the body ends with it.
                    Err(mpsc::error::TrySendError::Closed(_)) => http::Response::Done,
                })
            }
            http::Request::Handle { request, options } => {
                if let Some(options) = options {
                    self.entries.remove(&options);
                }
                self.reserve(ResourceCount(1))?;
                let Some(Resource::OutgoingRequest(mut outgoing)) = self.entries.remove(&request)
                else {
                    return Err(Error::InvalidHandle(request));
                };
                let Some(url) = url_of(&outgoing) else {
                    return Ok(http::Response::Failed(ErrorCode::HttpRequestUriInvalid));
                };
                let body = match outgoing.body.take().and_then(|channel| channel.receiver) {
                    Some(receiver) => RequestBody::Stream(receiver),
                    None => RequestBody::Empty,
                };
                self.issue(
                    bindings,
                    OutboundRequest {
                        method: outgoing.method.as_str().to_owned(),
                        url,
                        headers: outgoing.headers,
                        body,
                    },
                )
            }
            http::Request::FutureSubscribe(handle) => {
                if !matches!(self.entries.get(&handle), Some(Resource::FutureResponse(_))) {
                    return Err(Error::InvalidHandle(handle));
                }
                self.insert(Resource::Subscription(handle))
                    .map(http::Response::Handle)
            }
            http::Request::FutureGet(handle) => {
                let Some(Resource::FutureResponse(future)) = self.entries.get(&handle) else {
                    return Err(Error::InvalidHandle(handle));
                };
                let state = future.state.clone();
                let mut state = state.lock();
                match std::mem::replace(&mut *state, FutureState::Taken) {
                    FutureState::Pending => {
                        *state = FutureState::Pending;
                        Ok(http::Response::Pending)
                    }
                    FutureState::Taken => Ok(http::Response::Taken),
                    FutureState::Ready(Err(code)) => Ok(http::Response::Failed(code)),
                    FutureState::Ready(Ok(arrived)) => {
                        drop(state);
                        self.insert(Resource::IncomingResponse(IncomingResponse {
                            status: arrived.status,
                            headers: arrived.headers,
                            body: arrived.body,
                            permit: arrived.permit,
                        }))
                        .map(http::Response::Handle)
                    }
                }
            }
            http::Request::ResponseStatus(handle) => match self.entries.get(&handle) {
                Some(Resource::IncomingResponse(response)) => {
                    Ok(http::Response::Status(response.status))
                }
                _ => Err(Error::InvalidHandle(handle)),
            },
            http::Request::ResponseHeaders(handle) => {
                let headers = match self.entries.get(&handle) {
                    Some(Resource::IncomingResponse(response)) => response.headers.clone(),
                    _ => return Err(Error::InvalidHandle(handle)),
                };
                self.new_fields(headers, false)
            }
            http::Request::ResponseConsume(handle) => {
                self.reserve(ResourceCount(1))?;
                let Some(Resource::IncomingResponse(response)) = self.entries.get_mut(&handle)
                else {
                    return Err(Error::InvalidHandle(handle));
                };
                let Some(body) = response.body.take() else {
                    return Ok(http::Response::Refused);
                };
                let permit = response.permit.clone();
                self.insert(Resource::IncomingBody(IncomingBody {
                    body: Some(body),
                    permit,
                }))
                .map(http::Response::Handle)
            }
            http::Request::IncomingBodyStream(handle) => {
                self.reserve(ResourceCount(1))?;
                let Some(Resource::IncomingBody(incoming)) = self.entries.get_mut(&handle) else {
                    return Err(Error::InvalidHandle(handle));
                };
                let Some(body) = incoming.body.take() else {
                    return Ok(http::Response::Refused);
                };
                let permit = incoming.permit.clone();
                self.insert(Resource::HttpInput(HttpInput::new(body, permit)))
                    .map(http::Response::Handle)
            }
            http::Request::IncomingBodyFinish(handle) => {
                let Some(Resource::IncomingBody(_)) = self.entries.remove(&handle) else {
                    return Err(Error::InvalidHandle(handle));
                };
                self.insert(Resource::FutureTrailers)
                    .map(http::Response::Handle)
            }
        }
    }

    /// Issue `request` on the reactor: the egress check, the exchange, and
    /// the response head into the future the guest parks on.
    fn issue(
        &mut self,
        bindings: &CellBindings,
        request: OutboundRequest,
    ) -> Result<http::Response, Error> {
        let runtime = bindings
            .rt()
            .ok_or(Error::Unsupported("outbound HTTP without reactor"))?
            .clone();
        let owner =
            crate::egress::ConnectionOwner::new(bindings.ctx.tenant.clone(), bindings.ctx.instance)
                .map_err(Error::from)?;
        let future = bindings
            .alloc(wisp_abi::native_io::canonical::HTTP_IMPORT)
            .ok_or(Error::ResourceLimit)?;
        let state = Arc::new(Mutex::new(FutureState::Pending));
        let shared = state.clone();
        let completion = runtime.clone();
        let services = runtime.services.clone();
        let allowlist = bindings.ctx.allowlist.clone();
        let tenant = bindings.ctx.tenant.clone();
        let instance = bindings.ctx.instance;
        let url = request.url.clone();
        let task = runtime.reactor.spawn(async move {
            let outcome =
                async {
                    let permit = services
                    .egress
                    .check_owned(&url, allowlist.as_deref())
                    .await
                    .map_err(|error| {
                        if matches!(error, EgressError::Denied(_)) {
                            completion.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                        }
                        tracing::warn!(%tenant, %instance, %url, %error, "outbound HTTP refused");
                        egress_code(&error)
                    })?;
                    let target = permit.target();
                    let response = services
                    .fetcher
                    .exchange(owner, target, request)
                    .await
                    .map_err(|failure| {
                        tracing::warn!(%tenant, %instance, %url, %failure, "outbound HTTP failed");
                        failure_code(&failure)
                    })?;
                    Ok(Arrived {
                        status: response.status,
                        headers: response
                            .headers
                            .into_iter()
                            .map(|(name, value)| (name, value.into_bytes()))
                            .collect(),
                        body: Some(response.body),
                        permit: Arc::new(permit),
                    })
                }
                .await;
            {
                let mut current = shared.lock();
                if matches!(*current, FutureState::Pending) {
                    *current = FutureState::Ready(outcome);
                }
            }
            completion.settle_future(future, ResultRecord::Ok(Vec::new()));
        });
        runtime.futures.attach_task(future, task.abort_handle());
        self.insert(Resource::FutureResponse(FutureResponse {
            state,
            _task: Some(ReactorWork {
                future: FutureId::from(future),
                owner: bindings.ctx.instance,
                table: runtime.futures.clone(),
                task: task.abort_handle(),
            }),
        }))
        .map(http::Response::Handle)
    }

    /// Whether the future response `handle` subscribes to has arrived.
    pub(super) fn future_ready(&self, handle: ResourceHandle) -> Option<bool> {
        match self.entries.get(&handle) {
            Some(Resource::FutureResponse(future)) => {
                Some(!matches!(*future.state.lock(), FutureState::Pending))
            }
            _ => None,
        }
    }
}

/// The URL of a request whose scheme, authority and path the guest set; the
/// scheme defaults to HTTP, the path to the root.
fn url_of(request: &OutgoingRequest) -> Option<String> {
    let scheme = match request.scheme.as_ref().unwrap_or(&Scheme::Http) {
        Scheme::Http => "http",
        Scheme::Https => "https",
        Scheme::Other(_) => return None,
    };
    let authority = request.authority.as_deref()?;
    let path = request.path.as_deref().unwrap_or("/");
    Some(format!("{scheme}://{authority}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{CellRuntime, InstanceCtx, Services, WakeSink};
    use crate::egress::{DnsError, HostEgressPolicy, Resolver};
    use crate::fakes::FakeFetcher;
    use wisp_abi::config::{HandlerLifecycle, Limits};

    /// A resolver answering every name with one public address.
    struct Public;
    #[async_trait::async_trait]
    impl Resolver for Public {
        async fn resolve(&self, _: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, DnsError> {
            Ok(vec![std::net::SocketAddr::from(([93, 184, 216, 34], port))])
        }
    }

    struct Wake(tokio::sync::Notify);
    impl WakeSink for Wake {
        fn signal(&self, _: InstanceId) {
            self.0.notify_one();
        }
    }

    fn bindings() -> (CellBindings, Arc<HostEgressPolicy>, Arc<Wake>) {
        let egress = Arc::new(HostEgressPolicy::new(Box::new(Public), 2, 1 << 20));
        let wake = Arc::new(Wake(tokio::sync::Notify::new()));
        let mut context =
            InstanceCtx::for_test(InstanceId(91), HandlerLifecycle::Reuse, Limits::default());
        context.rt = Some(Arc::new(CellRuntime {
            services: Arc::new(Services {
                egress: egress.clone(),
                fetcher: Arc::new(FakeFetcher::default()),
            }),
            futures: Arc::new(FutureTable::new()),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            wake: wake.clone(),
            reactor: tokio::runtime::Handle::current(),
        }));
        context.begin_slice(1, 1);
        (CellBindings::new(Arc::new(context)), egress, wake)
    }

    fn handle(
        resources: &mut Resources,
        bindings: &CellBindings,
        request: http::Request,
    ) -> ResourceHandle {
        match resources.call(Request::Http(request), bindings) {
            Ok(Response::Http(http::Response::Handle(handle))) => handle,
            other => panic!("the operation must answer with a handle: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_exchange_holds_its_egress_permit_and_counts_as_outbound_http_until_its_body_is_released(
    ) {
        let (bindings, egress, wake) = bindings();
        let mut resources = Resources::new(ResourceLimits::default());
        let headers = handle(&mut resources, &bindings, http::Request::FieldsNew);
        let request = handle(
            &mut resources,
            &bindings,
            http::Request::RequestNew { headers },
        );
        for setter in [
            http::Request::RequestSetScheme {
                request,
                scheme: Some(Scheme::Https),
            },
            http::Request::RequestSetAuthority {
                request,
                authority: Some("upstream.test".into()),
            },
            http::Request::RequestSetPath {
                request,
                path: Some("/".into()),
            },
        ] {
            assert!(
                matches!(
                    resources.call(Request::Http(setter), &bindings),
                    Ok(Response::Http(http::Response::Done))
                ),
                "a well-formed request part must be accepted"
            );
        }
        assert!(
            !resources.has_http_exchange(),
            "a request under construction is guest state, not host work"
        );
        let future = handle(
            &mut resources,
            &bindings,
            http::Request::Handle {
                request,
                options: None,
            },
        );
        assert!(
            resources.has_http_exchange(),
            "an issued request is host work that capture must see"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), wake.0.notified())
            .await
            .expect("the exchange must settle its future");
        assert_eq!(
            egress.connections(),
            1,
            "the exchange holds the connection permit"
        );
        let response = handle(&mut resources, &bindings, http::Request::FutureGet(future));
        assert!(
            matches!(
                resources.call(Request::Http(http::Request::FutureGet(future)), &bindings),
                Ok(Response::Http(http::Response::Taken))
            ),
            "a response is taken once"
        );
        let body = handle(
            &mut resources,
            &bindings,
            http::Request::ResponseConsume(response),
        );
        let input = handle(
            &mut resources,
            &bindings,
            http::Request::IncomingBodyStream(body),
        );
        for (handle, kind) in [
            (future, ResourceKind::FutureResponse),
            (response, ResourceKind::IncomingResponse),
            (body, ResourceKind::IncomingBody),
        ] {
            resources
                .call(Request::Drop { handle, kind }, &bindings)
                .expect("a consumed resource drops");
        }
        assert!(
            matches!(
                resources.call(Request::Read { stream: input, length: ByteCount(64) }, &bindings),
                Ok(Response::Bytes(bytes)) if bytes == b"ok"
            ),
            "the body the transport answered with must be readable"
        );
        assert!(
            matches!(
                resources.call(
                    Request::Read {
                        stream: input,
                        length: ByteCount(64)
                    },
                    &bindings
                ),
                Ok(Response::Closed)
            ),
            "a drained body ends with the closed stream error"
        );
        assert!(
            resources.has_http_exchange(),
            "an open body stream keeps the exchange visible to capture"
        );
        assert_eq!(
            egress.connections(),
            1,
            "the permit lives as long as the body stream"
        );
        resources
            .call(
                Request::Drop {
                    handle: input,
                    kind: ResourceKind::Input,
                },
                &bindings,
            )
            .expect("the body stream drops");
        assert!(
            !resources.has_http_exchange(),
            "releasing the body ends the exchange"
        );
        assert_eq!(
            egress.connections(),
            0,
            "releasing the body releases the permit"
        );
    }

    #[tokio::test]
    async fn dropping_a_pending_future_response_cancels_the_exchange() {
        let (bindings, egress, _wake) = bindings();
        let mut resources = Resources::new(ResourceLimits::default());
        let headers = handle(&mut resources, &bindings, http::Request::FieldsNew);
        let request = handle(
            &mut resources,
            &bindings,
            http::Request::RequestNew { headers },
        );
        resources
            .call(
                Request::Http(http::Request::RequestSetAuthority {
                    request,
                    authority: Some("upstream.test".into()),
                }),
                &bindings,
            )
            .expect("the authority is accepted");
        let future = handle(
            &mut resources,
            &bindings,
            http::Request::Handle {
                request,
                options: None,
            },
        );
        resources
            .call(
                Request::Drop {
                    handle: future,
                    kind: ResourceKind::FutureResponse,
                },
                &bindings,
            )
            .expect("a pending future drops");
        assert!(
            !resources.has_http_exchange(),
            "a dropped future ends the exchange"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while egress.connections() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelling the exchange must release any permit it took");
    }

    #[test]
    fn header_fields_refuse_what_the_transport_owns_and_what_is_not_a_header() {
        let mut fields = Fields {
            entries: Vec::new(),
            mutable: true,
        };
        assert_eq!(
            fields.check("Host", &[b"x".to_vec()]),
            Err(HeaderError::Forbidden),
            "the host owns the connection's framing headers"
        );
        assert_eq!(
            fields.check("X Space", &[]),
            Err(HeaderError::InvalidSyntax),
            "a name outside the token syntax is not a header"
        );
        assert_eq!(
            fields.check("X-Ok", &[b"a\r\nb".to_vec()]),
            Err(HeaderError::InvalidSyntax),
            "a value carrying a line break would split the header"
        );
        assert_eq!(fields.check("X-Ok", &[b"fine".to_vec()]), Ok(()));
        fields.mutable = false;
        assert_eq!(
            fields.check("X-Ok", &[b"fine".to_vec()]),
            Err(HeaderError::Immutable),
            "response headers are read-only"
        );
    }
}
