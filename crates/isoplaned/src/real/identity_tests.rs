use super::RealCells;

async fn refuses_incomplete_metadata(remove: impl FnOnce(&RealCells)) {
    let directory = tempfile::tempdir().expect("a unique runtime directory");
    let mut config = crate::config::NodeConfig::dev_default();
    config.data_dir = directory.path().to_path_buf();
    config.engine.max_instances = 2;
    config.engine.max_memory_mib = 16;
    config.engine.guard_mib = 16;
    let node = crate::node::Node::build(config).expect("the local runtime must assemble");
    let real = node.real.as_ref().expect("the node must use real cells");
    real.modules
        .write()
        .insert(isoplane_host::ModuleId::new("quotes", "v1"));
    let id = isoplane_abi::host::DiId {
        app: "quotes".into(),
        class: "Room".into(),
        name: "one".into(),
    };
    assert!(
        real.target_for(&id).is_some(),
        "complete registered metadata must produce a target"
    );
    remove(real);
    assert!(
        real.target_for(&id).is_none(),
        "a target with missing ownership or execution settings must be refused"
    );
    tokio::task::spawn_blocking(move || drop(node))
        .await
        .expect("runtime workers must shut down cleanly");
}

#[tokio::test]
async fn a_registered_module_without_its_tenant_has_no_target() {
    refuses_incomplete_metadata(|real| {
        real.tenants.write().remove("quotes");
    })
    .await;
}

#[tokio::test]
async fn a_registered_module_without_its_execution_settings_has_no_target() {
    refuses_incomplete_metadata(|real| {
        real.app_config.write().remove("quotes");
    })
    .await;
}
