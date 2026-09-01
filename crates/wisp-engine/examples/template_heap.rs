//! Observes Linux process memory while loading and retaining templates.
//!
//! Allocator profiling and `/proc` mapping counters describe different parts
//! of the process footprint; their difference is not a precise code-size measure.
//!
//! ```text
//! scripts/dev cargo run -p wisp-engine --release --example template_heap -- \
//!     --guest guests/out/bigheap.wasm --n 32
//! scripts/dev cargo run -p wisp-engine --release --features dhat-heap --example template_heap -- \
//!     --guest guests/out/bigheap.wasm --n 8 --dhat
//! ```
//!
//! `--dhat` writes `dhat-heap.json` for allocator observations.
//! Serialized and mapped artifacts are produced by this invocation; mapped
//! files live in a private temporary directory owned by the run.

use std::time::Instant;

use wisp_engine::{EngineConfig, Template, WispEngine};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(all(feature = "mimalloc-alloc", not(feature = "dhat-heap")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

// ---------------------------------------------------------------------------
// process footprint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Footprint {
    rss_kb: u64,
    pss_kb: u64,
    private_kb: u64,
}

fn field(s: &str, key: &str) -> u64 {
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

/// Optionally trims the allocator before reading Linux process-memory counters.
fn footprint(trim: bool) -> Footprint {
    if trim {
        // SAFETY: `malloc_trim` takes no pointers and is safe to call at any
        // point; it is unsafe only because it is an FFI call.
        unsafe {
            libc::malloc_trim(0);
        }
    }
    let s = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    if s.is_empty() {
        let st = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        return Footprint {
            rss_kb: field(&st, "VmRSS:"),
            ..Default::default()
        };
    }
    Footprint {
        rss_kb: field(&s, "Rss:"),
        pss_kb: field(&s, "Pss:"),
        private_kb: field(&s, "Private_Dirty:") + field(&s, "Private_Clean:"),
    }
}

/// Resident kilobytes grouped by paths in `/proc/self/smaps`.
#[derive(Debug, Clone, Copy, Default)]
struct Vmas {
    memfd_kb: u64,
    anon_kb: u64,
    file_kb: u64,
    other_kb: u64,
}

impl Vmas {
    fn total_kb(&self) -> u64 {
        self.memfd_kb + self.anon_kb + self.file_kb + self.other_kb
    }
}

/// Descriptor observations complement the process byte counters.
fn open_fds() -> (usize, String) {
    let Ok(rd) = std::fs::read_dir("/proc/self/fd") else {
        return (0, String::new());
    };
    let mut n = 0usize;
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for e in rd.flatten() {
        n += 1;
        let t = std::fs::read_link(e.path()).unwrap_or_default();
        let t = t.to_string_lossy();
        // What the fd points at matters more than the count: an artifact file
        // held per template is a ceiling on tenants, an inherited pipe is not.
        let kind = if t.ends_with(".cwasm") {
            "artifact .cwasm"
        } else if t.ends_with(".meta") {
            "artifact .meta"
        } else if t.starts_with("/memfd:") {
            "memfd"
        } else if t.starts_with("pipe:") || t.starts_with("socket:") {
            "pipe/socket"
        } else {
            "other"
        };
        *kinds.entry(kind.to_string()).or_default() += 1;
    }
    let desc = kinds
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    (n, desc)
}

fn memfd_bytes() -> (usize, u64, u64) {
    let mut n = 0usize;
    let mut bytes = 0u64;
    let mut blocks = 0u64;
    let Ok(rd) = std::fs::read_dir("/proc/self/fd") else {
        return (0, 0, 0);
    };
    for e in rd.flatten() {
        let Ok(target) = std::fs::read_link(e.path()) else {
            continue;
        };
        if !target.to_string_lossy().starts_with("/memfd:") {
            continue;
        }
        n += 1;
        // `stat` on /proc/self/fd/N follows to the memfd itself.
        //
        // `len` is the memfd's *span*; `blocks` is what it actually occupies.
        // Keep span and allocated blocks separate for sparse files.
        if let Ok(m) = std::fs::metadata(e.path()) {
            use std::os::unix::fs::MetadataExt;
            bytes += m.len();
            blocks += m.blocks() * 512;
        }
    }
    (n, bytes, blocks)
}

fn vmas() -> Vmas {
    let s = match std::fs::read_to_string("/proc/self/smaps") {
        Ok(s) => s,
        Err(_) => return Vmas::default(),
    };
    let mut v = Vmas::default();
    // 0 = file-backed, 1 = memfd, 2 = anonymous, 3 = special (stack/vdso/...).
    let mut kind = 2usize;
    for line in s.lines() {
        // A header line is `addr-addr perms offset dev inode [path]`; a field
        // line is `Key: N kB`. Telling them apart by the colon is enough,
        // because no header field contains one.
        if let Some(rest) = line.strip_prefix("Rss:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
            match kind {
                0 => v.file_kb += kb,
                1 => v.memfd_kb += kb,
                2 => v.anon_kb += kb,
                _ => v.other_kb += kb,
            }
        } else if !line.contains(": ") && line.contains('-') {
            let path = line.split_whitespace().nth(5).unwrap_or("");
            kind = if path.is_empty() {
                2
            } else if path.starts_with("/memfd:") {
                1
            } else if path.starts_with('[') {
                3
            } else {
                0
            };
        }
    }
    v
}

// ---------------------------------------------------------------------------
// tenant-distinct module bytes
// ---------------------------------------------------------------------------

fn leb128(out: &mut Vec<u8>, mut v: u64) {
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

/// Appends a custom section so each index has distinct module bytes.
fn distinct_template(wasm: &[u8], tenant: u64) -> Vec<u8> {
    let name = b"wisp-heap-tenant";
    let mut section = Vec::new();
    leb128(&mut section, name.len() as u64);
    section.extend_from_slice(name);
    section.extend_from_slice(&tenant.to_le_bytes());

    let mut out = Vec::with_capacity(wasm.len() + section.len() + 8);
    out.extend_from_slice(wasm);
    out.push(0);
    leb128(&mut out, section.len() as u64);
    out.extend_from_slice(&section);
    out
}

// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy, Debug)]
enum Mode {
    /// `WispEngine::load_template` — cranelift compiles the module here.
    Compile,
    /// `WispEngine::deserialize` from an in-memory artifact. wasmtime copies
    /// the code into a fresh anonymous mapping.
    Deserialize,
    /// Loads this invocation's artifacts through `WispEngine::load_mapped`.
    Mapped,
}

struct Args {
    guest: String,
    n: usize,
    same: bool,
    dhat: bool,
    no_trim: bool,
    mode: Mode,
}

fn parse() -> Args {
    let a: Vec<String> = std::env::args().collect();
    let opt = |k: &str| -> Option<String> {
        a.iter()
            .position(|x| x == k)
            .and_then(|i| a.get(i + 1))
            .cloned()
            .or_else(|| {
                let p = format!("{k}=");
                a.iter()
                    .find_map(|x| x.strip_prefix(p.as_str()).map(str::to_string))
            })
    };
    Args {
        guest: opt("--guest").unwrap_or_else(|| "guests/out/bigheap.wasm".into()),
        n: opt("--n").and_then(|v| v.parse().ok()).unwrap_or(16),
        same: a.iter().any(|x| x == "--same"),
        dhat: a.iter().any(|x| x == "--dhat"),
        no_trim: a.iter().any(|x| x == "--no-trim"),
        mode: match opt("--mode").as_deref() {
            None | Some("compile") => Mode::Compile,
            Some("deserialize") => Mode::Deserialize,
            Some("mapped") => Mode::Mapped,
            Some(o) => panic!("--mode={o}: expected compile, deserialize or mapped"),
        },
    }
}

fn main() -> anyhow::Result<()> {
    let args = parse();
    let trim = !args.no_trim;

    #[cfg(not(feature = "dhat-heap"))]
    if args.dhat {
        anyhow::bail!(
            "--dhat needs the profiler compiled in: \
             cargo run -p wisp-engine --release --features dhat-heap --example template_heap"
        );
    }

    let wasm =
        std::fs::read(&args.guest).map_err(|e| anyhow::anyhow!("reading {}: {e}", args.guest))?;

    println!("PER-TEMPLATE MEMORY COST");
    println!(
        "  guest          {} ({} KB of wasm)",
        args.guest,
        wasm.len() / 1024
    );
    println!(
        "  templates      {} ({})",
        args.n,
        if args.same {
            "identical bytes"
        } else {
            "distinct per tenant"
        }
    );
    println!(
        "  malloc_trim    {}",
        if trim { "before every reading" } else { "OFF" }
    );
    println!("  load path      {:?}", args.mode);
    println!(
        "  allocator      {}",
        if cfg!(feature = "dhat-heap") {
            "dhat (profiling)"
        } else if cfg!(feature = "mimalloc-alloc") {
            "mimalloc"
        } else {
            "system (glibc malloc)"
        }
    );
    if cfg!(debug_assertions) {
        println!("  *** DEBUG BUILD — re-run with --release for numbers worth quoting. ***");
    }
    println!();

    // The engine, its pool reservation and its shared linker are not per
    // template and must not be charged to one.
    // Overridable so the address-space ceiling can be probed: the pooling
    // allocator reserves `max_instances x (max_memory + guard)` of PROT_NONE
    // virtual address space, and on x86-64 with 4-level paging the user half is
    // 128 TiB. That product, not RAM, is what decides how many instances a cell
    // can be *configured* for.
    let cfg = EngineConfig {
        max_instances: env_u64("WISP_MAX_INSTANCES", 10_000) as u32,
        guard_size_bytes: env_u64("WISP_GUARD_BYTES", EngineConfig::default().guard_size_bytes),
        ..EngineConfig::default()
    };
    println!(
        "  engine         max_instances={} guard={} MiB -> {:.1} TiB of address space reserved",
        cfg.max_instances,
        cfg.guard_size_bytes >> 20,
        cfg.max_instances as f64 * (cfg.max_memory_bytes + cfg.guard_size_bytes) as f64
            / (1u64 << 40) as f64,
    );
    let engine = WispEngine::new(cfg)?;
    let after_engine = footprint(trim);
    println!(
        "  engine ready, 0 templates:  RSS {} kB   PSS {} kB",
        after_engine.rss_kb, after_engine.pss_kb
    );

    // Prepare artifacts before measuring deserialize and mapped loads.
    let artifacts = build_artifacts(&engine, &wasm, &args)?;
    if args.mode != Mode::Compile {
        println!(
            "  prebuilt {} artifact(s), {} B each; the loop below is {:?} only",
            artifacts.blobs.len(),
            artifacts.blobs.first().map(|b| b.len()).unwrap_or(0),
            args.mode
        );
    }

    #[cfg(feature = "dhat-heap")]
    let _profiler = args.dhat.then(|| {
        // Deep enough to see through wasmtime's allocation helpers to the
        // caller that actually asked for the bytes.
        dhat::Profiler::builder().trim_backtraces(Some(16)).build()
    });

    // Warm one-time setup before taking the baseline.
    let warm = load(&engine, &wasm, &args, u64::MAX, &artifacts)?;
    let base = footprint(trim);
    let base_vmas = vmas();
    let base_untrimmed = footprint(false);
    println!(
        "  after 1 warm-up template:   RSS {} kB   PSS {} kB   (untrimmed RSS {} kB)",
        base.rss_kb, base.pss_kb, base_untrimmed.rss_kb
    );
    println!(
        "                              memfd {} kB, anon {} kB, file {} kB",
        base_vmas.memfd_kb, base_vmas.anon_kb, base_vmas.file_kb
    );
    println!();

    println!(
        "  {:>6}  {:>10} {:>10} {:>10}  {:>12} {:>12}",
        "n", "RSS kB", "PSS kB", "priv kB", "marg RSS kB", "marg PSS kB"
    );
    let mut held: Vec<Template> = Vec::with_capacity(args.n);
    let t0 = Instant::now();
    let step = (args.n / 8).max(1);
    let mut marks: Vec<(usize, Footprint, Vmas)> = Vec::new();
    for i in 0..args.n {
        held.push(load(&engine, &wasm, &args, i as u64, &artifacts)?);
        if i + 1 == 1 || (i + 1) % step == 0 || i + 1 == args.n {
            let f = footprint(trim);
            let k = (i + 1) as f64;
            println!(
                "  {:>6}  {:>10} {:>10} {:>10}  {:>12.1} {:>12.1}",
                i + 1,
                f.rss_kb.saturating_sub(base.rss_kb),
                f.pss_kb.saturating_sub(base.pss_kb),
                f.private_kb.saturating_sub(base.private_kb),
                f.rss_kb.saturating_sub(base.rss_kb) as f64 / k,
                f.pss_kb.saturating_sub(base.pss_kb) as f64 / k,
            );
            marks.push((i + 1, f, vmas()));
        }
    }
    let elapsed = t0.elapsed();
    let end = footprint(trim);
    let end_untrimmed = footprint(false);

    // The marginal slope over the LAST half of the sweep, which is free of
    // whatever the first few loads still had to warm.
    let (n_lo, f_lo, v_lo) = marks[marks.len() / 2];
    let (n_hi, f_hi, v_hi) = *marks.last().unwrap();
    let d = (n_hi - n_lo) as f64;
    let slope_rss = f_hi.rss_kb.saturating_sub(f_lo.rss_kb) as f64 / d;
    let slope_pss = f_hi.pss_kb.saturating_sub(f_lo.pss_kb) as f64 / d;

    println!();
    println!(
        "  compile+load    {:.1} ms/template",
        elapsed.as_secs_f64() * 1e3 / args.n as f64
    );
    println!(
        "  marginal (n={n_lo}..{n_hi}):  RSS {slope_rss:.1} kB/template   PSS {slope_pss:.1} kB/template"
    );
    println!();
    println!("  WHERE THE MARGINAL RSS LIVES (smaps, n={n_lo}..{n_hi}, kB per template):");
    println!(
        "    memfd (wasmtime CoW memory image)         {:>9.1}",
        v_hi.memfd_kb.saturating_sub(v_lo.memfd_kb) as f64 / d
    );
    println!(
        "    anonymous (compiled code, unwind, heap)   {:>9.1}",
        v_hi.anon_kb.saturating_sub(v_lo.anon_kb) as f64 / d
    );
    println!(
        "    file-backed (binary, libc — not per tmpl) {:>9.1}",
        v_hi.file_kb.saturating_sub(v_lo.file_kb) as f64 / d
    );
    println!(
        "    other ([stack], [heap], vdso)             {:>9.1}",
        v_hi.other_kb.saturating_sub(v_lo.other_kb) as f64 / d
    );
    println!(
        "    smaps total                               {:>9.1}   (smaps_rollup says {slope_rss:.1})",
        v_hi.total_kb().saturating_sub(v_lo.total_kb()) as f64 / d
    );
    println!();
    println!(
        "  untrimmed at end:  RSS {} kB  (trim releases {} kB of arena)",
        end_untrimmed.rss_kb.saturating_sub(base.rss_kb),
        end_untrimmed.rss_kb.saturating_sub(end.rss_kb),
    );

    // --- the accounting the engine can state about itself -------------------
    println!();
    println!("  TEMPLATE DETAILS AND PROCESS FILES:");
    let img = warm.memory_image();
    println!(
        "    memory image: {} B declared, {} B STORED in {} runs ({:.3}% dense)",
        img.len(),
        img.stored_bytes(),
        img.runs(),
        100.0 * img.stored_bytes() as f64 / img.len().max(1) as f64,
    );
    println!(
        "    memory_pages                                               {:>9}",
        warm.memory_pages()
    );
    let (_meta, cwasm) = engine.serialize_parts(&warm)?;
    println!(
        "    compiled artifact (cwasm; code + memory image)              {:>9} B",
        cwasm.len()
    );
    println!("    uses WASI PAL imports          {:>9}", warm.uses_pal());
    println!(
        "    deferred init units                                        {:>9}",
        warm.deferred_init_units().len()
    );
    let (fds, fd_bytes, fd_blocks) = memfd_bytes();
    let (nfd, fd_kinds) = open_fds();
    println!(
        "    open fds {nfd} for {} templates ({fd_kinds}) — RLIMIT_NOFILE caps tenants if this scales",
        args.n + 1
    );
    let per = (args.n + 1) as f64;
    println!(
        "    memfd files: {fds} held, span {:.0} B/template, allocated {:.0} B/template",
        fd_bytes as f64 / per,
        fd_blocks as f64 / per,
    );
    println!("      file-allocation counters are separate from the process RSS counters");
    drop(cwasm);

    println!();
    println!(
        "  total for {} templates: RSS {} kB, PSS {} kB above the 1-template baseline",
        args.n,
        end.rss_kb.saturating_sub(base.rss_kb),
        end.pss_kb.saturating_sub(base.pss_kb)
    );

    // --- what survives the template ----------------------------------------
    //
    // Compare the process footprint after dropping the measured templates.
    let n = args.n;
    if args.dhat {
        // The profile is dumped when `_profiler` drops, and its at-end column —
        // the only one that means "retained" rather than "touched" — is only
        // the per-template retention if every template is still alive then. So
        // under --dhat the drop probe is skipped rather than reordered: they
        // are two mutually exclusive experiments on the same run.
        println!();
        println!("  (--dhat: templates held to exit, so the drop probe is skipped)");
        // `held` is declared after `_profiler`, so it would otherwise drop
        // first and empty the at-end column this mode exists to read.
        std::mem::forget(held);
        drop(warm);
        return Ok(());
    }
    drop(held);
    let after_drop = footprint(trim);
    let after_drop_v = vmas();
    println!();
    println!("  RETAINED AFTER DROPPING ALL {n} TEMPLATES:");
    let held_back = after_drop.rss_kb.saturating_sub(base.rss_kb);
    println!("    RSS {held_back} kB above the pre-sweep baseline");
    println!(
        "    absolute RSS: {} kB at baseline -> {} kB at peak -> {} kB after drop",
        base.rss_kb, end.rss_kb, after_drop.rss_kb
    );
    // Keep the total process difference; per-template scaling needs separate measurements.
    println!("    (divide by {n} only if this scales with n — on the mapped path it does not)");
    println!(
        "    of which anonymous {} kB, [heap] {} kB",
        after_drop_v.anon_kb.saturating_sub(base_vmas.anon_kb),
        after_drop_v.other_kb.saturating_sub(base_vmas.other_kb),
    );

    // Held to here on purpose: the dhat profile is dumped when `_profiler`
    // drops, and "bytes still live at exit" is only the per-template retained
    // heap if the templates are still alive when it dumps. `warm` is the one
    // template deliberately kept.
    drop(warm);
    Ok(())
}

/// Selects identical module bytes or distinct content for each index.
fn bytes_for(wasm: &[u8], args: &Args, tenant: u64) -> Vec<u8> {
    if args.same {
        wasm.to_vec()
    } else {
        distinct_template(wasm, tenant)
    }
}

/// One template, by whichever route `--mode` selected.
///
/// `Artifacts` carries the pre-built `.cwasm` for the two deserialize modes, so
/// that cranelift runs *before* the measured region and its scratch cannot be
/// mistaken for something a template retains.
fn load(
    engine: &WispEngine,
    wasm: &[u8],
    args: &Args,
    tenant: u64,
    artifacts: &Artifacts,
) -> anyhow::Result<Template> {
    match args.mode {
        Mode::Compile => engine.load_template(&bytes_for(wasm, args, tenant)),
        Mode::Deserialize => {
            let a = artifacts.get(tenant);
            // SAFETY: this artifact was produced by `engine.serialize` on this
            // very engine, moments ago, and has not left the process.
            unsafe { engine.deserialize(a) }
        }
        Mode::Mapped => {
            // SAFETY: this invocation owns the private directory and only
            // this engine writes its artifacts.
            let (t, _compiled) =
                unsafe { engine.load_mapped(&bytes_for(wasm, args, tenant), artifacts.dir()?) }?;
            Ok(t)
        }
    }
}

/// Pre-built artifacts for the deserialize modes.
#[derive(Default)]
struct Artifacts {
    blobs: Vec<Vec<u8>>,
    dir: Option<tempfile::TempDir>,
    same: bool,
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

impl Artifacts {
    fn dir(&self) -> anyhow::Result<&std::path::Path> {
        self.dir
            .as_ref()
            .map(tempfile::TempDir::path)
            .ok_or_else(|| anyhow::anyhow!("mapped load requires its artifact directory"))
    }

    fn get(&self, tenant: u64) -> &[u8] {
        let i = if self.same {
            0
        } else {
            tenant as usize % self.blobs.len()
        };
        &self.blobs[i]
    }
}

/// Prepares artifacts before the measured load sweep.
fn build_artifacts(engine: &WispEngine, wasm: &[u8], args: &Args) -> anyhow::Result<Artifacts> {
    if args.mode == Mode::Compile {
        return Ok(Artifacts::default());
    }
    let n = if args.same { 1 } else { args.n };
    if args.mode == Mode::Mapped {
        let dir = tempfile::tempdir()?;
        for i in 0..n {
            let b = bytes_for(wasm, args, i as u64);
            engine.precompile_to_files(&b, dir.path())?;
        }
        return Ok(Artifacts {
            blobs: Vec::new(),
            dir: Some(dir),
            same: args.same,
        });
    }
    let mut blobs = Vec::with_capacity(n);
    for i in 0..n {
        let t = engine.load_template(&bytes_for(wasm, args, i as u64))?;
        blobs.push(engine.serialize(&t)?);
    }
    Ok(Artifacts {
        blobs,
        dir: None,
        same: args.same,
    })
}
