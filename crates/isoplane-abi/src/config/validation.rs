//! Operator-selected structural admission limits.

/// Resource policy applied before native compilation; sandbox capabilities stay fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ValidationLimits {
    /// Functions, including imported functions.
    pub max_functions: u32,
    /// Locals declared by a single function.
    pub max_locals_per_function: u32,
    /// Total declared table elements.
    pub max_table_elements: u32,
    /// Combined custom-section bytes.
    pub max_custom_section_bytes: u64,
    /// Imported entries.
    pub max_imports: u32,
    /// Exported entries.
    pub max_exports: u32,
    /// Declared types.
    pub max_types: u32,
    /// Element segments.
    pub max_element_segments: u32,
    /// Data segments.
    pub max_data_segments: u32,
}

impl ValidationLimits {
    /// Default structural resource policy.
    pub const DEFAULT: Self = Self {
        max_functions: 200_000,
        max_locals_per_function: 50_000,
        max_table_elements: 1_000_000,
        max_custom_section_bytes: 16 << 20,
        max_imports: 512,
        max_exports: 512,
        max_types: 100_000,
        max_element_segments: 100_000,
        max_data_segments: 100_000,
    };
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Default metadata bytes in a pooled core-instance slot.
pub const DEFAULT_CORE_INSTANCE_SIZE_BYTES: usize = 4 << 20;
/// Default elements in each pooled function table.
pub const DEFAULT_TABLE_ELEMENTS: usize = 131_072;
/// Default function tables permitted in a pooled module.
pub const DEFAULT_TABLES_PER_MODULE: u32 = 4;

/// Default native stack reserved per compiler worker.
pub const DEFAULT_COMPILER_STACK_BYTES: usize = 8 << 20;

/// Default native stack allowed for a guest invocation.
pub const DEFAULT_WASM_STACK_BYTES: usize = 512 << 10;
