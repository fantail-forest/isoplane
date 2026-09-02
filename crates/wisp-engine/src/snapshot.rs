//! The non-memory half of an instance's mutable state. Linear memory is
//! diffed by `wisp-hibernate`; globals and tables are small and captured whole.

use anyhow::bail;
use serde::{Deserialize, Serialize};
use wasmtime::Val;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstanceImage {
    /// Exported mutable globals, in module index order.
    pub globals: Vec<GlobalValue>,
    pub table_mutations: Vec<TableMutation>,
    /// Linear-memory size in WASM pages, so restore can grow to match before
    /// delta pages are applied.
    pub memory_pages: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum GlobalValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl GlobalValue {
    pub(crate) fn of(v: Val, store: impl wasmtime::AsContext) -> crate::Result<Self> {
        Ok(match v {
            Val::I32(x) => GlobalValue::I32(x),
            Val::I64(x) => GlobalValue::I64(x),
            Val::F32(x) => GlobalValue::F32(x),
            Val::F64(x) => GlobalValue::F64(x),
            other => bail!(
                "mutable global of type {:?} is not a numeric value and cannot be captured",
                other.ty(store)
            ),
        })
    }
}

impl From<GlobalValue> for Val {
    fn from(v: GlobalValue) -> Val {
        match v {
            GlobalValue::I32(x) => Val::I32(x),
            GlobalValue::I64(x) => Val::I64(x),
            GlobalValue::F32(x) => Val::F32(x),
            GlobalValue::F64(x) => Val::F64(x),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMutation {
    pub table: u32,
    pub index: u32,
    /// Function index, or `None` for a null slot.
    pub func: Option<u32>,
}
