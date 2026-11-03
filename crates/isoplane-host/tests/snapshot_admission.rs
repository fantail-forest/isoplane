//! Snapshot reservation refusal through the cell's public dispatch boundary.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use isoplane_abi::dispatch::{DispatchKind, DispatchRecord};
use isoplane_harness::fakes::{FakeActivator, FakeBehaviour};
use isoplane_hibernate::snapshot::{
    SnapshotBytes, SnapshotError, SnapshotReservation, SnapshotStorage,
};
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::Activator;

pub mod common;

struct ExhaustedStorage;

impl SnapshotStorage for ExhaustedStorage {
    fn reserve(&self, _: SnapshotBytes) -> Result<Box<dyn SnapshotReservation>, SnapshotError> {
        Err(SnapshotError::Io {
            operation: "reserve",
            path: "exhausted-test-storage".into(),
            source: std::io::ErrorKind::StorageFull.into(),
        })
    }
}

#[derive(Clone, Default)]
struct LogBytes(Arc<Mutex<Vec<u8>>>);

impl Write for LogBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("test log lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn exhausted_snapshot_capacity_refuses_admission_before_guest_activation() {
    let output = LogBytes::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("the admission test owns this process's tracing subscriber");

    let (services, _) = common::services_resolving("93.184.216.34:80");
    let cell = Cell::with_snapshot_storage(
        CellConfig::for_test("snapshot-admission"),
        services,
        Arc::new(ExhaustedStorage),
    )
    .expect("cell startup must not require an instance reservation");
    let activator = Arc::new(FakeActivator::new(FakeBehaviour::done()));
    let module = activator.module().clone();
    cell.register_module(
        "acme",
        activator.clone(),
        common::manifest("capacity-refusal"),
    )
    .expect("fixture module must register");
    let record = DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(isoplane_abi::HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: Default::default(),
        cancel_future: 0,
        occupancy_budget_ms: 1_000,
        pending_futures_limit: 64,
        now_ms: 0,
    };
    let refused = cell
        .dispatch_blocking(
            &module,
            Target::Handler { route: "/".into() },
            record,
            Duration::from_secs(10),
        )
        .expect("reservation refusal must answer the caller");
    assert_eq!(
        refused.status, 503,
        "exhausted snapshot capacity must produce retryable admission refusal"
    );
    assert_eq!(
        refused.body, b"snapshot_capacity_unavailable",
        "capacity refusal must carry its stable admission diagnostic"
    );
    assert_eq!(
        activator.activation_count(),
        0,
        "the guest must not activate without reserved snapshot capacity"
    );
    assert!(
        cell.resident_ids().is_empty(),
        "failed reservation must not create a resident instance"
    );
    cell.shutdown();

    let bytes = output.0.lock().expect("test log lock").clone();
    let records: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .expect("JSON logs must be UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("each log line must be JSON")
        })
        .filter(|record| record["fields"]["event"] == "dispatch_completed")
        .collect();
    assert_eq!(
        records.len(),
        1,
        "one refused admission must emit one terminal completion"
    );
    assert_eq!(
        records[0]["fields"]["outcome"], "snapshot_capacity_unavailable",
        "the completion must name snapshot admission exhaustion"
    );
    assert_eq!(
        records[0]["fields"]["status"], 503,
        "the completion status must match the refused caller's response"
    );
}
