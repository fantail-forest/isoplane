//! The `wasi:http` 0.2.0 entries of the canonical table: the client surface
//! the runtime's `WasiHttpHandler` reaches from managed code, delegated to
//! the host's outbound HTTP capability. Server-side resources and the
//! request options the client never sets trap on call.

use super::{Action, Definition, HttpAction, OperationId, HTTP_IMPORT};
use crate::imports::{Sig, Ty};
use crate::native_io::ResourceKind;
use crate::pal::{Delegation, Disposition, PalDef};

/// The interface carrying the HTTP resources.
pub const TYPES: &str = "wasi:http/types@0.2.0";
/// The interface issuing an outgoing request.
pub const OUTGOING_HANDLER: &str = "wasi:http/outgoing-handler@0.2.0";

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// An entry reached only from managed code, so no libc wrapper carries it.
const fn managed(
    id: u32,
    module: &'static str,
    name: &'static str,
    params: &'static [Ty],
    results: &'static [Ty],
    action: Action,
) -> Definition {
    Definition {
        id: OperationId(id),
        symbol: None,
        capability: HTTP_IMPORT,
        action,
        pal: PalDef {
            module,
            func: name,
            sig: Some(Sig { params, results }),
            disposition: Disposition::Delegate(Delegation::Cap(HTTP_IMPORT)),
            why:
                "host-owned outbound HTTP with TLS terminated at the host under the egress policy; \
                  serving requests and request options remain unsupported",
        },
    }
}

const fn http(
    id: u32,
    name: &'static str,
    params: &'static [Ty],
    results: &'static [Ty],
    action: HttpAction,
) -> Definition {
    managed(id, TYPES, name, params, results, Action::Http(action))
}

const fn refused(
    id: u32,
    name: &'static str,
    params: &'static [Ty],
    results: &'static [Ty],
) -> Definition {
    managed(id, TYPES, name, params, results, Action::Trap)
}

const fn drop(id: u32, name: &'static str, kind: ResourceKind) -> Definition {
    managed(id, TYPES, name, &[I32], &[], Action::Drop(kind))
}

/// Every `wasi:http` entry, ids 76 onward.
pub const DEFINITIONS: &[Definition] = &[
    // fields
    drop(76, "[resource-drop]fields", ResourceKind::Fields),
    http(
        77,
        "[constructor]fields",
        &[],
        &[I32],
        HttpAction::FieldsNew,
    ),
    http(
        78,
        "[static]fields.from-list",
        &[I32, I32, I32],
        &[],
        HttpAction::FieldsFromList,
    ),
    http(
        79,
        "[method]fields.get",
        &[I32, I32, I32, I32],
        &[],
        HttpAction::FieldsGet,
    ),
    http(
        80,
        "[method]fields.has",
        &[I32, I32, I32],
        &[I32],
        HttpAction::FieldsHas,
    ),
    http(
        81,
        "[method]fields.set",
        &[I32, I32, I32, I32, I32, I32],
        &[],
        HttpAction::FieldsSet,
    ),
    http(
        82,
        "[method]fields.delete",
        &[I32, I32, I32, I32],
        &[],
        HttpAction::FieldsDelete,
    ),
    http(
        83,
        "[method]fields.append",
        &[I32, I32, I32, I32, I32, I32],
        &[],
        HttpAction::FieldsAppend,
    ),
    http(
        84,
        "[method]fields.entries",
        &[I32, I32],
        &[],
        HttpAction::FieldsEntries,
    ),
    http(
        85,
        "[method]fields.clone",
        &[I32],
        &[I32],
        HttpAction::FieldsClone,
    ),
    // incoming-request: a guest never serves
    refused(86, "[resource-drop]incoming-request", &[I32], &[]),
    refused(87, "[method]incoming-request.method", &[I32, I32], &[]),
    refused(
        88,
        "[method]incoming-request.path-with-query",
        &[I32, I32],
        &[],
    ),
    refused(89, "[method]incoming-request.scheme", &[I32, I32], &[]),
    refused(90, "[method]incoming-request.authority", &[I32, I32], &[]),
    refused(91, "[method]incoming-request.headers", &[I32], &[I32]),
    refused(92, "[method]incoming-request.consume", &[I32, I32], &[]),
    // outgoing-request
    drop(
        93,
        "[resource-drop]outgoing-request",
        ResourceKind::OutgoingRequest,
    ),
    http(
        94,
        "[constructor]outgoing-request",
        &[I32],
        &[I32],
        HttpAction::RequestNew,
    ),
    http(
        95,
        "[method]outgoing-request.body",
        &[I32, I32],
        &[],
        HttpAction::RequestBody,
    ),
    refused(96, "[method]outgoing-request.method", &[I32, I32], &[]),
    http(
        97,
        "[method]outgoing-request.set-method",
        &[I32, I32, I32, I32],
        &[I32],
        HttpAction::RequestSetMethod,
    ),
    refused(
        98,
        "[method]outgoing-request.path-with-query",
        &[I32, I32],
        &[],
    ),
    http(
        99,
        "[method]outgoing-request.set-path-with-query",
        &[I32, I32, I32, I32],
        &[I32],
        HttpAction::RequestSetPath,
    ),
    refused(100, "[method]outgoing-request.scheme", &[I32, I32], &[]),
    http(
        101,
        "[method]outgoing-request.set-scheme",
        &[I32, I32, I32, I32, I32],
        &[I32],
        HttpAction::RequestSetScheme,
    ),
    refused(102, "[method]outgoing-request.authority", &[I32, I32], &[]),
    http(
        103,
        "[method]outgoing-request.set-authority",
        &[I32, I32, I32, I32],
        &[I32],
        HttpAction::RequestSetAuthority,
    ),
    http(
        104,
        "[method]outgoing-request.headers",
        &[I32],
        &[I32],
        HttpAction::RequestHeaders,
    ),
    // request-options: the client passes none; timeouts are its own
    refused(105, "[resource-drop]request-options", &[I32], &[]),
    refused(106, "[constructor]request-options", &[], &[I32]),
    refused(
        107,
        "[method]request-options.connect-timeout",
        &[I32, I32],
        &[],
    ),
    refused(
        108,
        "[method]request-options.set-connect-timeout",
        &[I32, I32, I64],
        &[I32],
    ),
    refused(
        109,
        "[method]request-options.first-byte-timeout",
        &[I32, I32],
        &[],
    ),
    refused(
        110,
        "[method]request-options.set-first-byte-timeout",
        &[I32, I32, I64],
        &[I32],
    ),
    refused(
        111,
        "[method]request-options.between-bytes-timeout",
        &[I32, I32],
        &[],
    ),
    refused(
        112,
        "[method]request-options.set-between-bytes-timeout",
        &[I32, I32, I64],
        &[I32],
    ),
    // response-outparam: serving
    refused(113, "[resource-drop]response-outparam", &[I32], &[]),
    refused(
        114,
        "[static]response-outparam.set",
        &[I32, I32, I32, I32, I64, I32, I32, I32, I32],
        &[],
    ),
    // incoming-response
    drop(
        115,
        "[resource-drop]incoming-response",
        ResourceKind::IncomingResponse,
    ),
    http(
        116,
        "[method]incoming-response.status",
        &[I32],
        &[I32],
        HttpAction::ResponseStatus,
    ),
    http(
        117,
        "[method]incoming-response.headers",
        &[I32],
        &[I32],
        HttpAction::ResponseHeaders,
    ),
    http(
        118,
        "[method]incoming-response.consume",
        &[I32, I32],
        &[],
        HttpAction::ResponseConsume,
    ),
    // incoming-body
    drop(
        119,
        "[resource-drop]incoming-body",
        ResourceKind::IncomingBody,
    ),
    http(
        120,
        "[method]incoming-body.stream",
        &[I32, I32],
        &[],
        HttpAction::IncomingBodyStream,
    ),
    http(
        121,
        "[static]incoming-body.finish",
        &[I32],
        &[I32],
        HttpAction::IncomingBodyFinish,
    ),
    // future-trailers: the client drops them unread
    drop(
        122,
        "[resource-drop]future-trailers",
        ResourceKind::FutureTrailers,
    ),
    refused(123, "[method]future-trailers.subscribe", &[I32], &[I32]),
    refused(124, "[method]future-trailers.get", &[I32, I32], &[]),
    // outgoing-response: serving
    refused(125, "[resource-drop]outgoing-response", &[I32], &[]),
    refused(126, "[constructor]outgoing-response", &[I32], &[I32]),
    refused(127, "[method]outgoing-response.status-code", &[I32], &[I32]),
    refused(
        128,
        "[method]outgoing-response.set-status-code",
        &[I32, I32],
        &[I32],
    ),
    refused(129, "[method]outgoing-response.headers", &[I32], &[I32]),
    refused(130, "[method]outgoing-response.body", &[I32, I32], &[]),
    // outgoing-body
    drop(
        131,
        "[resource-drop]outgoing-body",
        ResourceKind::OutgoingBody,
    ),
    http(
        132,
        "[method]outgoing-body.write",
        &[I32, I32],
        &[],
        HttpAction::OutgoingBodyWrite,
    ),
    http(
        133,
        "[static]outgoing-body.finish",
        &[I32, I32, I32, I32],
        &[],
        HttpAction::OutgoingBodyFinish,
    ),
    // future-incoming-response
    drop(
        134,
        "[resource-drop]future-incoming-response",
        ResourceKind::FutureResponse,
    ),
    http(
        135,
        "[method]future-incoming-response.subscribe",
        &[I32],
        &[I32],
        HttpAction::FutureSubscribe,
    ),
    http(
        136,
        "[method]future-incoming-response.get",
        &[I32, I32],
        &[],
        HttpAction::FutureGet,
    ),
    refused(137, "http-error-code", &[I32, I32], &[]),
    // outgoing-handler
    managed(
        138,
        OUTGOING_HANDLER,
        "handle",
        &[I32, I32, I32, I32],
        &[],
        Action::Http(HttpAction::Handle),
    ),
];
