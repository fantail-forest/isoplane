//! Guest memory access for the initializer's Wasmtime version.

use anyhow::Result;
use wisp_engine::pal::filesystem::Memory;
use wizer::wasmtime;
pub(super) struct CallerMemory<'a, 'b, T: 'static>(pub &'a mut wasmtime::Caller<'b, T>);

impl<T: 'static> CallerMemory<'_, '_, T> {
    fn memory(&mut self) -> Result<wasmtime::Memory> {
        self.0
            .get_export("memory")
            .and_then(|export| export.into_memory())
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no exported memory"))
    }
}

impl<T: 'static> Memory for CallerMemory<'_, '_, T> {
    fn read(&mut self, pointer: u32, bytes: &mut [u8]) -> Result<()> {
        self.memory()?.read(&*self.0, pointer as usize, bytes)?;
        Ok(())
    }
    fn write(&mut self, pointer: u32, bytes: &[u8]) -> Result<()> {
        self.memory()?
            .write(&mut *self.0, pointer as usize, bytes)?;
        Ok(())
    }
    fn allocate(&mut self, size_bytes: u32) -> Result<u32> {
        if let Some(function) = self
            .0
            .get_export(wisp_abi::exports::FS_ALLOC)
            .and_then(|export| export.into_func())
        {
            return Ok(function
                .typed::<i32, i32>(&*self.0)?
                .call(&mut *self.0, size_bytes as i32)? as u32);
        }
        let function = self
            .0
            .get_export("cabi_realloc")
            .and_then(|export| export.into_func())
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no allocator export"))?;
        Ok(function
            .typed::<(i32, i32, i32, i32), i32>(&*self.0)?
            .call(&mut *self.0, (0, 0, 4, size_bytes as i32))? as u32)
    }
    fn free(&mut self, pointer: u32) -> Result<()> {
        if let Some(function) = self
            .0
            .get_export(wisp_abi::exports::FS_FREE)
            .and_then(|export| export.into_func())
        {
            function
                .typed::<i32, ()>(&*self.0)?
                .call(&mut *self.0, pointer as i32)?;
            return Ok(());
        }
        let function = self
            .0
            .get_export("cabi_realloc")
            .and_then(|export| export.into_func())
            .ok_or_else(|| anyhow::anyhow!("filesystem guest has no allocator export"))?;
        function
            .typed::<(i32, i32, i32, i32), i32>(&*self.0)?
            .call(&mut *self.0, (pointer as i32, 4, 4, 0))?;
        Ok(())
    }
}
