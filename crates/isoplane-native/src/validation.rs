use isoplane_abi::config::{GuestMemoryBytes, Limits, ValidationLimits};
use isoplane_runtime::inspection;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("owned process operation failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("local session failed: {0:#}")]
    Session(#[source] anyhow::Error),
    #[error("native input buffer is null or exceeds addressable memory")]
    Buffer,
    #[error("invalid validation policy: {0}")]
    Policy(#[from] serde_json::Error),
    #[error("cannot inspect module: {0}")]
    Module(#[from] wasmparser::BinaryReaderError),
    #[error("the runtime cache directory must be an absolute UTF-8 path")]
    CacheDirectory,
    #[error("runtime cache operation failed: {0}")]
    Cache(#[from] isoplane_cache::Error),
    #[error("preparing the shared runtime failed: {0:#}")]
    RuntimePreparation(#[source] anyhow::Error),
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    limits: Limits,
    guest_memory_bytes: Option<GuestMemoryBytes>,
    validation_limits: ValidationLimits,
}

pub(crate) fn inspect(wasm: &[u8], policy: &[u8]) -> Result<Vec<u8>, Error> {
    let policy: Policy = serde_json::from_slice(policy)?;
    let report = inspection::analyse(wasm)?;
    let verdict = isoplane_runtime::validate_with_limits(
        wasm,
        policy.limits,
        policy.guest_memory_bytes,
        &policy.validation_limits,
    );
    let violations = verdict.as_ref().err().map(Vec::as_slice).unwrap_or(&[]);
    let result = serde_json::json!({
        "bytes": report.bytes,
        "blake3": report.blake3,
        "limits": policy.limits,
        "guest_memory_bytes": policy.guest_memory_bytes.map_or(policy.limits.instance_memory_max, |bytes| bytes.get()),
        "validator": true,
        "rejected": verdict.is_err(),
        "violations": violations.iter().map(|violation| serde_json::json!({
            "code": format!("{:?}", violation.code),
            "subject": violation.subject,
            "detail": violation.detail,
        })).collect::<Vec<_>>(),
        "capabilities": report.declared.iter().map(|import| serde_json::json!({
            "name": import.name,
            "group": inspection::group_name(import.group),
            "kind": inspection::kind_label(import.kind),
            "note": inspection::kind_note(import),
            "sensitive": import.sensitive,
            "init_visible": import.init_visible,
        })).collect::<Vec<_>>(),
        "undeclared": report.undeclared.iter().map(|import| format!("{}.{}", import.module, import.field)).collect::<Vec<_>>(),
        "pal": report.pal.iter().map(|entry| serde_json::json!({
            "name": entry.full_name(),
            "delegates_to": entry.disposition.capability(),
            "traps": !entry.disposition.is_delegated(),
        })).collect::<Vec<_>>(),
        "exports": report.exports,
        "required_exports": isoplane_abi::imports::exports::REQUIRED,
        "optional_exports": isoplane_abi::imports::exports::OPTIONAL,
        "missing_required_exports": report.missing_required_exports,
        "missing_optional_exports": report.missing_optional_exports,
        "effects": report.plain_english(),
    });
    Ok(serde_json::to_vec(&result)?)
}
