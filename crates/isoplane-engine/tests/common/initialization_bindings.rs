use isoplane_engine::{HostBindings, ImportCall};

use super::TestBindings;

/// [`TestBindings`] answering the initialization sandbox's imports.
pub struct InitializationTestBindings(pub TestBindings);

impl isoplane_engine::bindings::InitializationBindings for InitializationTestBindings {
    fn begin_slice(&mut self) {
        self.0.begin_slice();
    }

    fn slice_clock_ms(&self) -> u64 {
        self.0.slice_clock_ms()
    }

    fn memory_limit_bytes(&self) -> Option<u64> {
        self.0.memory_limit_bytes
    }

    fn call_sync(
        &mut self,
        call: ImportCall<'_>,
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        Ok(self.0.call_sync(call))
    }

    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        out: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        self.0.call_fill(call, out)
    }

    fn runtime_bringup(&self) -> bool {
        false
    }
}
