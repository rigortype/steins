//! Stamp a fingerprint of the analyzer's own sources into the crate, so a
//! generation identity can tell two builds apart (issue #563).
//!
//! `analyzer_version()` was `CARGO_PKG_VERSION` alone, and its doc comment
//! claimed what the value did not carry: "a new Steins is a new universe". Two
//! builds of `0.1.6` are not a new universe by that reading, so they shared a
//! store — and the second could be served findings the first computed, which
//! inverts ADR-0092 §2's invariant (a MISS costs time and never changes an
//! answer; a HIT was changing one).
//!
//! **Why the sources and not the git revision.** The revision is what the
//! `version` banner stamps, and it is the wrong primitive here for the case
//! that matters most: a contributor A/B-ing a branch against master in one
//! working tree is at ONE revision with two different trees, and even a
//! dirty-flag refinement gives both of them the same answer. A content hash
//! distinguishes exactly what needs distinguishing — two source trees that
//! could disagree about a finding — and it needs no git at all, so a build from
//! a published tarball is fingerprinted as precisely as one from a checkout.
//!
//! **Why every crate's sources and not this one's.** The question is whether
//! two builds can disagree about a finding, and the answer runs through the
//! whole analysis stack: the domain, the contract lowering, the syntax
//! lowering, the catalog, the shard layer. Hashing all of `crates/*/src`
//! over-invalidates by including the ones that cannot change a finding
//! (`steins-cli`, `steins-wasm`), and that is the safe direction: a spurious
//! rebuild costs time, a spurious HIT costs correctness. It is also the rule
//! that needs no maintenance as crates are added.
//!
//! **Why the files the sources embed, too.** A crate can compile a file that is
//! not Rust into the binary: `steins-sidecar` embeds `runner.php`, the program
//! every fold runs, with `include_str!`. A walk of `src/**/*.rs` alone did not
//! see it, so a runner change (252b0e1e) left the fingerprint where it was, and
//! a warm run replayed walk blocks the old runner had folded — the bug above,
//! one file over. So the walk also follows every `include!`, `include_str!`
//! and `include_bytes!` a source names, and an argument it cannot follow to a
//! file fails the build: an embed the fingerprint cannot see is that bug
//! waiting for its next edit. So does a walk that finds nothing or a file it
//! cannot read, since either would stamp a value that does not describe the
//! tree.
//!
//! **Why `Cargo.lock` and the manifests.** The analyzer is also what its
//! dependencies make it. A Mago bump moves the parser's rev in the root
//! `Cargo.toml` and in `Cargo.lock`, changes the syntax lowering, and touches
//! nothing under `src`; a manifest can switch a feature or a profile setting
//! the same way. So the lockfile, the root manifest and every crate's manifest
//! are hashed too. The one build this cannot pin is `cargo install --git`
//! without `--locked`, which resolves afresh rather than reading the lockfile
//! it hashes.
//!
//! **What it costs.** A released binary has fixed sources, so its identity is
//! stable across rebuilds and its store keeps working. A working tree
//! invalidates the store whenever any analyzer source changes — which is the
//! honest answer, since the analyzer did change.
//!
//! No dependency is added: FNV-1a is a few lines, and the value only has to be
//! stable and collision-resistant enough to separate source trees.

use std::path::{Path, PathBuf};

/// The macros that compile a file into the crate whose source names it.
const EMBEDS: [&str; 3] = ["include!", "include_str!", "include_bytes!"];

fn main() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/ is the parent");
    let root = canonical(crates.parent().expect("the workspace root is the parent of crates/"));
    let mut sources = Vec::new();
    let mut builds = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    for e in list(crates) {
        let src = e.join("src");
        if src.is_dir() {
            collect(&src, &mut sources);
        }
        let manifest = e.join("Cargo.toml");
        if manifest.is_file() {
            builds.push(manifest);
        }
    }
    assert!(!sources.is_empty(), "no crates/*/src/**/*.rs under {}", crates.display());
    let mut embeds = Vec::new();
    for rs in &sources {
        let text = std::fs::read_to_string(rs)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", rs.display()));
        embedded(rs, &text, &mut embeds);
    }
    // Everything hashed that is not a `crates/*/src/**/*.rs` file.
    let mut extra: Vec<PathBuf> = embeds.iter().chain(&builds).map(|f| canonical(f)).collect();
    extra.sort();
    extra.dedup();
    // Sorted so the fingerprint is a property of the tree and not of the order
    // the filesystem happened to report it in; deduplicated because an
    // `include!` can name a file the walk already holds.
    let mut files: Vec<PathBuf> = sources.iter().map(|f| canonical(f)).collect();
    files.extend_from_slice(&extra);
    files.sort();
    files.dedup();
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for f in &files {
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("cannot read {}: {e}", f.display()));
        field(&mut h, name(&root, f).as_bytes());
        field(&mut h, &bytes);
        println!("cargo:rerun-if-changed={}", f.display());
    }
    println!("cargo:rustc-env=STEINS_ANALYZER_FINGERPRINT={h:016x}");
    // What the fingerprint covers past the sources, so a test can pin the
    // runner and the lockfile in it without re-deriving the walk.
    let extra: Vec<String> = extra.iter().map(|f| name(&root, f)).collect();
    println!("cargo:rustc-env=STEINS_ANALYZER_EXTRA_INPUTS={}", extra.join(";"));
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for p in list(dir) {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every file `rs` compiles in through [`EMBEDS`], resolved against the
/// directory of `rs`, as the macros resolve it. Line comments are dropped
/// first, so prose that mentions a macro is not read as a use of it.
///
/// An argument that is not a string literal naming a file panics: a
/// `concat!`, an `env!`, or a macro that wraps the call would compile a file
/// this walk cannot see.
fn embedded(rs: &Path, text: &str, out: &mut Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().map(|l| l.split_once("//").map_or(l, |(c, _)| c)).collect();
    let code = lines.join("\n");
    let dir = rs.parent().expect("a source file has a directory");
    for mac in EMBEDS {
        for (at, _) in code.match_indices(mac) {
            // `my_include!` is another macro.
            if code[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let Some(arg) = code[at + mac.len()..].trim_start().strip_prefix(['(', '[', '{'])
            else {
                continue;
            };
            let target = arg
                .trim_start()
                .strip_prefix('"')
                .and_then(|lit| lit.split_once('"'))
                .map(|(lit, _)| dir.join(lit))
                .filter(|t| t.is_file());
            let Some(target) = target else {
                panic!(
                    "{}: this `{mac}` names no file the analyzer fingerprint can follow; name \
                     it with a string literal, or teach crates/steins-infer/build.rs to find it",
                    rs.display()
                );
            };
            out.push(target);
        }
    }
}

/// The entries of `dir`, or a failed build: a directory the walk cannot list
/// would drop its files from the fingerprint without a word.
fn list(dir: &Path) -> Vec<PathBuf> {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    entries
        .map(|e| e.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display())).path())
        .collect()
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|e| panic!("cannot resolve {}: {e}", p.display()))
}

/// The name a file is hashed under: relative to the workspace root and
/// `/`-separated, so where the tree is checked out does not move the
/// fingerprint. A file outside the tree keeps its full path.
fn name(root: &Path, f: &Path) -> String {
    let rel = f.strip_prefix(root).unwrap_or(f);
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    parts.join("/")
}

/// FNV-1a over one length-prefixed field, folded in place so one hasher spans
/// every file. The prefix keeps a name and its contents from running together.
fn field(h: &mut u64, bytes: &[u8]) {
    for b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}
