//! Runtime configuration is available only to the registered module owner.

use std::sync::Arc;

use wisp_abi::config::{HandlerLifecycle, Limits};

use wisp_host::cell::{Cell, CellConfig};
use wisp_host::fakes::*;
use wisp_host::ModuleId;

mod common;
use common::manifest;

struct Rig {
    cell: Arc<Cell>,
}

fn rig(cfg: CellConfig) -> Rig {
    let (cell, _fetcher) = common::rig_resolving(cfg, "93.184.216.34:80");
    Rig { cell }
}
