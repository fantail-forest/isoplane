#[test]
fn preparation_uses_the_application_owner_and_execution_network_budgets() {
    let mut config = crate::config::NodeConfig::new("test-owner");
    config.network.instance_pending_connections = 1;
    config.network.instance_open_connections = 2;
    config.host.guest_resources.native_handles = 7;
    config.host.buffered_future_bytes = 4096;
    let manifest: isoplane_abi::config::AppManifest = toml::from_str(
            "name='app'\n[bindings]\nfetch_allowlist=['db.example:5432']\n[resource_limits]\npending_futures_per_dispatch=2",
        ).expect("application policy");
    let endpoint = "127.0.0.1:5432".parse().expect("local database endpoint");
    let policy = super::initialization_network(
        "owner",
        &manifest,
        vec![endpoint],
        config.network,
        &config.host,
    );
    assert_eq!(
        policy.tenant, "owner",
        "initialization must retain the established owner"
    );
    assert_eq!(
        policy.allowlist, manifest.bindings.fetch_allowlist,
        "application egress restrictions must also apply before capture"
    );
    assert_eq!(
        policy.destinations,
        isoplane_prepare::InitializationDestinations::Public {
            exceptions: vec![endpoint]
        },
        "the explicit local database allowance must reach preparation"
    );
    assert_eq!(
        (
            policy.limits.instance_pending_connections,
            policy.limits.instance_open_connections
        ),
        (1, 2),
        "initialization must obey the host connection limits"
    );
    assert_eq!(
        policy.native_handles, 7,
        "initialization must obey the host native resource limit"
    );
    assert_eq!(
        policy.buffered_bytes, 4096,
        "initialization must obey the host buffering limit"
    );
    assert_eq!(
        policy.pending_futures, 2,
        "initialization must obey application resource overrides"
    );
}

#[test]
fn operator_profiling_configuration_reaches_the_engine() {
    let mut config = crate::config::NodeConfig::new("test-owner");
    let trust = config.trust_bundle().expect("the bundled roots must load");
    assert!(
        !super::engine_config(&config, &trust)
            .expect("valid engine configuration")
            .perf_jitdump,
        "normal node operation must not write profiling artifacts"
    );
    config.engine = toml::from_str("perf_jitdump=true")
        .expect("operator profiling configuration must deserialize");
    assert!(
        super::engine_config(&config, &trust)
            .expect("valid engine configuration")
            .perf_jitdump,
        "an operator must be able to enable native guest symbols"
    );
}

#[test]
fn configured_engine_resource_limits_reach_the_execution_engine() {
    let mut config = crate::config::NodeConfig::new("test-owner");
    config.engine = toml::from_str(
            "max_core_instance_size_bytes=8388608\ntable_elements=262144\nmax_tables_per_module=8\ncompiler_stack_bytes=16777216\nmax_wasm_stack_bytes=1048576\ntotal_tables=20",
        ).expect("operator engine limits must deserialize");
    let trust = config.trust_bundle().expect("the bundled roots must load");
    let engine = super::engine_config(&config, &trust).expect("platform files must be valid");
    assert_eq!(
        engine.total_tables,
        Some(20),
        "aggregate table slots must reach the engine"
    );
    assert_eq!(
        engine.max_wasm_stack_bytes,
        1 << 20,
        "guest stack limit must reach the engine"
    );
    assert_eq!(
        engine.max_core_instance_size_bytes,
        8 << 20,
        "node metadata limit must reach the engine"
    );
    assert_eq!(
        engine.table_elements, 262144,
        "node table size must reach the engine"
    );
    assert_eq!(
        engine.max_tables_per_module, 8,
        "node table count must reach the engine"
    );
    assert_eq!(
        engine.compiler_stack_bytes,
        16 << 20,
        "node compiler stack must reach the engine"
    );
}

impl super::RealCells {
    pub(crate) async fn install_prepared(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        manifest: isoplane_abi::config::AppManifest,
        template: isoplane_engine::Template,
    ) -> anyhow::Result<isoplane_host::ModuleId> {
        self.ensure_configured(app)?;
        self.install_prepared_on_cell(
            isoplane_host::ModuleId::new(app, version),
            tenant,
            manifest,
            template,
            None,
            None,
        )
        .await
    }
}
