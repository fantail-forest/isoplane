//! Reflection, runtime-created generics, serialization and hibernation on Mono.

#[macro_use]
mod common;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;
use isoplane_host::InstanceId;

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(owner));
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_mono_runtime! {
    /// A type named by a literal is kept by the trimmer and found; a
    /// framework instantiation is found by its qualified name.
    #[tokio::test]
    async fn a_type_the_trimmer_can_see_is_found_by_name(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 360, "/reflection/type-by-name/literal").await,
            "Acceptance.Sample instance=created",
            "Type.GetType over a literal name must find the type and Activator must construct it"
        );
        assert_eq!(
            answer(runtime, 361, "/reflection/type-by-name/framework").await,
            "System.Collections.Generic.List`1[[System.Int32, System.Private.CoreLib, Version=10.0.0.0, Culture=neutral, PublicKeyToken=7cec85d7bea7798e]] instance=created",
            "an assembly-qualified generic instantiation must round-trip through Type.GetType"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_runtime_named_type_remains_constructible(runtime: GuestRuntime) {
        let expected = "Acceptance.Orphan instance=created";
        assert_eq!(
            answer(runtime, 362, "/reflection/type-by-name/assembled").await,
            expected,
            "a runtime-named type must remain constructible in untrimmed Mono"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn make_generic_type_over_a_reference_type_instantiates_in_mono(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 363, "/reflection/make-generic/reference").await,
            "Holder<Sample> method=Sample",
            "MakeGenericType and MakeGenericMethod over a reference type must use the shared instantiation"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn make_generic_type_over_an_unseen_value_type_instantiates(runtime: GuestRuntime) {
        let answer = answer(runtime, 364, "/reflection/make-generic/value").await;
        assert!(
            answer == "Holder<UnseenPoint> method=UnseenPoint" || answer == "Holder<UnseenSize> method=UnseenSize",
            "the interpreter must instantiate a generic over a value type it has never seen: {answer}"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn activator_constructs_with_and_without_arguments_and_refuses_a_missing_constructor(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 365, "/reflection/activator").await,
            "default:0:|named:3:|default:0:|MissingMethodException",
            "Activator must use the parameterless and the matching constructor and refuse a signature the type lacks"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn attributes_properties_and_methods_are_enumerable_over_a_kept_type(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 366, "/reflection/attributes").await,
            "class=/sample methods=Greet@/greet properties=Count:Int32,Name:String,Tags:List`1 set=9",
            "custom attributes on the class and its methods, its public properties and a reflective set must all be visible"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn method_info_invokes_binds_delegates_and_wraps_a_wrong_argument(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 367, "/reflection/invoke").await,
            "inv greets isoplane|inv greets delegate|5|ctor:2:|ArgumentException",
            "Invoke, CreateDelegate, a generic method closed by reflection and a constructor must work, and a wrong argument type must be refused"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn generic_virtual_methods_dispatch_over_reference_and_value_arguments(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 368, "/reflection/generic-virtual").await,
            "circle<Int32>3/circle<String>s/circle<UnseenPoint>UnseenPoint { X = 1, Y = 2 }|square<Int32>3/square<String>s/square<UnseenPoint>UnseenPoint { X = 1, Y = 2 }|Circle visits 2.5",
            "a generic virtual method must dispatch to the override for every statically visible instantiation, and an interface method through the base"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn interface_and_delegate_dispatch_including_dynamic_invoke(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 369, "/reflection/dispatch").await,
            "woof,meow|11,2,5|26|42|ArgumentException",
            "interface dispatch, closures, method groups, composed delegates and DynamicInvoke must work, and a wrong argument must be refused"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn enums_parse_format_flags_and_refuse_an_unknown_name(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 370, "/reflection/enum").await,
            "Friday|Friday|None,Read,Write,Execute|Read, Execute|Read, Write|False|8|ArgumentException|0,1,2,3,4,5,6",
            "Enum parsing, name enumeration, flags formatting and an undefined value must all behave as on the desktop runtime"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn reflection_and_generated_json_both_round_trip(runtime: GuestRuntime) {
        let expected = r#"{"Name":"json","Count":4,"Tags":["a","b"]} json:4:a+b"#;
        assert_eq!(
            answer(runtime, 371, "/reflection/json/reflection").await,
            expected,
            "default JSON reflection must round-trip in untrimmed Mono"
        );
        assert_eq!(
            answer(runtime, 372, "/reflection/json/source-generated").await,
            r#"{"name":"json","count":4,"tags":["a","b"]} json:4:a+b"#,
            "the source-generated context must serialize and deserialize the sample"
        );
        assert_eq!(
            answer(runtime, 373, "/reflection/json/source-generated-options").await,
            r#"{"Name":"json","Count":4,"Tags":["a","b"]} json:4:a+b"#,
            "fresh options carrying the context as their resolver must serialize through it, without the naming policy the context's attribute gives only its own default options"
        );
        assert_eq!(
            answer(runtime, 374, "/reflection/json/document").await,
            "doc 2 1",
            "JsonDocument must parse and navigate without any serializer"
        );
    }
}

async fn answer_after_delta(runtime: GuestRuntime, owner: InstanceId, path: &str) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(owner.0));
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let outcome = guest.start(path);
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "runtime state must park before capture: {outcome:?}"
    );
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    reactor.context.begin_slice(61, 1);
    let outcome = guest.wake_through_delta(&reactor).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "runtime state must remain usable after restoring its delta: {outcome:?}"
    );
    reactor.response_text(1)
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn interpreted_expression_closures_remain_callable_after_hibernation(runtime: GuestRuntime) {
        assert_eq!(answer_after_delta(runtime, InstanceId(380), "/reflection/expression-after-park").await,
            "42/43", "expression delegates must preserve their captured mutable state across hibernation");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn disposing_a_safe_handle_twice_releases_it_once(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 381, "/reflection/safe-handle-dispose").await,
            "1", "SafeHandle must release its owned resource exactly once");
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn a_finalized_safe_handle_is_not_released_again_after_hibernation(runtime: GuestRuntime) {
        assert_eq!(answer_after_delta(runtime, InstanceId(382), "/reflection/safe-handle-finalizer-after-park").await,
            "1/1", "finalization must release an unreachable SafeHandle once and preserve that state across hibernation");
    }
}

#[tokio::test(start_paused = true)]
async fn an_interpreted_emitted_delegate_remains_callable_after_hibernation() {
    assert_eq!(
        answer_after_delta(
            GuestRuntime::Mono,
            InstanceId(383),
            "/reflection/emitted-delegate-after-park"
        )
        .await,
        "42/43",
        "Mono must preserve emitted IL and its callable delegate across hibernation"
    );
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn default_expression_compilation_remains_callable_after_hibernation(runtime: GuestRuntime) {
        assert_eq!(answer_after_delta(runtime, InstanceId(384), "/reflection/expression-default-after-park").await,
            "42/43", "ordinary expression compilation must choose an available execution path and preserve captured state across hibernation");
    }
}

#[tokio::test(start_paused = true)]
async fn an_interpreted_loaded_assembly_remains_callable_after_hibernation() {
    assert_eq!(
        answer_after_delta(
            GuestRuntime::Mono,
            InstanceId(390),
            "/reflection/assembly-after-park"
        )
        .await,
        "42/43",
        "Mono must preserve a loaded assembly and its reflection metadata across hibernation"
    );
}

#[tokio::test(start_paused = true)]
async fn interpreted_dynamic_members_remain_callable_after_hibernation() {
    assert_eq!(
        answer_after_delta(
            GuestRuntime::Mono,
            InstanceId(392),
            "/reflection/dynamic-after-park/member"
        )
        .await,
        "42/43",
        "a preserved user method must remain dynamically callable across hibernation"
    );
}

#[tokio::test(start_paused = true)]
async fn interpreted_dynamic_expando_bindings_survive_hibernation() {
    assert_eq!(
        answer_after_delta(
            GuestRuntime::Mono,
            InstanceId(393),
            "/reflection/dynamic-after-park/expando"
        )
        .await,
        "42/43",
        "ExpandoObject dynamic binding rules must preserve mutable state across hibernation"
    );
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn enabled_event_listeners_survive_hibernation_and_stop_at_disposal(runtime: GuestRuntime) {
        assert_eq!(answer_after_delta(runtime, InstanceId(396), "/reflection/events-after-park").await,
            "42,43", "an enabled listener must preserve payloads across hibernation and receive nothing after disposal");
    }
}
