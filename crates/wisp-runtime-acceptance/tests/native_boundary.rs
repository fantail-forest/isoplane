#[macro_use]
mod common;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;

use common::GuestRuntime;
use wisp_engine::Completion;

async fn after_restore(runtime: GuestRuntime, path: &str) -> String {
    let reactor = no_egress::reactor_without_egress(5600);
    reactor.context.begin_slice(1, 1);
    let mut guest = common::guest(&reactor, runtime);
    let first = guest.start(path);
    assert_eq!(
        first.completion,
        Completion::Parked,
        "native boundary probe must reach the supported suspension boundary: {first:?}; response: {}",
        reactor.response_text(1)
    );
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    reactor.context.begin_slice(61, 1);
    let resumed = guest.wake(&reactor).await;
    assert_eq!(
        resumed.completion,
        Completion::Done,
        "native boundary probe must remain usable after Store restoration: {resumed:?}"
    );
    reactor.response_text(1)
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn a_missing_native_library_is_refused_before_and_after_store_restoration(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-library-after-park/missing").await;
        assert!(matches!(result.as_str(), "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "missing native library loading must be refused consistently: {result}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_loaded_assembly_cannot_invoke_an_unknown_native_library() {
    let result = after_restore(
        GuestRuntime::Mono,
        "/reflection/native-payload-after-park/library",
    )
    .await;
    assert_eq!(
        result, "DllNotFoundException/DllNotFoundException",
        "a dynamically loaded declaration cannot resolve an absent native library"
    );
}

#[tokio::test(start_paused = true)]
async fn a_loaded_assembly_cannot_invoke_an_unknown_prelinked_symbol() {
    let result = after_restore(
        GuestRuntime::Mono,
        "/reflection/native-payload-after-park/symbol",
    )
    .await;
    assert_eq!(
        result, "EntryPointNotFoundException/EntryPointNotFoundException",
        "an existing prelinked library must refuse an unknown entry point"
    );
}

#[tokio::test(start_paused = true)]
async fn a_loaded_assembly_can_call_a_prelinked_guest_function_without_host_native_loading() {
    let result = after_restore(
        GuestRuntime::Mono,
        "/reflection/native-payload-after-park/timestamp",
    )
    .await;
    assert_eq!(
        result, "prelinked-call-returned/prelinked-call-returned",
        "native dispatch may reach existing guest-Wasm functions across Store restoration"
    );
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn a_host_native_library_is_not_available_to_the_guest(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-library-after-park/host").await;
        assert!(matches!(result.as_str(), "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "host libc must not become available through guest native-library loading: {result}");
    }
}

#[tokio::test(start_paused = true)]
async fn byte_loading_an_impostor_cannot_replace_the_existing_core_library() {
    assert_eq!(
        after_restore(
            GuestRuntime::Mono,
            "/reflection/trusted-identity-after-park/corelib"
        )
        .await,
        "original/original",
        "core library identity must remain the runtime's original before and after restore"
    );
}

#[tokio::test(start_paused = true)]
async fn byte_loading_an_impostor_cannot_replace_a_trusted_bcl_default_binding() {
    assert_eq!(
        after_restore(
            GuestRuntime::Mono,
            "/reflection/trusted-identity-after-park/bcl"
        )
        .await,
        "original/original",
        "default BCL binding must remain the original before and after restore"
    );
}

#[tokio::test(start_paused = true)]
async fn byte_loading_an_impostor_cannot_replace_the_sdk_default_binding() {
    assert_eq!(
        after_restore(
            GuestRuntime::Mono,
            "/reflection/trusted-identity-after-park/sdk"
        )
        .await,
        "original/original",
        "default SDK binding must remain the original before and after restore"
    );
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn invoking_an_unknown_native_declaration_is_refused_before_and_after_store_restoration(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-declaration-after-park").await;
        assert!(matches!(result.as_str(), "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "a reached unsupported native declaration must be refused consistently: {result}");
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn a_static_declaration_for_an_absent_prelinked_symbol_is_refused_when_called(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-symbol-after-park/symbol").await;
        assert!(matches!(result.as_str(), "EntryPointNotFoundException/EntryPointNotFoundException" | "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "a missing prelinked entry point must be a managed refusal before and after Store restoration: {result}");
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn an_absent_internal_symbol_is_refused_when_called(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-symbol-after-park/internal").await;
        assert!(matches!(result.as_str(), "EntryPointNotFoundException/EntryPointNotFoundException" | "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "an internal declaration must not bind an absent symbol: {result}");
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn a_supported_static_native_export_remains_callable_across_store_restoration(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-symbol-after-park/timestamp").await;
        assert_eq!(result, "prelinked-call-returned/prelinked-call-returned", "available exports must retain strong archive extraction and correct binding");
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn an_absent_star_module_symbol_is_a_managed_refusal(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-symbol-after-park/star").await;
        assert!(matches!(result.as_str(), "EntryPointNotFoundException/EntryPointNotFoundException" | "DllNotFoundException/DllNotFoundException" | "PlatformNotSupportedException/PlatformNotSupportedException"), "a missing star-module symbol must refuse with a catchable managed exception rather than trap: {result}");
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn a_supported_star_module_export_remains_callable(runtime: GuestRuntime) {
        let result = after_restore(runtime, "/reflection/native-symbol-after-park/star-timestamp").await;
        assert_eq!(result, "prelinked-call-returned/prelinked-call-returned", "the star module must preserve direct binding to actual selected native exports");
    }
}
