use isoplane_engine::{PlatformFile, PlatformFiles};
use isoplane_harness::preparation::shared_runtime::{self, Application};
use isoplane_prepare::PrepareError;

pub fn files(name: &str) -> PlatformFiles {
    PlatformFiles::new(
        isoplane_fixtures::managed_probe(name)
            .into_iter()
            .map(|file| PlatformFile {
                path: file.path,
                bytes: file.bytes.into(),
                modified_ns: 0,
            })
            .collect(),
    )
    .expect("valid managed application files")
}

pub fn prepare(name: &str, platform_files: PlatformFiles) -> Result<Application, PrepareError> {
    prepare_configured(name, platform_files, b"{}", None)
}

/// Prepares the probe with `runtime_config` as the JSON object of settings its
/// initialization reads, and `network` as what that initialization may reach.
pub fn prepare_configured(
    name: &str,
    platform_files: PlatformFiles,
    runtime_config: &[u8],
    network: Option<isoplane_prepare::InitializationNetwork>,
) -> Result<Application, PrepareError> {
    let mut request = shared_runtime::request(files(name), platform_files);
    request.initialization.network = network;
    request.initialization.runtime_config = runtime_config.to_vec();
    shared_runtime::prepare(&request)
}
