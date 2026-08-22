//! `cargo xtask licenses` — regenerate `THIRD-PARTY-LICENSES.md` from the
//! dependencies bundled into the release binary.
//!
//! A thin wrapper around `cargo about generate about.hbs`, with no post-
//! processing: cargo-about's own per-license-text grouping renders as-is, so a
//! crate whose license text typographically differs from another's otherwise-
//! identical text gets its own section rather than being folded in. (An
//! earlier pass merged shared-body sections — collapsing 39 MIT sections with
//! 36 distinct copyright holders into one — which reads accurately but defeats
//! one-crate-one-block scanning; dropped in favor of matching
//! `rigortype/lisplens`'s unprocessed `about.toml`/`about.hbs` shape.)
//!
//! `xtask` stays the entry point rather than a bare `cargo about generate` call
//! because `xtask` is already this repo's home for generation steps, and
//! finding the repo root and reporting the byte count are worth keeping in one
//! place.

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
        .map_err(|e| format!(
            "running `cargo about`: {e} (install it with `cargo install --locked cargo-about --features cli`, \
             at the version .github/workflows/ci.yml pins)"
        ))?;
    if !out.status.success() {
        return Err(format!(
            "`cargo about generate` failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
