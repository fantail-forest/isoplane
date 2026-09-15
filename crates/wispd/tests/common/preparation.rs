use wisp_abi::config::{AppManifest, InitSandbox};
use wispd::real::{Deployed, RealCells};

pub(crate) trait LocalDeployment {
    async fn deploy_locally(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        manifest: AppManifest,
        wasm: &[u8],
        sandbox: InitSandbox,
    ) -> anyhow::Result<Deployed>;
}

impl LocalDeployment for RealCells {
    async fn deploy_locally(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        mut manifest: AppManifest,
        wasm: &[u8],
        sandbox: InitSandbox,
    ) -> anyhow::Result<Deployed> {
        let provider = self.local_preparation()?;
        let cancellation = tokio_util::sync::CancellationToken::new();
        let context = wispd::deploy::PreparationContext {
            runtime_configuration: Default::default(),
            application_files: wisp_engine::PlatformFiles::default(),
            engine: self.engine(),
            provider: &provider,
            runtime: wisp_prepare::RuntimeProfile::RawWasm,
            cpu_ms: wisp_prepare::preinit::DEFAULT_INITIALIZATION_CPU_MS,
            wall_ms: wisp_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: wisp_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: wisp_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
            storage: wisp_engine::prepared::PreparedStorage::VerifiedCopy,
            cancellation: &cancellation,
        };
        manifest.name = app.to_string();
        self.deploy_prepared_version(
            manifest,
            wasm,
            wisp_control::DeployOptions {
                version: wisp_control::VersionId(version.to_string()),
                tenant: tenant.to_string(),
                init_sandbox: sandbox,
            },
            &context,
        )
        .await
    }
}
