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

#[test]
fn runtime_configuration_requires_the_registered_module_owner() {
    use wisp_host::bindings::{RuntimeConfiguration, RuntimeConfigurationError};
    let rig = rig(CellConfig::for_test("configuration"));
    let module = ModuleId::new("app", "v1");
    assert!(
        matches!(
            rig.cell
                .set_runtime_configuration("acme", &module, RuntimeConfiguration::default()),
            Err(RuntimeConfigurationError::UnknownModule)
        ),
        "configuration must not create an unregistered module"
    );
    rig.cell
        .register_module(
            "acme",
            Arc::new(FakeActivator::named(module.clone(), FakeBehaviour::done())),
            manifest("app", HandlerLifecycle::Reuse, Limits::default()),
        )
        .expect("register module");
    assert!(
        matches!(
            rig.cell
                .set_runtime_configuration("another", &module, RuntimeConfiguration::default()),
            Err(RuntimeConfigurationError::WrongTenant)
        ),
        "a tenant cannot configure another tenant's module"
    );
    assert!(
        rig.cell
            .set_runtime_configuration("acme", &module, RuntimeConfiguration::default())
            .is_ok(),
        "the registered owner can set its configuration"
    );
    rig.cell.shutdown();
}
