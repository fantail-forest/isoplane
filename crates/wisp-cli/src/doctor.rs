//! The capability manifest `wisp doctor` prints: a module's imports read
//! against `wisp_abi::imports` and `wisp_abi::pal`, with the policy bits that
//! decide what each capability costs. Presentation only; the validator in
//! `wisp-control` decides whether the module deploys.

use std::collections::BTreeMap;

use wisp_abi::imports::{self, Group, ImportDef, ImportKind};
use wisp_abi::pal::{self, PalDef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImport {
    pub module: String,
    pub field: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub bytes: usize,
    pub blake3: String,
    /// Imports in the capability table, in table order.
    pub declared: Vec<&'static ImportDef>,
    /// Imports in the PAL whitelist, kept apart from `declared` because a
    /// PAL entry is a call the adapter forwards to a capability or refuses,
    /// not a capability itself.
    pub pal: Vec<&'static PalDef>,
    /// Imports in neither table.
    pub undeclared: Vec<ModuleImport>,
    pub exports: Vec<String>,
    pub missing_required_exports: Vec<&'static str>,
    pub missing_optional_exports: Vec<&'static str>,
    pub has_memory: bool,
}

impl Report {
    /// The two V1 conditions readable from the import and export names. A
    /// strict subset of what the validator checks, so it decides the exit
    /// code only in a build without the validator.
    pub fn would_be_rejected(&self) -> bool {
        !self.undeclared.is_empty() || !self.missing_required_exports.is_empty()
    }

    pub fn by_group(&self) -> BTreeMap<&'static str, Vec<&'static ImportDef>> {
        let mut m: BTreeMap<&'static str, Vec<&'static ImportDef>> = BTreeMap::new();
        for i in &self.declared {
            m.entry(group_name(i.group)).or_default().push(i);
        }
        m
    }

    /// Every capability the module can reach, directly or through a
    /// delegated PAL entry.
    pub fn effective_capabilities(&self) -> Vec<&'static ImportDef> {
        let mut out = self.declared.clone();
        for def in self
            .pal
            .iter()
            .filter_map(|p| p.disposition.capability())
            .filter_map(imports::lookup)
        {
            if !out.iter().any(|x| x.name == def.name) {
                out.push(def);
            }
        }
        out
    }

    /// What the module can do through its reachable capabilities.
    pub fn plain_english(&self) -> Vec<String> {
        let mut out = Vec::new();
        let reachable = self.effective_capabilities();
        let has = |g: Group| reachable.iter().any(|i| i.group == g);
        if has(Group::Http) {
            out.push("make outbound HTTP requests (host-side SSRF policy applies)".into());
        }
        if reachable.iter().any(|import| import.name == "socket_call") {
            out.push("open outbound TCP connections and resolve DNS names (host-side egress policy applies)".into());
        }
        if has(Group::Entropy) {
            out.push("draw randomness from the host CSPRNG".into());
        }
        if out.is_empty() {
            out.push("nothing but compute and reply — it holds no I/O capability at all".into());
        }
        out
    }


    pub fn sensitive_imports(&self) -> Vec<&'static ImportDef> {
        self.declared
            .iter()
            .copied()
            .filter(|i| i.sensitive)
            .collect()
    }
}

pub fn group_name(g: Group) -> &'static str {
    match g {
        Group::Core => "core",
        Group::Observe => "observe",
        Group::Clock => "clock",
        Group::Entropy => "entropy",
        Group::Http => "http",
        Group::Response => "response",
        Group::Socket => "socket",
        Group::Ctx => "ctx",
    }
}

pub fn kind_label(k: ImportKind) -> &'static str {
    match k {
        ImportKind::Sync => "sync",
        ImportKind::Future => "future",
    }
}

pub fn kind_note(i: &ImportDef) -> &'static str {
    match i.kind {
        ImportKind::Sync => "returns immediately; cannot block",
        ImportKind::Future => "returns a future handle; the guest polls for completion",
    }
}

pub fn analyse(wasm: &[u8]) -> anyhow::Result<Report> {
    use wasmparser::{Parser, Payload};

    let mut report = Report {
        bytes: wasm.len(),
        blake3: hex::encode(blake3::hash(wasm).as_bytes()),
        ..Default::default()
    };
    let mut found: Vec<&'static ImportDef> = Vec::new();
    let mut pal_found: Vec<&'static PalDef> = Vec::new();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::ImportSection(reader) => {
                // The compact-import encodings are flattened, so a grouped
                // import cannot hide a capability.
                for imp in reader.into_imports_with_offsets() {
                    let (_, imp) = imp?;
                    // The same three-way split the validator makes.
                    match imports::lookup(imp.name) {
                        Some(def) if imp.module == imports::HOST_MODULE => found.push(def),
                        _ => match pal::lookup(imp.module, imp.name) {
                            Some(p) => pal_found.push(p),
                            None => report.undeclared.push(ModuleImport {
                                module: imp.module.to_string(),
                                field: imp.name.to_string(),
                            }),
                        },
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for e in reader {
                    let e = e?;
                    report.exports.push(e.name.to_string());
                    if e.name == imports::exports::MEMORY
                        && matches!(e.kind, wasmparser::ExternalKind::Memory)
                    {
                        report.has_memory = true;
                    }
                }
            }
            _ => {}
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    report.declared = imports::IMPORTS
        .iter()
        .filter(|d| found.iter().any(|f| f.name == d.name) && seen.insert(d.name))
        .collect();

    let mut seen = std::collections::BTreeSet::new();
    report.pal = pal::PAL_IMPORTS
        .iter()
        .filter(|d| {
            pal_found
                .iter()
                .any(|f| f.module == d.module && f.func == d.func)
                && seen.insert((d.module, d.func))
        })
        .collect();

    for e in imports::exports::REQUIRED {
        if !report.exports.iter().any(|x| x == e) {
            report.missing_required_exports.push(e);
        }
    }
    for e in imports::exports::OPTIONAL {
        if !report.exports.iter().any(|x| x == e) {
            report.missing_optional_exports.push(e);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module assembled by hand: one `() -> ()` type, the imports, one
    /// memory, the exports.
    fn module(imports_: &[(&str, &str)], exports: &[&str]) -> Vec<u8> {
        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

        // type section: one type, () -> ()
        m.extend_from_slice(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00]);

        // import section
        let mut body = Vec::new();
        body.push(imports_.len() as u8);
        for (module_, field) in imports_ {
            body.push(module_.len() as u8);
            body.extend_from_slice(module_.as_bytes());
            body.push(field.len() as u8);
            body.extend_from_slice(field.as_bytes());
            body.extend_from_slice(&[0x00, 0x00]); // func, type 0
        }
        m.push(0x02);
        m.push(body.len() as u8);
        m.extend_from_slice(&body);

        // memory section: one memory, min 1 page
        m.extend_from_slice(&[0x05, 0x03, 0x01, 0x00, 0x01]);

        // export section
        let mut body = Vec::new();
        body.push(exports.len() as u8);
        for e in exports {
            body.push(e.len() as u8);
            body.extend_from_slice(e.as_bytes());
            if *e == "memory" {
                body.extend_from_slice(&[0x02, 0x00]); // memory 0
            } else {
                body.extend_from_slice(&[0x00, 0x00]); // func 0
            }
        }
        m.push(0x07);
        m.push(body.len() as u8);
        m.extend_from_slice(&body);
        m
    }

    #[test]
    fn a_well_formed_module_yields_its_capability_manifest() {
        let m = module(
            &[
                ("wisp", "http_body_read"),
                ("wisp", "http_fetch"),
                ("wisp", "resp_new"),
            ],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(!r.would_be_rejected(), "every import is declared");
        let names: Vec<_> = r.declared.iter().map(|i| i.name).collect();
        assert_eq!(
            names,
            vec!["http_fetch", "http_body_read", "resp_new"],
            "declared imports come out in table order"
        );
        assert!(r.has_memory, "the memory export is seen");
        assert_eq!(
            r.missing_required_exports,
            Vec::<&str>::new(),
            "every required export is present"
        );
        assert_eq!(
            r.missing_optional_exports,
            imports::exports::OPTIONAL,
            "the optional exports are reported as absent"
        );
    }

    #[test]
    fn a_present_optional_export_is_not_reported_missing() {
        for export in imports::exports::OPTIONAL {
            let m = module(&[("wisp", "http_body_read")], &[export]);
            let report = analyse(&m).expect("the module must be inspectable");
            let absent: Vec<_> = imports::exports::OPTIONAL
                .iter()
                .filter(|candidate| *candidate != export)
                .copied()
                .collect();
            assert_eq!(
                report.missing_optional_exports, absent,
                "doctor must distinguish the present optional export {export} from absent ones"
            );
        }
    }

    #[test]
    fn an_undeclared_import_is_a_rejection() {
        let m = module(
            &[("wisp", "http_body_read"), ("env", "socket")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(
            r.would_be_rejected(),
            "an undeclared import rejects the module"
        );
        assert_eq!(
            r.undeclared,
            vec![ModuleImport {
                module: "env".into(),
                field: "socket".into()
            }],
            "the undeclared import is named"
        );
    }

    #[test]
    fn the_right_name_under_the_wrong_module_is_still_undeclared() {
        let m = module(
            &[("wasi_snapshot_preview1", "http_body_read")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(
            r.would_be_rejected(),
            "the module name is part of an import's identity"
        );
        assert_eq!(
            r.undeclared[0].module, "wasi_snapshot_preview1",
            "the offending module is named"
        );
    }

    #[test]
    fn a_module_missing_a_required_export_is_not_deployable() {
        let m = module(&[("wisp", "http_body_read")], &["wisp_alloc", "memory"]);
        let r = analyse(&m).unwrap();
        assert!(
            r.would_be_rejected(),
            "a module the host cannot enter is rejected"
        );
        assert_eq!(
            r.missing_required_exports,
            vec!["wisp_handle", "wisp_wake"],
            "every missing export is named"
        );
    }


    #[test]
    fn sensitive_imports_are_called_out_separately() {
        let m = module(
            &[("wisp", "init_hash_seed")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert_eq!(
            r.sensitive_imports()
                .iter()
                .map(|i| i.name)
                .collect::<Vec<_>>(),
            vec!["init_hash_seed"],
            "the sensitive import is listed"
        );
    }

    #[test]
    fn a_module_with_no_io_capabilities_says_so_in_words() {
        let m = module(
            &[("wisp", "log_emit"), ("wisp", "resp_new")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert_eq!(
            r.plain_english(),
            vec!["nothing but compute and reply — it holds no I/O capability at all".to_string()],
            "logging and replying are not I/O capabilities"
        );
    }

    #[test]
    fn duplicate_imports_grant_the_capability_once() {
        let m = module(
            &[("wisp", "http_body_read"), ("wisp", "http_body_read")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        assert_eq!(
            analyse(&m).unwrap().declared.len(),
            1,
            "importing a function twice grants it once"
        );
    }

    #[test]
    fn the_hash_is_content_addressed() {
        let a = module(&[("wisp", "http_body_read")], &["memory"]);
        let b = module(&[("wisp", "http_fetch")], &["memory"]);
        assert_ne!(
            analyse(&a).unwrap().blake3,
            analyse(&b).unwrap().blake3,
            "different bytes, different hash"
        );
        assert_eq!(
            analyse(&a).unwrap().blake3,
            analyse(&a).unwrap().blake3,
            "the hash is a function of the bytes"
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(
            analyse(b"not a wasm module at all").is_err(),
            "garbage is an error, not a panic"
        );
    }
}
