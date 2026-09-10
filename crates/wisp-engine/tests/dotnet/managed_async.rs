//! Managed thread-pool continuations across a destroyed Wasmtime Store.

use crate::dotnet::{budget, fixture, GuestRuntime, Request};
use wisp_abi::result::ResultRecord;
use wisp_engine::{Completion, InstanceOps};

in_both_runtimes! {
    fn an_uncaptured_managed_continuation_resumes_on_another_worker_after_store_destruction(runtime: GuestRuntime) {
        let fixture = fixture("wisp_guest_dotnet_async", runtime);
        let (mut instance, _) = fixture.activate();
        let outcome = Request::get("/async").dispatch(&mut instance);
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "managed thread-pool work must reach the host future before parking: {outcome:?}"
        );
        let image = instance
            .capture_mutable_state()
            .expect("a returned guest must be capturable");
        let memory = instance.memory().to_vec();
        drop(instance);

        let completion = std::thread::spawn(move || {
            let (mut restored, _) = fixture.activate();
            restored
                .restore_mutable_state(&image)
                .expect("the guest globals must restore");
            restored.memory().copy_from_slice(&memory);
            restored
                .wake(1, &ResultRecord::Ok(vec![0]), budget())
                .completion
        })
        .join()
        .expect("the worker must not panic");
        assert_eq!(
            completion,
            Completion::Done,
            "ConfigureAwait(false) and subsequent thread-pool work must finish after Store destruction"
        );
    }
}
