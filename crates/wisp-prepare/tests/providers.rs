mod common;

use tokio_util::sync::CancellationToken;
use wisp_prepare::{BuildRequest, LocalPreparation, Preparation, RuntimeProfile};

async fn preserves_application_state(provider: &dyn Preparation) {
    let wasm = common::guest("", "(global.set $state (i32.const 42))");
    let engine = common::engine();
    let request = BuildRequest {
        engine: engine.configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        limits: wisp_abi::config::Limits::default(),
        initialization: common::request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let product = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("the selected provider must satisfy the shared preparation contract");
    assert_eq!(
        product.initialized.seed_reads, 0,
        "this representation comparison uses no random inputs"
    );
    assert_eq!(
        product.initialized.passes, 1,
        "the provider must run one successful initialization"
    );
    let template = engine
        .load_template(&product.initialized.wasm)
        .expect("initialized portable result");
    assert!(
        !template.has_initialize(),
        "the provider must remove initialization from its output"
    );
    assert!(
        !product.prepared_bytes().is_empty(),
        "the provider must return native and explicit-state transport"
    );
}

#[tokio::test]
async fn local_preparation_satisfies_the_shared_provider_contract() {
    preserves_application_state(&LocalPreparation::new().expect("portable provider")).await;
}
