//! Outbound operations and bodies belong to their instance through the real HTTP bindings.
#[path = "common/outbound_http.rs"]
mod common;

use bytes::Bytes;
use common::{deadline, services, Rig, Server};
use hyper::body::Frame;
use isoplane_abi::result::{BodyChunk, FutureError, ResultRecord};
use isoplane_host::InstanceId;

#[tokio::test]
async fn an_unread_response_holds_its_connection_permit_until_eof() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let mut rig = Rig::new(svc.clone(), "tenant", InstanceId(1));
        let fetch = rig.fetch();
        let exchange = server
            .requests
            .recv()
            .await
            .expect("the fetch must reach HTTP");
        exchange
            .headers
            .send(())
            .expect("the fetch must await its headers");
        let response = rig.response(fetch).await;
        assert_eq!(
            svc.egress.instance_connections(InstanceId(1)),
            1,
            "headers must not release a streaming response's permit"
        );
        let refused = rig.fetch();
        assert!(
            matches!(
                rig.result::<isoplane_abi::result::FetchResponse>(refused)
                    .await,
                Err(FutureError::Denied(_))
            ),
            "the configured connection quota must include unread bodies"
        );
        exchange
            .frames
            .send(Ok(Frame::data(Bytes::from_static(b"retained"))))
            .expect("the response must accept bytes");
        drop(exchange.frames);
        let mut bytes = Vec::new();
        loop {
            let read = rig.read(response.body_handle, 3);
            let chunk: BodyChunk = rig
                .result(read)
                .await
                .expect("the body must remain readable");
            bytes.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(
            bytes, b"retained",
            "small reads must preserve the entire response"
        );
        assert_eq!(
            svc.egress.instance_connections(InstanceId(1)),
            0,
            "EOF must release the connection permit"
        );
        let spent = rig.read(response.body_handle, 3);
        assert!(
            matches!(
                rig.result::<BodyChunk>(spent).await,
                Err(FutureError::NotFound)
            ),
            "EOF must retire its body handle"
        );
    })
    .await;
}

#[tokio::test]
async fn a_retained_http_body_cannot_be_read_by_another_tenant_or_instance() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let mut owner = Rig::new(svc.clone(), "tenant", InstanceId(1));
        let fetch = owner.fetch();
        let exchange = server.requests.recv().await.expect("the owner must fetch");
        exchange
            .headers
            .send(())
            .expect("the owner must await headers");
        let response = owner.response(fetch).await;
        for (tenant, instance) in [("intruder", InstanceId(1)), ("tenant", InstanceId(2))] {
            let mut intruder = Rig::new(svc.clone(), tenant, instance);
            let read = intruder.read(response.body_handle, 8);
            assert!(
                matches!(
                    intruder.result::<BodyChunk>(read).await,
                    Err(FutureError::NotFound)
                ),
                "a different owner must learn nothing from a guessed handle"
            );
        }
        exchange
            .frames
            .send(Ok(Frame::data(Bytes::from_static(b"private"))))
            .expect("the owner's body must remain open");
        drop(exchange.frames);
        let mut received = Vec::new();
        loop {
            let read = owner.read(response.body_handle, 0);
            let chunk: BodyChunk = owner
                .result(read)
                .await
                .expect("the owner must retain access");
            received.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(
            received, b"private",
            "denied readers must not consume the owner's bytes"
        );
    })
    .await;
}

#[tokio::test]
async fn reusing_an_instance_preserves_bodies_until_eof_without_reassigning_handles() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let mut rig = Rig::new(svc.clone(), "tenant", InstanceId(1));
        let fetch = rig.fetch();
        let first = server
            .requests
            .recv()
            .await
            .expect("the first dispatch must fetch");
        first
            .headers
            .send(())
            .expect("the first dispatch must await headers");
        let old = rig.response(fetch).await.body_handle;
        rig.ctx.begin_dispatch(0, 1);
        assert_eq!(
            svc.egress.instance_connections(InstanceId(1)),
            1,
            "request completion preserves the instance's unread body"
        );
        drop(first.frames);
        loop {
            let read = rig.read(old, 8);
            if rig
                .result::<BodyChunk>(read)
                .await
                .expect("retained body remains readable")
                .eof
            {
                break;
            }
        }
        assert_eq!(svc.egress.instance_connections(InstanceId(1)), 0);
        let fetch = rig.fetch();
        let second = server
            .requests
            .recv()
            .await
            .expect("the reused dispatch must fetch");
        second
            .headers
            .send(())
            .expect("the reused dispatch must await headers");
        let current = rig.response(fetch).await.body_handle;
        assert_ne!(
            current, old,
            "even repeated dispatch IDs must not recycle stale guest body handles"
        );
        let stale = rig.read(old, 8);
        assert!(
            matches!(
                rig.result::<BodyChunk>(stale).await,
                Err(FutureError::NotFound)
            ),
            "a stale handle must not read the next dispatch's response"
        );
    })
    .await;
}

#[tokio::test]
async fn a_body_transport_error_releases_its_connection_permit() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let mut rig = Rig::new(svc.clone(), "tenant", InstanceId(1));
        let fetch = rig.fetch();
        let exchange = server
            .requests
            .recv()
            .await
            .expect("the dispatch must fetch");
        exchange
            .headers
            .send(())
            .expect("the dispatch must await headers");
        let handle = rig.response(fetch).await.body_handle;
        let read = rig.read(handle, 8);
        exchange
            .frames
            .send(Err(std::io::Error::other("fixture body failure")))
            .expect("the server must inject the transport failure");
        assert!(
            matches!(rig.result::<BodyChunk>(read).await, Err(FutureError::Io(_))),
            "a response transport failure must reach the reader"
        );
        assert_eq!(
            svc.egress.instance_connections(InstanceId(1)),
            0,
            "a failed body must release its admission permit"
        );
    })
    .await;
}

#[tokio::test]
async fn completing_a_cell_dispatch_preserves_its_body_until_instance_discard() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let activator = std::sync::Arc::new(isoplane_harness::fakes::FakeActivator::named(
            isoplane_host::ModuleId::new("outbound-end", "v1"),
            isoplane_harness::fakes::FakeBehaviour::program([
                isoplane_harness::fakes::Step::Fetch {
                    url: "http://upstream.test/".into(),
                    times: 1,
                },
            ]),
        ));
        let (cell, response) = common::cell(svc.clone(), activator);
        let exchange = server
            .requests
            .recv()
            .await
            .expect("the Cell dispatch must fetch");
        exchange
            .headers
            .send(())
            .expect("the dispatch must await its response");
        let result = common::completed(response).await;
        assert_eq!(
            result.status, 200,
            "the guest must complete after receiving headers"
        );
        assert_eq!(
            common::capture(&cell, result.instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp)
        );
        assert_eq!(svc.egress.instance_connections(result.instance), 1);
        cell.discard_instance(result.instance)
            .expect("discard idle owner");
        assert_eq!(
            svc.egress.instance_connections(result.instance),
            0,
            "instance discard must release the retained response's permit"
        );
        cell.shutdown();
    })
    .await;
}

#[tokio::test]
async fn capture_refuses_pending_fetches_retained_bodies_and_inflight_reads() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let activator = std::sync::Arc::new(isoplane_harness::fakes::FakeActivator::named(
            isoplane_host::ModuleId::new("outbound-capture", "v1"),
            isoplane_harness::fakes::FakeBehaviour::program([
                isoplane_harness::fakes::Step::Fetch {
                    url: "http://upstream.test/".into(),
                    times: 1,
                },
                isoplane_harness::fakes::Step::Park { imports: vec![] },
            ]),
        ));
        let (cell, response) = common::cell(svc.clone(), activator.clone());
        let exchange = server
            .requests
            .recv()
            .await
            .expect("the Cell dispatch must fetch");
        let instance = cell.resident_ids()[0];
        assert_eq!(
            common::capture(&cell, instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp),
            "capture must retain a pending bounded fetch that may return a streaming handle"
        );
        exchange
            .headers
            .send(())
            .expect("the pending fetch must remain live after capture refusal");
        let head = loop {
            if let Some((_, isoplane_abi::result::ResultRecord::Ok(bytes))) =
                activator.wakes().first()
            {
                break postcard::from_bytes::<isoplane_abi::result::FetchResponse>(bytes)
                    .expect("the Cell must deliver response headers");
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(
            common::capture(&cell, instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp),
            "capture must refuse a body whose head was already delivered"
        );
        let slot = cell
            .slot(instance)
            .expect("the parked instance must remain resident");
        let mut bindings = isoplane_host::bindings::CellBindings::new(slot.ctx.clone());
        use isoplane_engine::HostBindings;
        let read = bindings.call_future(isoplane_engine::ImportCall {
            import: "http_body_read",
            payload: &[],
            scalars: [u64::from(head.body_handle), 8, 0],
        });
        assert_ne!(
            read,
            isoplane_host::futures::INVALID_FUTURE,
            "the body read must register before capture"
        );
        assert_eq!(
            common::capture(&cell, instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp),
            "a body held inside a pending read must remain visible to capture"
        );
        exchange
            .frames
            .send(Ok(Frame::data(Bytes::from_static(b"finish"))))
            .expect("the read must remain live after capture refusal");
        let result = common::completed(response).await;
        assert_eq!(
            result.status, 200,
            "the dispatch must complete when its read settles"
        );
        while slot.state() != isoplane_host::LifecycleTier::Parked {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            common::capture(&cell, instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp),
            "a partially read body remains instance-owned after request completion"
        );
        cell.discard_instance(instance)
            .expect("discard idle HTTP owner");
        assert_eq!(
            svc.egress.instance_connections(instance),
            0,
            "completed ownership must release admission"
        );
        cell.shutdown();
    })
    .await;
}

#[tokio::test]
async fn a_gc_hint_cannot_hide_a_new_fetch_from_capture() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let payload = postcard::to_allocvec(&isoplane_abi::calls::FetchRequest { method: "GET".into(), url: "http://upstream.test/".into(), headers: vec![], body: vec![] }).expect("the GC fetch request must encode");
        let data = isoplane_test_support::wat_bytes(&payload);
        let wasm = wat::parse_str(format!(r#"(module
            (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
            (import "isoplane" "http_fetch" (func $fetch (param i32 i32) (result i64)))
            (memory (export "memory") 2 2)
            (data (i32.const 1024) "{data}")
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
            (func (export "isoplane_handle") (param i32 i32) (result i64) (drop (call $timer (i64.const 600000))) (i64.const 1))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
            (func (export "isoplane_gc_hint") (param i32) (drop (call $fetch (i32.const 1024) (i32.const {})))))"#, payload.len())).expect("the GC fixture must compile");
        let engine = std::sync::Arc::new(isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig { max_instances: 4, max_memory_bytes: 2 << 20, guard_size_bytes: 64 << 20, ..Default::default() }).expect("the real engine must start"));
        let activator = isoplane_host::activator::EngineActivator::load(engine, isoplane_host::ModuleId::new("gc-fetch", "v1"), &wasm).expect("the GC fixture must load");
        let (cell, _response) = common::cell(svc, std::sync::Arc::new(activator));
        let instance = loop {
            if let Some(instance) = cell.resident_ids().first().copied() {
                if cell.instance_state(instance) == Some(isoplane_host::LifecycleTier::Parked) { break instance; }
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(common::capture(&cell, instance).await, Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp), "capture must check outbound ownership after the guest's GC hint can start work");
        let exchange = server.requests.recv().await.expect("the GC hint must issue a real HTTP request");
        drop(exchange);
        cell.shutdown();
    }).await;
}

#[tokio::test]
async fn a_discarded_slot_releases_its_body_while_its_context_remains_reachable() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let activator = std::sync::Arc::new(isoplane_harness::fakes::FakeActivator::named(
            isoplane_host::ModuleId::new("outbound-discard", "v1"),
            isoplane_harness::fakes::FakeBehaviour::program([
                isoplane_harness::fakes::Step::Fetch {
                    url: "http://upstream.test/".into(),
                    times: 1,
                },
                isoplane_harness::fakes::Step::Park {
                    imports: vec![("clock_deadline_in", 600_000)],
                },
            ]),
        ));
        let (cell, _response) = common::cell(svc.clone(), activator.clone());
        let exchange = server
            .requests
            .recv()
            .await
            .expect("the Cell must open a response body");
        exchange
            .headers
            .send(())
            .expect("the guest must await headers");
        while activator.wakes().is_empty() {
            tokio::task::yield_now().await;
        }
        let instance = cell.resident_ids()[0];
        assert_eq!(
            common::capture(&cell, instance).await,
            Err(isoplane_host::hibernate_policy::CaptureRefused::OutboundHttp),
            "the body must be live before the discard"
        );
        let slot = cell
            .slot(instance)
            .expect("the body owner must be resident");
        slot.discard();
        assert_eq!(
            svc.egress.instance_connections(instance),
            0,
            "a discarded slot cannot retain the outbound transport in its surviving context"
        );
        cell.shutdown();
    })
    .await;
}

#[tokio::test]
async fn a_pending_fetch_survives_handover_and_request_state_cleanup() {
    deadline(async {
        let mut server = Server::start().await;
        let svc = services(server.address);
        let mut rig = Rig::new(svc.clone(), "tenant", InstanceId(1));
        let fetch = rig.fetch();
        let exchange = server
            .requests
            .recv()
            .await
            .expect("pending fetch reached peer");
        assert_eq!(rig.runtime.futures.hand_to_instance(rig.ctx.instance, 1), 1);
        assert!(!rig.runtime.futures.end_dispatch(rig.ctx.instance, 1));
        rig.ctx.begin_dispatch(0, 1);
        exchange
            .headers
            .send(())
            .expect("handed-over fetch remains live");
        let ready = loop {
            if let Some(ready) = rig.runtime.futures.take_background_ready(rig.ctx.instance) {
                break ready;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(ready.future, fetch);
        let ResultRecord::Ok(bytes) = ready.result else {
            panic!("fetch must succeed after handover")
        };
        let head: isoplane_abi::result::FetchResponse =
            postcard::from_bytes(&bytes).expect("response head");
        exchange
            .frames
            .send(Ok(Frame::data(Bytes::from_static(b"handed over"))))
            .expect("body remains open");
        drop(exchange.frames);
        let mut received = Vec::new();
        loop {
            let read = rig.read(head.body_handle, 32);
            let chunk = rig
                .result::<BodyChunk>(read)
                .await
                .expect("handed-over body");
            received.extend(chunk.data);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(received, b"handed over");
        assert_eq!(svc.egress.instance_connections(rig.ctx.instance), 0);
    })
    .await;
}
