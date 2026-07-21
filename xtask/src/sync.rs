//! `corpus-sync`: materialize the pinned corpus as shallow git checkouts.
//!
//! Pinning discipline (ADR-0013 reproducibility): on first run (or a missing
//! entry, or `--update`) the latest stable release tag is resolved from the
//! remote and recorded in `corpus.lock.toml`; otherwise the lock is authoritative
//! and we clone exactly what it says. Existing correct checkouts are left alone.

use std::path::Path;
use std::process::Command;

use semver::Version;

use crate::corpus::{
    Lock, LockEntry, PACKAGES, checkout_dir, read_lock, write_lock,
};

/// Entry point for `cargo xtask corpus-sync [--update]`.
pub fn run(update: bool) -> Result<(), String> {
    let mut lock = read_lock();
    let mut changed = false;

    for pkg in PACKAGES {
        let need_resolve = update || lock.get(pkg.name).is_none();
        if need_resolve {
            print!("resolving {} … ", pkg.name);
            let (tag, commit) = resolve_latest_stable(pkg.repo)?;
            println!("{tag} ({})", short(&commit));
            lock.upsert(LockEntry {
                name: pkg.name.to_owned(),
                repo: pkg.repo.to_owned(),
                tag,
                commit,
            });
            changed = true;
        }
    }

    if changed {
        write_lock(&lock);
        println!("wrote {}", crate::corpus::lock_path().display());
    }

    // Clone/verify against the (now-complete) lock.
    for entry in &lock.packages {
        sync_one(entry, &lock)?;
    }
    Ok(())
}

/// Ensure `corpus/<pkg>` is a checkout at the locked tag; clone if absent.
fn sync_one(entry: &LockEntry, _lock: &Lock) -> Result<(), String> {
    let dir = checkout_dir(&entry.name);
    if dir.join(".git").is_dir() {
        let head = git_head(&dir)?;
        if head == entry.commit {
            println!("ok    {} @ {} ({})", entry.name, entry.tag, short(&entry.commit));
            return Ok(());
        }
        // Wrong revision (lock changed, or stale) → replace the checkout.
        println!("stale {} (have {}, want {}) — re-cloning", entry.name, short(&head), short(&entry.commit));
        std::fs::remove_dir_all(&dir).map_err(|e| format!("rm {}: {e}", dir.display()))?;
    }

    std::fs::create_dir_all(dir.parent().expect("checkout has parent"))
        .map_err(|e| format!("mkdir corpus: {e}"))?;
    println!("clone {} @ {} …", entry.name, entry.tag);
    let status = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            &entry.tag,
            &entry.repo,
            &dir.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("spawn git clone: {e}"))?;
    if !status.success() {
        return Err(format!("git clone failed for {}", entry.name));
    }

    // Verify the shallow clone landed on the locked commit.
    let head = git_head(&dir)?;
    if head != entry.commit {
        return Err(format!(
            "{}: cloned {} but tag {} points at {} (lock drift?)",
            entry.name,
            short(&head),
            entry.tag,
            short(&entry.commit)
        ));
    }
    Ok(())
}

/// `git rev-parse HEAD` in `dir`.
fn git_head(dir: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("spawn git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-parse failed in {}", dir.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Resolve the highest stable-semver release tag of a remote, returning
/// `(tag, commit)`. Pre-releases (alpha/beta/RC/dev) are skipped.
fn resolve_latest_stable(repo: &str) -> Result<(String, String), String> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", repo])
        .output()
        .map_err(|e| format!("spawn git ls-remote: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-remote failed for {repo}"));
    }
    let text = String::from_utf8_lossy(&out.stdout);

    // Map tag -> commit. Prefer the peeled (`^{}`) object of an annotated tag,
    // which is the actual commit the tag names.
    let mut best: Option<(Version, String, String)> = None; // (ver, tag, commit)
    let mut peeled: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut direct: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(hash), Some(reff)) = (it.next(), it.next()) else { continue };
        let Some(tag) = reff.strip_prefix("refs/tags/") else { continue };
        if let Some(base) = tag.strip_suffix("^{}") {
            peeled.insert(base.to_owned(), hash.to_owned());
        } else {
            direct.insert(tag.to_owned(), hash.to_owned());
        }
    }

    for (tag, direct_hash) in &direct {
        let Some(version) = parse_stable(tag) else { continue };
        let commit = peeled.get(tag).cloned().unwrap_or_else(|| direct_hash.clone());
        let better = best.as_ref().is_none_or(|(bv, _, _)| version > *bv);
        if better {
            best = Some((version, tag.clone(), commit));
        }
    }

    best.map(|(_, tag, commit)| (tag, commit))
        .ok_or_else(|| format!("no stable release tag found for {repo}"))
}

/// Parse a tag as a *stable* semver version, or `None` for pre-releases and
/// non-version tags. Accepts an optional leading `v`.
fn parse_stable(tag: &str) -> Option<Version> {
    let raw = tag.strip_prefix('v').unwrap_or(tag);
    let version = Version::parse(raw).ok()?;
    // Stable only: no pre-release identifiers (alpha/beta/RC/dev/rc…).
    if version.pre.is_empty() { Some(version) } else { None }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
