//! `cargo xtask licenses` — generate `THIRD-PARTY-LICENSES.md`, then merge the
//! sections whose license bodies differ only in typography (issue #43).
//!
//! # Why a pass after the generator
//!
//! `cargo about` groups dependencies by **exact license text**, so a crate that
//! ships Apache-2.0 centred and hard-wrapped and a crate that ships it
//! left-aligned and unwrapped land in two sections of the same name. Measured on
//! this tree: two `## Apache License 2.0` sections whose bodies are identical
//! once whitespace is collapsed — 8,641 characters each — and two `## ISC
//! License` sections in the same situation.
//!
//! The grouping happens inside cargo-about, and `about.hbs` is a Handlebars
//! template that cannot normalize before it. So the fix is a deterministic pass
//! over the generator's output rather than a template change, and it lives here
//! because `xtask` is already this repo's home for generation steps — adding a
//! script in another language would add a toolchain to CI for one file.
//!
//! # What it merges, and what it must not
//!
//! Merging is keyed on the **normalized body**, never on the section name alone.
//! Two sections sharing a name but carrying genuinely different text — an MIT
//! body naming a different copyright holder, say — are different notices and
//! stay separate. Whitespace is the only difference this pass is allowed to
//! consider immaterial.
//!
//! MIT is deliberately untouched. Its 39 sections carry only four distinct
//! permission texts, but the rest of each body is the copyright line, which is
//! the attribution MIT exists to preserve. Collapsing that correctly means
//! restructuring the section — permission text once, every copyright notice
//! listed under it — which is a separate decision, not a typography fix.
//!
//! # Determinism
//!
//! CI regenerates and diffs, so the output must be byte-stable. The surviving
//! rendering of a merged group is the **lexicographically smallest** of its raw
//! bodies: a rule determined by the content itself, so it does not change when
//! an unrelated crate enters or leaves the dependency tree.

use std::path::Path;
use std::process::Command;

/// One rendered license section: the heading name, the `Used by:` bullet lines,
/// and the fenced license body.
struct Section {
    name: String,
    used_by: Vec<String>,
    body: String,
}

/// A parsed chunk of the generated document: a license section, or prose that is
/// reproduced untouched.
enum Item {
    PassThrough(String),
    Section(Section),
}

/// The output plan: prose in place, and each merge group emitted at the position
/// of its first member so merging never reorders the document.
enum Emit {
    PassThrough(String),
    Group(usize),
}

/// Run the generator and write the merged file to `THIRD-PARTY-LICENSES.md`.
///
/// # Errors
/// When `cargo about` is missing or fails, when its output cannot be parsed as
/// the expected section shape, or when the file cannot be written.
pub fn run() -> Result<(), String> {
    let root = crate::corpus::repo_root();
    let raw = generate(&root)?;
    let merged = merge(&raw)?;
    let out = root.join("THIRD-PARTY-LICENSES.md");
    std::fs::write(&out, &merged).map_err(|e| format!("writing {}: {e}", out.display()))?;
    eprintln!("wrote {} ({} bytes)", out.display(), merged.len());
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

/// Collapse whitespace so two renderings of the same license compare equal.
/// Every run of whitespace is removed rather than folded to one space: line
/// wrapping moves breaks *inside* a sentence, so a fold to single spaces would
/// still see `licenses/\nLICENSE` and `licenses/ LICENSE` as different.
fn normalize(body: &str) -> String {
    body.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Merge sections whose bodies normalize equal, preserving everything else.
///
/// # Errors
/// When the generator's output does not have the expected `## <name>` /
/// `Used by:` / fenced-body shape.
pub fn merge(raw: &str) -> Result<String, String> {
    // A chunk carrying no fenced body is prose, not a license section — the
    // `Overview` heading is one, and the template may grow others. Those pass
    // through verbatim, in place, so the document's shape is the generator's and
    // only license sections are ever touched.
    let mut items: Vec<Item> = Vec::new();
    let mut first = true;
    for chunk in raw.split("\n## ") {
        if first {
            first = false;
            items.push(Item::PassThrough(chunk.to_owned()));
            continue;
        }
        if chunk.contains("```text\n") {
            items.push(Item::Section(parse_section(chunk)?));
        } else {
            items.push(Item::PassThrough(format!("## {chunk}")));
        }
    }
    if !items.iter().any(|i| matches!(i, Item::Section(_))) {
        return Err("no license section (a `## ` heading with a ```text body) in the generated file".to_owned());
    }

    // Group by (name, normalized body). A group is emitted at the position of its
    // FIRST member, so merging never reorders the document.
    let mut groups: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new();
    let mut order: Vec<Emit> = Vec::new();
    for item in items {
        match item {
            Item::PassThrough(t) => order.push(Emit::PassThrough(t)),
            Item::Section(s) => {
                let key = normalize(&s.body);
                match groups.iter().position(|(n, k, _, _)| *n == s.name && *k == key) {
                    Some(i) => {
                        let (_, _, used, bodies) = &mut groups[i];
                        for u in s.used_by {
                            if !used.contains(&u) {
                                used.push(u);
                            }
                        }
                        bodies.push(s.body);
                    }
                    None => {
                        order.push(Emit::Group(groups.len()));
                        groups.push((s.name, key, s.used_by, vec![s.body]));
                    }
                }
            }
        }
    }

    // `split` consumed the newline before each `## `, so items are re-joined with
    // one. Every item ends with exactly one newline, which is what makes a second
    // pass over this output reproduce it byte for byte (the CI drift guard runs
    // generate → merge → diff, so a non-idempotent pass would fail forever).
    let mut rendered: Vec<String> = Vec::new();
    for e in order {
        match e {
            Emit::PassThrough(t) => rendered.push(t),
            Emit::Group(i) => {
                let (name, _, used, bodies) = &mut groups[i];
                used.sort();
                used.dedup();
                // Content-determined survivor: stable against unrelated churn.
                let body = bodies.iter().min().expect("a group always has one body");
                let mut s = format!("## {name}\n\nUsed by:\n\n");
                for u in used.iter() {
                    s.push_str(u);
                    s.push('\n');
                }
                s.push_str(&format!("\n```text\n{body}\n```\n"));
                rendered.push(s);
            }
        }
    }
    Ok(rendered.join("\n"))
}

/// Parse one section body (already stripped of its leading `## `).
fn parse_section(chunk: &str) -> Result<Section, String> {
    let mut lines = chunk.lines();
    let name = lines.next().ok_or_else(|| "section with no heading".to_owned())?.trim().to_owned();
    let open = chunk
        .find("```text\n")
        .ok_or_else(|| format!("section `{name}` has no ```text body"))?;
    let after = open + "```text\n".len();
    let close = chunk[after..]
        .find("\n```")
        .ok_or_else(|| format!("section `{name}` has an unterminated body"))?;
    let body = chunk[after..after + close].to_owned();
    let used_by = chunk[..open].lines().filter(|l| l.starts_with("- ")).map(str::to_owned).collect();
    Ok(Section { name, used_by, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(sections: &str) -> String {
        format!("# Third-Party Licenses\n\npreamble\n\n## Overview\n\n- X (1)\n{sections}")
    }

    #[test]
    fn typographic_variants_of_one_license_merge() {
        // The measured Apache-2.0 case: same text, centred vs left-aligned.
        let raw = doc(concat!(
            "\n## Apache License 2.0\n\nUsed by:\n\n- b 1.0\n\n```text\n   Apache License\n   Version 2.0\n```\n",
            "\n## Apache License 2.0\n\nUsed by:\n\n- a 1.0\n\n```text\nApache License\nVersion 2.0\n```\n",
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## Apache License 2.0").count(), 1, "one section survives");
        assert!(out.contains("- a 1.0") && out.contains("- b 1.0"), "both crates are still credited");
        // The lexicographically smallest raw body wins — content-determined, so
        // the choice does not move when an unrelated crate joins or leaves. Here
        // leading spaces sort below `A`, which keeps the indented rendering: the
        // one upstream publishes and the one this repo's own LICENSE carries.
        assert!(out.contains("   Apache License\n   Version 2.0"));
        assert_eq!(out.matches("Apache License").count(), 2, "the heading and one body, not two bodies");
    }

    #[test]
    fn different_text_under_one_name_stays_separate() {
        // The MIT shape: same heading, different copyright holder. Merging these
        // would delete an attribution, which is the one thing MIT requires.
        let raw = doc(concat!(
            "\n## MIT License\n\nUsed by:\n\n- a 1.0\n\n```text\nCopyright (c) Alice\nPermission granted\n```\n",
            "\n## MIT License\n\nUsed by:\n\n- b 1.0\n\n```text\nCopyright (c) Bob\nPermission granted\n```\n",
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## MIT License").count(), 2, "distinct notices are not merged");
        assert!(out.contains("Alice") && out.contains("Bob"));
    }

    #[test]
    fn merging_is_idempotent() {
        // CI regenerates and diffs, so a second pass must be a no-op.
        let raw = doc(concat!(
            "\n## ISC License\n\nUsed by:\n\n- b 1.0\n\n```text\n  ISC text\n```\n",
            "\n## ISC License\n\nUsed by:\n\n- a 1.0\n\n```text\nISC text\n```\n",
        ));
        let once = merge(&raw).unwrap();
        assert_eq!(merge(&once).unwrap(), once);
    }

    #[test]
    fn the_preamble_and_overview_pass_through() {
        // Prose carries no fenced body, so it is reproduced verbatim and in place
        // — the pass only ever rewrites license sections.
        let raw = doc("\n## Zlib License\n\nUsed by:\n\n- z 1.0\n\n```text\nzlib\n```\n");
        let out = merge(&raw).unwrap();
        assert!(out.starts_with("# Third-Party Licenses\n\npreamble\n"));
        assert!(out.contains("## Overview\n\n- X (1)\n"));
        assert!(out.contains("## Zlib License"));
    }
}
