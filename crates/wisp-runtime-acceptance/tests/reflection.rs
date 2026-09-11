//! Reflection, generics and serialization where the interpreter and the
//! ahead-of-time compiler can differ. Both profiles are trimmed, so what
//! the trimmer cannot see is gone in both; an instantiation the compiler
//! never generated exists only where code can be created at run time, and
//! that is the one behaviour whose expected outcome is a function of the
//! runtime under test, stated in its assertion.

#[macro_use]
mod common;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;

use common::{guest, GuestRuntime};
use no_egress::reactor_without_egress;

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    let reactor = reactor_without_egress(owner);
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_both_runtimes! {
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

in_both_runtimes! {
    /// A type nothing references, named by a string assembled at run
    /// time, is trimmed away in both profiles: `Type.GetType` answers null,
    /// never a type that exists only in the source.
    #[tokio::test]
    async fn a_type_only_a_runtime_string_names_is_trimmed_in_both_profiles(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 362, "/reflection/type-by-name/assembled").await,
            "null",
            "a type reachable only through an assembled name must have been trimmed"
        );
    }
}

in_both_runtimes! {
    /// A closed generic over a reference type shares the canonical code
    /// both compilers already have.
    #[tokio::test]
    async fn make_generic_type_over_a_reference_type_instantiates_in_both_profiles(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 363, "/reflection/make-generic/reference").await,
            "Holder<Sample> method=Sample",
            "MakeGenericType and MakeGenericMethod over a reference type must use the shared instantiation"
        );
    }
}

in_both_runtimes! {
    /// A closed generic over a value type the program never instantiates
    /// needs code the ahead-of-time compiler did not generate: the
    /// interpreter builds it at run time, NativeAOT refuses it.
    #[tokio::test]
    async fn make_generic_type_over_an_unseen_value_type_depends_on_the_runtime(runtime: GuestRuntime) {
        let answer = answer(runtime, 364, "/reflection/make-generic/value").await;
        match runtime {
            GuestRuntime::Mono => assert!(
                answer == "Holder<UnseenPoint> method=UnseenPoint" || answer == "Holder<UnseenSize> method=UnseenSize",
                "the interpreter must instantiate a generic over a value type it has never seen: {answer}"
            ),
            GuestRuntime::NativeAot => assert_eq!(
                answer,
                "NotSupportedException",
                "NativeAOT has no code for a value-type instantiation it never generated and must refuse it, never answer wrongly"
            ),
        }
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn activator_constructs_with_and_without_arguments_and_refuses_a_missing_constructor(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 365, "/reflection/activator").await,
            "default:0:|named:3:|default:0:|MissingMethodException",
            "Activator must use the parameterless and the matching constructor and refuse a signature the type lacks"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn attributes_properties_and_methods_are_enumerable_over_a_kept_type(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 366, "/reflection/attributes").await,
            "class=/sample methods=Greet@/greet properties=Count:Int32,Name:String,Tags:List`1 set=9",
            "custom attributes on the class and its methods, its public properties and a reflective set must all be visible"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn method_info_invokes_binds_delegates_and_wraps_a_wrong_argument(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 367, "/reflection/invoke").await,
            "inv greets wisp|inv greets delegate|5|ctor:2:|ArgumentException",
            "Invoke, CreateDelegate, a generic method closed by reflection and a constructor must work, and a wrong argument type must be refused"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn generic_virtual_methods_dispatch_over_reference_and_value_arguments(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 368, "/reflection/generic-virtual").await,
            "circle<Int32>3/circle<String>s/circle<UnseenPoint>UnseenPoint { X = 1, Y = 2 }|square<Int32>3/square<String>s/square<UnseenPoint>UnseenPoint { X = 1, Y = 2 }|Circle visits 2.5",
            "a generic virtual method must dispatch to the override for every statically visible instantiation, and an interface method through the base"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn interface_and_delegate_dispatch_including_dynamic_invoke(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 369, "/reflection/dispatch").await,
            "woof,meow|11,2,5|26|42|ArgumentException",
            "interface dispatch, closures, method groups, composed delegates and DynamicInvoke must work, and a wrong argument must be refused"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn enums_parse_format_flags_and_refuse_an_unknown_name(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 370, "/reflection/enum").await,
            "Friday|Friday|None,Read,Write,Execute|Read, Execute|Read, Write|False|8|ArgumentException|0,1,2,3,4,5,6",
            "Enum parsing, name enumeration, flags formatting and an undefined value must all behave as on the desktop runtime"
        );
    }
}

in_both_runtimes! {
    /// Both profiles publish trimmed, and trimming disables reflection-based
    /// `System.Text.Json` by default: the reflection serializer refuses,
    /// the source-generated context serializes.
    #[tokio::test]
    async fn reflection_based_json_is_disabled_and_source_generated_json_works(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 371, "/reflection/json/reflection").await,
            "InvalidOperationException",
            "the reflection-based serializer must be refused under trimming rather than serialize a partial shape"
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
