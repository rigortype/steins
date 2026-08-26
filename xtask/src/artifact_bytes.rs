//! `artifact-bytes`: where a package artifact's bytes actually go (issue #504).
//!
//! # Why this exists
//!
//! A generation's artifacts run many times the size of the source they were
//! lowered from, and once the publish phase became the bulk of a warm *edit*
//! (issue #517) that ratio stopped being a disk-footprint note and became the
//! cost of typing a character. Cutting it needs a number, not a taste: **how
//! much of the ratio is the codec's field-name repetition, and how much is the
//! lowering genuinely being larger than its source text?**
//!
//! Field names vanish under any schema-carrying codec — they are pure overhead.
//! Spans, resolved names and per-node vectors do not: they are content, and no
//! codec removes them. The split decides whether swapping the codec is
//! sufficient or whether the payload *shape* needs work too, so it is measured
//! here rather than argued.
//!
//! # Usage
//!
//! ```text
//! cargo xtask artifact-bytes <DIR>… [--no-php]
//! ```
//!
//! Per target it publishes one cold generation into a scratch store (the same
//! call `cargo xtask perf --warm` makes, so the artifacts are the real ones),
//! then reports:
//!
//! * the analyzed source bytes and the artifact bytes, per section, with the
//!   ratio the issue quotes;
//! * a JSON byte histogram over the same trees — field names, structural
//!   punctuation, string content, numbers — which is the phase-1 split;
//! * what the payload codec in force actually spends on those trees, so the
//!   swap's win is read off the same tool before and after.
//!
//! The histogram is computed by re-encoding each tree as JSON here rather than
//! by scanning the stored payload, so the line stays meaningful — and directly
//! comparable — after the stored payload stops being JSON. One caveat that
//! comes with that: the re-encoding uses whatever the serde schema says
//! *today*, so a change to a type's own codec moves the JSON line too. Issue
//! #504 moved it by 1.3% (`PhpStr` became a byte string, which JSON spells as
//! an array of numbers); the four-bucket split it exists for did not move.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use steins_db::{EffectsPolicy, PluginFacts, composer};
use steins_gen::{SectionName, Store};
use steins_infer::{FinalKeyword, GenerationParams, generation_check};
use steins_syntax::SourceTree;

use crate::corpus::collect_php_files;

/// Headroom for the worker thread the measurement runs on — the same number and
/// reasoning as `perf.rs`'s: CST recursion costs a frame per nesting level.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Entry point for `cargo xtask artifact-bytes <DIR>… [--no-php]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let php = !args.iter().any(|a| a == "--no-php");
    let targets: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if targets.is_empty() {
        return Err("usage: cargo xtask artifact-bytes <DIR>… [--no-php]".to_owned());
    }
    for target in targets {
        let dir = PathBuf::from(target);
        if !dir.is_dir() {
            return Err(format!("`{target}` is not a directory"));
        }
        let m = measure(&dir, php)?;
        print_measurement(target, &m);
    }
    Ok(())
}

/// What one target's artifacts weigh, and where the weight sits.
struct Measurement {
    files: usize,
    packages: usize,
    source_bytes: u64,
    /// Total container bytes across every package artifact of the generation.
    container_bytes: u64,
    /// Per section name, the bytes that section occupies summed over packages.
    sections: BTreeMap<String, u64>,
    /// What the payload codec in force spends on the target's lowered trees,
    /// summed — the `trace` section's payload area, without its directory.
    trace_payload_bytes: u64,
    /// The same trees, re-encoded as JSON, and where those bytes go.
    json: JsonSplit,
}

/// A JSON encoding's bytes, by what they are. The four buckets tile the
/// encoding exactly — `total` is asserted equal to their sum.
#[derive(Default)]
struct JsonSplit {
    total: u64,
    /// `"field":` — the quotes, the name, the colon. Removed wholesale by any
    /// schema-carrying codec.
    field_names: u64,
    /// `{`, `}`, `[`, `]`, `,` and whitespace — framing a length-prefixed
    /// format replaces with its own, much smaller, framing.
    structure: u64,
    /// String values: the quotes, the content, the escapes. Content.
    strings: u64,
    /// Numbers, `true`, `false`, `null` — a span offset, a slot index, an enum
    /// discriminant. Content, and the part a varint shrinks without removing.
    scalars: u64,
}

fn measure(dir: &Path, php: bool) -> Result<Measurement, String> {
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || measure_on_worker(&dir, php))
        .expect("failed to spawn the artifact-bytes worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn measure_on_worker(dir: &Path, php: bool) -> Result<Measurement, String> {
    let store = std::env::temp_dir().join(format!(
        "steins-artifact-bytes-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&store).map_err(|e| format!("create scratch store: {e}"))?;
    let result = measure_in_store(dir, &store, php);
    let _ = std::fs::remove_dir_all(&store);
    result
}

fn measure_in_store(dir: &Path, store: &Path, php: bool) -> Result<Measurement, String> {
    let files = collect_php_files(dir);
    let rel: Vec<PathBuf> =
        files.iter().map(|f| f.strip_prefix(dir).unwrap_or(f).to_path_buf()).collect();

    let mut source_bytes = 0u64;
    let mut json = JsonSplit::default();
    let mut trace_payload_bytes = 0u64;
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        source_bytes += text.len() as u64;
        let tree = SourceTree::parse(&text);
        let encoded = serde_json::to_vec(&tree).map_err(|e| format!("re-encode as JSON: {e}"))?;
        json.absorb(&encoded)?;
        trace_payload_bytes += steins_db::persist::trace_payload(&tree).len() as u64;
    }

    let layout = composer::discover(&[dir.to_path_buf()], dir);
    let partition = steins_db::partition::discover(&layout);
    let plugins = PluginFacts::discover(&layout, None);
    let effects = EffectsPolicy::none();
    let params = GenerationParams {
        store_root: store,
        capture_root: dir,
        files: &rel,
        layout: &layout,
        partition: &partition,
        plugins: &plugins,
        effects: &effects,
        warning_handler_abort: true,
        final_keyword: FinalKeyword::Enforced,
        php,
        paranoid: false,
    };
    generation_check(&params).map_err(|e| format!("cold generation build: {e}"))?;

    let opened = Store::open(store).map_err(|e| format!("open the scratch store: {e}"))?;
    let generation = opened
        .current()
        .map_err(|e| format!("read the published generation: {e}"))?
        .ok_or("the cold build published no generation")?;

    let mut container_bytes = 0u64;
    let mut sections: BTreeMap<String, u64> = BTreeMap::new();
    let packages: Vec<_> = generation.packages().cloned().collect();
    for package in &packages {
        let reader = generation
            .artifact(package)
            .map_err(|e| format!("open the artifact for `{}`: {e}", package.as_str()))?;
        let names: Vec<String> = reader.sections().map(|s| s.as_str().to_owned()).collect();
        for name in names {
            let section = SectionName::new(&name).expect("a listed section name");
            let len = reader.section_len(&section).unwrap_or(0);
            *sections.entry(name).or_default() += len;
            container_bytes += len;
        }
    }
    Ok(Measurement {
        files: files.len(),
        packages: packages.len(),
        source_bytes,
        container_bytes,
        sections,
        trace_payload_bytes,
        json,
    })
}

impl JsonSplit {
    /// Tokenize one JSON encoding and add its bytes to the four buckets.
    ///
    /// A minimal scanner, deliberately: `serde_json::from_slice` into a `Value`
    /// would allocate the whole document and still not say where the bytes went.
    /// The one distinction that needs care is a string in key position versus a
    /// string in value position — which is exactly the split being measured.
    fn absorb(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut i = 0usize;
        // The nesting stack: `true` for an object, `false` for an array. A
        // string is a field name exactly when the innermost container is an
        // object and no colon has been seen for the current member.
        let mut stack: Vec<bool> = Vec::new();
        let mut expecting_key = false;
        let before = self.total;
        while i < bytes.len() {
            let b = bytes[i];
            match b {
                b'{' => {
                    stack.push(true);
                    expecting_key = true;
                    self.structure += 1;
                    i += 1;
                }
                b'[' => {
                    stack.push(false);
                    expecting_key = false;
                    self.structure += 1;
                    i += 1;
                }
                b'}' | b']' => {
                    stack.pop();
                    expecting_key = false;
                    self.structure += 1;
                    i += 1;
                }
                b',' => {
                    expecting_key = stack.last().copied().unwrap_or(false);
                    self.structure += 1;
                    i += 1;
                }
                b':' => {
                    // The colon belongs to the field name it terminates.
                    expecting_key = false;
                    self.field_names += 1;
                    i += 1;
                }
                b'"' => {
                    let end = string_end(bytes, i)?;
                    let len = (end - i) as u64;
                    if expecting_key {
                        self.field_names += len;
                    } else {
                        self.strings += len;
                    }
                    i = end;
                }
                b' ' | b'\t' | b'\n' | b'\r' => {
                    self.structure += 1;
                    i += 1;
                }
                _ => {
                    let start = i;
                    while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b':' | b'"') {
                        i += 1;
                    }
                    self.scalars += (i - start) as u64;
                }
            }
        }
        self.total += bytes.len() as u64;
        let counted = self.field_names + self.structure + self.strings + self.scalars;
        if counted != self.total {
            return Err(format!(
                "the JSON histogram does not tile its input: {counted} counted, {} bytes (this document added {})",
                self.total,
                self.total - before
            ));
        }
        Ok(())
    }
}

/// The index just past the closing quote of the JSON string starting at `open`.
fn string_end(bytes: &[u8], open: usize) -> Result<usize, String> {
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("unterminated string in the JSON encoding".to_owned())
}

fn print_measurement(target: &str, m: &Measurement) {
    let mb = |n: u64| n as f64 / (1024.0 * 1024.0);
    let pct = |n: u64, d: u64| if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 };
    println!("{target}");
    println!("  {} PHP file(s), {:.2} MB of source", m.files, mb(m.source_bytes));
    println!(
        "  artifacts: {:.2} MB of section bytes across {} package(s) — {:.1}x the source",
        mb(m.container_bytes),
        m.packages,
        m.container_bytes as f64 / m.source_bytes.max(1) as f64,
    );
    for (name, bytes) in &m.sections {
        println!(
            "    {name:<10} {:>10.2} MB  {:>5.1}% of the artifact  {:>5.1}x the source",
            mb(*bytes),
            pct(*bytes, m.container_bytes),
            *bytes as f64 / m.source_bytes.max(1) as f64,
        );
    }
    println!(
        "  trace payloads under the codec in force: {:.2} MB ({:.1}x the source)",
        mb(m.trace_payload_bytes),
        m.trace_payload_bytes as f64 / m.source_bytes.max(1) as f64,
    );
    let j = &m.json;
    println!(
        "  the same trees as JSON: {:.2} MB ({:.1}x the source)",
        mb(j.total),
        j.total as f64 / m.source_bytes.max(1) as f64,
    );
    println!(
        "    field names  {:>10.2} MB  {:>5.1}%   (overhead — any schema-carrying codec removes it)",
        mb(j.field_names),
        pct(j.field_names, j.total),
    );
    println!(
        "    structure    {:>10.2} MB  {:>5.1}%   (overhead — replaced by length prefixes)",
        mb(j.structure),
        pct(j.structure, j.total),
    );
    println!(
        "    strings      {:>10.2} MB  {:>5.1}%   (content — names, spellings, docblocks)",
        mb(j.strings),
        pct(j.strings, j.total),
    );
    println!(
        "    scalars      {:>10.2} MB  {:>5.1}%   (content — spans, slots, discriminants)",
        mb(j.scalars),
        pct(j.scalars, j.total),
    );
    let overhead = j.field_names + j.structure;
    println!(
        "    => {:.1}% of the JSON encoding is codec overhead; the other {:.2} MB ({:.1}x the source) is content",
        pct(overhead, j.total),
        mb(j.total - overhead),
        (j.total - overhead) as f64 / m.source_bytes.max(1) as f64,
    );
    println!(
        "       (that is what removing the overhead alone would leave, not a floor: a binary codec spells the same content more cheaply — varints for the scalars, a length prefix instead of quotes and escapes)"
    );
}
