//! `cargo xtask licenses` — regenerate `THIRD-PARTY-LICENSES.md` from the
//! dependencies bundled into the release binary.
//!
//! A thin wrapper around `cargo about generate about.hbs`: this used to run a
//! second pass that merged sections sharing a license body (collapsing, on this
//! tree, 39 MIT sections with 36 distinct copyright holders into one section
//! listing every holder above a single shared body). That reads as one
//! license with many attributions, which is accurate, but it is also the
//! opposite of how a reader scans a notices file — one crate, one block — and
//! it made every MIT-licensed crate here harder to find than it needed to be.
//! So this wrapper does none of that: cargo-about's own per-license-text
//! grouping is left exactly as it renders, and a crate whose license happens
//! to typographically differ from another crate's otherwise-identical text
//! lands in its own section rather than being folded into one. Nothing here
//! is UNIQUE to this repo — `rigortype/lisplens` ships the same `about.toml` +
//! `about.hbs` shape with no post-processing at all, and this file now matches
//! it.
//!
//! `xtask` stays the entry point (`cargo xtask licenses`) rather than a bare
//! `cargo about generate ... -o ...` because `xtask` is already this repo's
//! home for generation steps, and finding the repo root and reporting the
//! byte count on success are conveniences worth keeping in one place.

use std::path::Path;
use std::process::Command;

/// Run the generator and write its output to `THIRD-PARTY-LICENSES.md`.
///
/// # Errors
/// When `cargo about` is missing or fails, or when the file cannot be written.
pub fn run() -> Result<(), String> {
    let root = crate::corpus::repo_root();
    let generated = generate(&root)?;
    let out = root.join("THIRD-PARTY-LICENSES.md");
    std::fs::write(&out, &generated).map_err(|e| format!("writing {}: {e}", out.display()))?;
    eprintln!("wrote {} ({} bytes)", out.display(), generated.len());
    Ok(())
}

/// Invoke `cargo about generate about.hbs` and return its output.
fn generate(root: &Path) -> Result<String, String> {
    let out = Command::new("cargo")
        .current_dir(root)
        .args(["about", "generate", "about.hbs"])
        .output()
        .map_err(|e| format!("running `cargo about`: {e} (install it with `cargo install cargo-about`)"))?;
    if !out.status.success() {
        return Err(format!(
            "`cargo about generate` failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
