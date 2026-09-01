//! A template with a genuinely large, genuinely non-zero pre-initialized image.
//!
//! The density claim is meaningless against an all-zero template: zero pages
//! are shared by the kernel whatever the engine does. Marginal cost is meant
//! to be *dirty pages*, so the template has to have real content that a naive
//! implementation would have to copy per instance.

#![allow(dead_code)]

/// Bytes of non-zero initialized data in the template image. This stands in for
/// what pre-initialization actually bakes: a DI container, an interned string
/// table, parsed config.
pub const IMAGE_BYTES: usize = 8 << 20;

/// A guest with `pages * 64 KiB` of linear memory, `IMAGE_BYTES` of it
/// initialized to a non-zero pattern, whose `wisp_handle` dirties `n` 4 KiB
/// pages (the record's `dispatch_id` is ignored; `n` is baked in).
pub fn big_guest(pages: u32, dirty_pages: u32) -> Vec<u8> {
    assert!(pages as usize * 65536 >= IMAGE_BYTES);

    // Printable ASCII only, so the data segment needs no escaping and the .wat
    // source stays the same size as the data it carries.
    const UNIT: &[u8] = b"WISP-template-page-content-0123456789abcdefghijklmnopqrstuvwxy";
    let mut data = String::with_capacity(IMAGE_BYTES + 64);
    while data.len() < IMAGE_BYTES {
        data.push_str(std::str::from_utf8(UNIT).unwrap());
    }
    data.truncate(IMAGE_BYTES);

    // Dirty pages start above the initialized region, so a dirtied page is a
    // genuinely private page rather than a copy of a shared one... except that
    // is the *easy* case. Start inside the image instead: writing to a
    // CoW-mapped page is what actually forces a private copy, and that is the
    // cost the density claim is about.
    let src = format!(
        r#"
(module
  (memory (export "memory") {pages} {pages})
  (data (i32.const 0) "{data}")
  (global $bump (mut i32) (i32.const 16))

  (func (export "wisp_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    ;; Allocate out of the last 64 KiB, so the allocator's own writes do not
    ;; swamp the dirty-page measurement.
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const 65000))
      (then (global.set $bump (i32.const 16))))
    (local.set $p (i32.add (i32.const {alloc_base}) (global.get $bump)))
    (global.set $bump
      (i32.and (i32.add (i32.add (global.get $bump) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  (func (export "wisp_handle") (param $ptr i32) (param $len i32) (result i64)
    (call $dirty (i32.const {dirty_pages}))
    (i64.const 0))

  (func (export "wisp_wake") (param $f i64) (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))

  ;; One 8-byte store per 4 KiB page, starting at 4 KiB — inside the
  ;; pre-initialized image, so each store forces a copy-on-write fault.
  (func $dirty (param $n i32)
    (local $i i32)
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $n)))
      (i64.store (i32.add (i32.const 4096) (i32.mul (local.get $i) (i32.const 4096)))
                 (i64.const 3735928559))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l))))

  ;; Reads a byte the guest never wrote: proves the CoW template content is
  ;; actually visible rather than the instance having been handed zeros.
  (func (export "probe") (param $a i32) (result i64) (i64.load (local.get $a)))
)
"#,
        alloc_base = pages as usize * 65536 - 65536,
    );
    wat::parse_str(&src).expect("big fixture must assemble")
}
