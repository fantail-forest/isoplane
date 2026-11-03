//! The shipped .NET guest through pre-initialization: the template answers
//! its first call with the managed runtime already up and no init entry left
//! to run.

use isoplane_abi::config::GuestMemoryBytes;
use isoplane_control::validator;
#[path = "common/managed.rs"]
mod managed;

#[tokio::test]
async fn the_dotnet_template_activates_without_running_the_runtime_start_up() {
    let wasm = isoplane_fixtures::mono_runtime();
    validator::validate_with_memory_limit(
        &wasm,
        isoplane_abi::config::GuestMemoryBytes::default(),
        Some(GuestMemoryBytes::WASM32_MAX),
    )
    .expect(".NET guest must validate");

    let out = managed::prepare("isoplane_guest_dotnet_bcl", Default::default())
        .expect("pre-init must succeed on the managed application");
    assert!(
        !out.product.initialized.pal_calls.is_empty(),
        "the runtime reaches the PAL while coming up; nothing reached means no runtime start-up \
         was captured"
    );

    let artifact = out.artifact();
    let engine = isoplane_engine::IsoplaneEngine::new(isoplane_fixtures::engine_config(2)).unwrap();
    let template = engine
        .load_prepared_product(&artifact)
        .expect("prepared template");
    assert!(
        !template.has_initialize(),
        "prepared activation must have no initializer to rerun"
    );
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(1));
    let mut guest = isoplane_harness::ReturnedGuest::prepared(artifact, &reactor);
    assert_eq!(guest.answer(&reactor, "/trust/at-boot", 4).await, "roots=0 ",
        "the first dispatch must observe application state already captured with an empty trust bundle");
}

#[test]
fn a_failed_managed_application_initializer_refuses_pre_initialization() {
    let result = managed::prepare("isoplane_guest_dotnet_failing_boot", Default::default());
    let Err(error) = result else {
        panic!("a failed application initializer must fail deployment");
    };
    assert_eq!(
        error.stage,
        isoplane_prepare::BuildStage::Initialization,
        "the deployment must fail while initializing the application"
    );
}
