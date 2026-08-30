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
//! **What it costs.** A released binary has fixed sources, so its identity is
//! stable across rebuilds and its store keeps working. A working tree
//! invalidates the store whenever any analyzer source changes — which is the
//! honest answer, since the analyzer did change.
//!
//! No dependency is added: FNV-1a is a few lines, and the value only has to be
//! stable and collision-resistant enough to separate source trees.

use std::path::{Path, PathBuf};

fn main() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/ is the parent");
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(crates) {
        for e in entries.flatten() {
            let src = e.path().join("src");
            if src.is_dir() {
                collect(&src, &mut files);
            }
        }
    }
    // Sorted so the fingerprint is a property of the tree and not of the order
    // the filesystem happened to report it in.
    files.sort();
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for f in &files {
        fnv(&mut h, f.to_string_lossy().as_bytes());
        if let Ok(bytes) = std::fs::read(f) {
            fnv(&mut h, &bytes);
        }
        println!("cargo:rerun-if-changed={}", f.display());
    }
    println!("cargo:rustc-env=STEINS_ANALYZER_FINGERPRINT={h:016x}");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// FNV-1a, folded in place so one hasher spans every file.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}
