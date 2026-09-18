//! Structural resource policy is independent of capability validation.
use isoplane_abi::config::{Limits, ValidationLimits};
use isoplane_runtime::validator::{validate_with_limits, ViolationCode};

fn resource_refused(wat: &str, limits: &ValidationLimits) -> bool {
    let wasm = wat::parse_str(wat).expect("the structural fixture must assemble");
    validate_with_limits(&wasm, Limits::default(), None, limits)
        .err()
        .is_some_and(|errors| {
            errors
                .iter()
                .any(|error| error.code == ViolationCode::V4ResourceBomb)
        })
}

macro_rules! boundary {
    ($name:ident, $field:ident, $wat:expr, $used:expr) => {
        #[test]
        fn $name() {
            let mut limits = ValidationLimits {
                $field: $used,
                ..Default::default()
            };
            assert!(
                !resource_refused($wat, &limits),
                "the configured structural boundary must be admitted"
            );
            limits.$field = $used - 1;
            assert!(
                resource_refused($wat, &limits),
                "lowering the configured structural boundary must refuse the same module"
            );
        }
    };
}

boundary!(
    function_limit_is_applied,
    max_functions,
    "(module (func))",
    1
);
boundary!(
    locals_limit_is_applied,
    max_locals_per_function,
    "(module (func (local i32)))",
    1
);
boundary!(
    table_elements_limit_is_applied,
    max_table_elements,
    "(module (table 2 funcref))",
    2
);
boundary!(
    imports_limit_is_applied,
    max_imports,
    "(module (import \"unknown\" \"fn\" (func)))",
    1
);
boundary!(
    exports_limit_is_applied,
    max_exports,
    "(module (func (export \"fn\")))",
    1
);
boundary!(
    types_limit_is_applied,
    max_types,
    "(module (type (func)))",
    1
);
boundary!(
    element_segments_limit_is_applied,
    max_element_segments,
    "(module (elem funcref))",
    1
);
boundary!(
    data_segments_limit_is_applied,
    max_data_segments,
    "(module (data \"a\"))",
    1
);
boundary!(
    custom_bytes_limit_is_applied,
    max_custom_section_bytes,
    "(module (@custom \"extra\" \"abc\"))",
    3
);
