//! The deferred-unit list travels inside the template as a custom section,
//! because an artifact and metadata kept beside it eventually get separated.
//! `wisp-engine` reads it back as `Template::deferred_init_units`.

use std::collections::BTreeSet;

use wisp_engine::DEFERRED_INIT_SECTION;

/// Append the section: little-endian `u32` count, then the ids. Nothing is
/// appended when nothing was deferred.
pub(super) fn append_deferred_init_section(wasm: &mut Vec<u8>, deferred: &BTreeSet<u32>) {
    if deferred.is_empty() {
        return;
    }
    let name = DEFERRED_INIT_SECTION.as_bytes();
    let mut body = Vec::with_capacity(name.len() + 8 + deferred.len() * 4);
    leb128_u32(&mut body, name.len() as u32);
    body.extend_from_slice(name);
    body.extend_from_slice(&(deferred.len() as u32).to_le_bytes());
    for id in deferred {
        body.extend_from_slice(&id.to_le_bytes());
    }
    wasm.push(0);
    leb128_u32(wasm, body.len() as u32);
    wasm.extend_from_slice(&body);
}

fn leb128_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_payload(wasm: &[u8]) -> Option<Vec<u8>> {
        wasmparser::Parser::new(0)
            .parse_all(wasm)
            .flatten()
            .find_map(|p| match p {
                wasmparser::Payload::CustomSection(c) if c.name() == DEFERRED_INIT_SECTION => {
                    Some(c.data().to_vec())
                }
                _ => None,
            })
    }

    #[test]
    fn the_section_carries_the_count_then_the_ids() {
        let mut wasm = wat::parse_str("(module)").expect("assembles");
        append_deferred_init_section(&mut wasm, &BTreeSet::from([2u32, 300]));
        assert_eq!(
            section_payload(&wasm),
            Some(vec![2, 0, 0, 0, 2, 0, 0, 0, 44, 1, 0, 0]),
            "two units, little-endian, in ascending order"
        );
        assert!(
            wasmparser::Validator::new().validate_all(&wasm).is_ok(),
            "the module stays valid with the section appended"
        );
    }

    #[test]
    fn nothing_deferred_appends_no_section() {
        let mut wasm = wat::parse_str("(module)").expect("assembles");
        let before = wasm.clone();
        append_deferred_init_section(&mut wasm, &BTreeSet::new());
        assert_eq!(wasm, before, "an absent section means nothing was deferred");
    }
}
