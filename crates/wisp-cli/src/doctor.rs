//! Inspect the capabilities and entry points declared by a WebAssembly module.

use std::collections::BTreeMap;

use wisp_abi::imports::{self, Group, ImportDef, ImportKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImport {
    pub module: String,
    pub field: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub bytes: usize,
    pub blake3: String,
    /// Imports that are in the platform's manifest, in canonical order.
    pub declared: Vec<&'static ImportDef>,
    /// Imports that are NOT — each one is a V1 rejection (§4.3).
    pub undeclared: Vec<ModuleImport>,
    pub exports: Vec<String>,
    pub missing_required_exports: Vec<&'static str>,
    pub missing_optional_exports: Vec<&'static str>,
    pub has_memory: bool,
}

impl Report {
    /// V1: any import outside `wisp_abi::imports::IMPORTS` is an attempted
    /// capability and the module is rejected. There is no dynamic-linking
    /// escape hatch, so this is decidable from the bytes alone.
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

    /// The one-line answer to "what can this code do?", in the terms a tenant
    /// (or their auditor) actually cares about.
    pub fn plain_english(&self) -> Vec<String> {
        let mut out = Vec::new();
        let has = |g: Group| self.declared.iter().any(|i| i.group == g);
        if has(Group::Http) {
            out.push("make outbound HTTP requests (host-side SSRF policy applies — §8.6)".into());
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
        self.declared.iter().copied().filter(|i| i.sensitive).collect()
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
        Group::Ctx => "ctx",
    }
}

pub fn kind_label(k: ImportKind) -> &'static str {
    match k {
        ImportKind::Sync => "sync",
        ImportKind::Future => "future",
    }
}

/// What each policy bit *means*, spelled out. The point of `wisp doctor` is
/// not to dump a table but to make the consequence legible.
pub fn kind_note(i: &ImportDef) -> &'static str {
    match i.kind {
        ImportKind::Sync => "returns immediately; cannot block",
        ImportKind::Future => "returns a future handle; the guest polls for completion",
    }
}

/// Read a module's imports and exports and diff them against the platform's
/// canonical manifest.
pub fn analyse(wasm: &[u8]) -> anyhow::Result<Report> {
    use wasmparser::{Parser, Payload};

    let mut report = Report {
        bytes: wasm.len(),
        blake3: hex::encode(blake3::hash(wasm).as_bytes()),
        ..Default::default()
    };
    let mut found: Vec<&'static ImportDef> = Vec::new();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::ImportSection(reader) => {
                // `into_imports_with_offsets` flattens the compact-import
                // encodings, so a module cannot hide a capability behind a
                // grouped import form.
                for imp in reader.into_imports_with_offsets() {
                    let (_, imp) = imp?;
                    match imports::lookup(imp.name) {
                        Some(def) if imp.module == imports::HOST_MODULE => found.push(def),
                        // Right name, wrong module: still an undeclared
                        // capability. The module name is part of the identity.
                        _ => report.undeclared.push(ModuleImport {
                            module: imp.module.to_string(),
                            field: imp.name.to_string(),
                        }),
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

    // Canonical order, deduplicated: a module may import the same function
    // twice, but it has the capability once.
    let mut seen = std::collections::BTreeSet::new();
    report.declared = imports::IMPORTS
        .iter()
        .filter(|d| found.iter().any(|f| f.name == d.name) && seen.insert(d.name))
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

    /// A minimal module hand-assembled as bytes: header, type section, import
    /// section, memory + function exports. Enough to exercise the reader
    /// without a wat dependency in this crate.
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
            &[("wisp", "http_body_read"), ("wisp", "http_fetch"), ("wisp", "init_hash_seed")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(!r.would_be_rejected());
        let names: Vec<_> = r.declared.iter().map(|i| i.name).collect();
        assert_eq!(names, vec!["init_hash_seed", "http_fetch", "http_body_read"]);
        assert!(r.has_memory);
        assert_eq!(r.missing_required_exports, Vec::<&str>::new());
        assert_eq!(r.missing_optional_exports, vec!["wisp_gc_hint", "wisp_initialize"]);
    }

    /// V1 (§4.3): an undeclared import is an attempted capability, and the
    /// module is rejected. This is the whole of invariant 4's teeth.
    #[test]
    fn an_undeclared_import_is_a_rejection() {
        let m = module(
            &[("wisp", "http_body_read"), ("env", "socket")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(r.would_be_rejected());
        assert_eq!(
            r.undeclared,
            vec![ModuleImport { module: "env".into(), field: "socket".into() }]
        );
    }

    /// A declared *name* under a different module is still not a capability
    /// this platform granted.
    #[test]
    fn the_right_name_under_the_wrong_module_is_still_undeclared() {
        let m = module(
            &[("wasi_snapshot_preview1", "http_body_read")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert!(r.would_be_rejected());
        assert_eq!(r.undeclared[0].module, "wasi_snapshot_preview1");
    }

    #[test]
    fn a_module_missing_a_required_export_is_not_deployable() {
        let m = module(&[("wisp", "http_body_read")], &["wisp_alloc", "memory"]);
        let r = analyse(&m).unwrap();
        assert!(r.would_be_rejected());
        assert_eq!(r.missing_required_exports, vec!["wisp_handle", "wisp_wake"]);
    }


    #[test]
    fn sensitive_imports_are_called_out_separately() {
        let m = module(
            &[("wisp", "init_hash_seed")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        let r = analyse(&m).unwrap();
        assert_eq!(r.sensitive_imports().iter().map(|i| i.name).collect::<Vec<_>>(), vec!["init_hash_seed"]);
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
            vec!["nothing but compute and reply — it holds no I/O capability at all".to_string()]
        );
    }

    #[test]
    fn duplicate_imports_grant_the_capability_once() {
        let m = module(
            &[("wisp", "http_body_read"), ("wisp", "http_body_read")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        );
        assert_eq!(analyse(&m).unwrap().declared.len(), 1);
    }

    #[test]
    fn the_hash_is_content_addressed() {
        let a = module(&[("wisp", "http_body_read")], &["memory"]);
        let b = module(&[("wisp", "http_fetch")], &["memory"]);
        assert_ne!(analyse(&a).unwrap().blake3, analyse(&b).unwrap().blake3);
        assert_eq!(analyse(&a).unwrap().blake3, analyse(&a).unwrap().blake3);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(analyse(b"not a wasm module at all").is_err());
    }
}
