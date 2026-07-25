//! `cargo xtask licenses` — generate `THIRD-PARTY-LICENSES.md`, then merge the
//! sections whose license bodies differ only in typography (issue #43) and group
//! the ones that differ only in who holds the copyright (issue #45).
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
//! Merging is keyed on the **permission notice** — the part of the body that
//! grants the rights — never on the section name alone. Whitespace is immaterial
//! (that is the typography fix); a different grant is a different licence and
//! stays in its own section.
//!
//! The copyright line is the other half of the body, and it is the half that may
//! never be thrown away. MIT requires that "the above copyright notice **and**
//! this permission notice shall be included in all copies": the attribution is
//! the thing the licence exists to preserve, so a merge that keeps one holder's
//! line and drops thirty-five others is not a tidier notice file, it is an
//! incomplete one. Measured on this tree, the generator emits **39 MIT sections
//! carrying one permission notice** (nine renderings of it, identical once
//! whitespace is collapsed) and **36 distinct copyright notices**.
//!
//! So a group of sections that share a permission notice is rendered as one
//! section: every distinct copyright notice, then the permission notice once —
//! which is also how MIT's own sentence reads, the notices above and the
//! permission below. Nothing is dropped and nothing is reworded; what is lost is
//! only the mapping from one holder to the crates that ship it, and that mapping
//! is not what either clause requires. (It could not be kept anyway without
//! putting prose between the `Used by:` list and the fenced body, which is
//! exactly the region the parser below discards on a second pass — and a second
//! pass that discarded a copyright notice is the failure this whole design is
//! avoiding.)
//!
//! The grouping is not MIT-specific: any family whose bodies open with a
//! recognized permission notice ([`PERMISSION_OPENERS`]) is treated the same way,
//! so a second ISC or BSD holder entering the tree collapses too. MIT is simply
//! the only family with more than one holder today. A body with no recognized
//! opener is never split, and falls back to the whole-body keying.
//!
//! # Determinism
//!
//! CI regenerates and diffs, so the output must be byte-stable. The surviving
//! rendering of a merged group is the **lexicographically smallest** of its raw
//! bodies, and the copyright notices are listed in lexicographic order: rules
//! determined by the content itself, so they do not change when an unrelated
//! crate enters or leaves the dependency tree.
//!
//! Idempotence is a hard requirement (the CI guard is generate → merge → diff, so
//! a pass that is not a fixed point fails forever), and it comes from a
//! deliberate restraint: **a group is restructured only when it actually has more
//! than one copyright notice to preserve.** A group whose members all carry the
//! same notice — including a group of one, which is what a restructured section
//! is on the next pass — is emitted as its raw body, byte for byte. That keeps
//! every section this pass has no business touching identical to the generator's
//! own output.

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

/// One section's body, split into the two things a permissive licence requires
/// to travel together: the copyright notice and the permission notice.
struct Member {
    /// The body exactly as the generator rendered it — what is emitted verbatim
    /// whenever this pass has nothing to preserve by restructuring.
    body: String,
    /// Everything before the permission notice, newline-trimmed. Empty when the
    /// crate ships no copyright line, or when the body has no recognized opener
    /// (then `permission` is the whole body).
    notice: String,
    /// The permission notice and everything after it — the grouping key's source.
    permission: String,
}

/// A merge group: sections sharing a name and a permission notice.
struct Group {
    name: String,
    /// The normalized permission notice — what "these are the same licence" means.
    key: String,
    used_by: Vec<String>,
    members: Vec<Member>,
}

/// The first words of a permission notice, per licence family. A body is split at
/// the first line containing one of these, which puts the copyright line (and any
/// title line above it) in [`Member::notice`] and the grant in
/// [`Member::permission`].
///
/// The table is deliberately short and literal. A body that matches nothing is
/// not split — it keeps today's whole-body keying — so an unrecognized licence
/// degrades to the pre-#45 behaviour instead of being restructured on a guess.
/// Notice blocks that are *only* a licence title and therefore preserve no
/// attribution: one crate in this tree ships an MIT body headed `MIT License`
/// with no copyright line at all, and carrying that heading into a merged section
/// would put a bare `MIT License` between two real notices.
///
/// The match is against the whole trimmed block, never a prefix, so a title
/// sitting *above* a copyright line — which is how most crates render it — stays
/// with the notice it belongs to. Exact equality is what keeps this from ever
/// dropping an attribution.
const TITLE_ONLY_NOTICES: [&str; 3] = ["MIT License", "The MIT License", "The MIT License (MIT)"];

const PERMISSION_OPENERS: [&str; 3] = [
    // MIT, MIT-0, and Unicode-3.0's data-files grant.
    "Permission is hereby granted",
    // ISC (and the OpenBSD-style wording it comes from).
    "Permission to use, copy, modify",
    // BSD 2-Clause / 3-Clause.
    "Redistribution and use in source and binary forms",
];

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

    // Group by (name, normalized permission notice). A group is emitted at the
    // position of its FIRST member, so merging never reorders the document.
    let mut groups: Vec<Group> = Vec::new();
    let mut order: Vec<Emit> = Vec::new();
    for item in items {
        match item {
            Item::PassThrough(t) => order.push(Emit::PassThrough(t)),
            Item::Section(s) => {
                let (notice, permission) = split_notice(&s.body);
                let key = normalize(permission);
                let member = Member {
                    notice: notice.to_owned(),
                    permission: permission.to_owned(),
                    body: s.body,
                };
                match groups.iter().position(|g| g.name == s.name && g.key == key) {
                    Some(i) => {
                        let g = &mut groups[i];
                        for u in s.used_by {
                            if !g.used_by.contains(&u) {
                                g.used_by.push(u);
                            }
                        }
                        g.members.push(member);
                    }
                    None => {
                        order.push(Emit::Group(groups.len()));
                        groups.push(Group {
                            name: s.name,
                            key,
                            used_by: s.used_by,
                            members: vec![member],
                        });
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
                let g = &mut groups[i];
                g.used_by.sort();
                g.used_by.dedup();
                let body = render_body(&g.members);
                let mut s = format!("## {}\n\nUsed by:\n\n", g.name);
                for u in g.used_by.iter() {
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

/// Split a body into its copyright notice and its permission notice at the first
/// line that opens a grant ([`PERMISSION_OPENERS`]). Everything above the grant is
/// the notice — the copyright line, and any title line or provenance note sitting
/// with it, kept verbatim because guessing which of those lines "is" the
/// attribution is not this pass's business.
///
/// A body with no recognized opener yields an empty notice and itself as the
/// permission text, which reproduces the pre-#45 whole-body keying exactly.
fn split_notice(body: &str) -> (&str, &str) {
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        if PERMISSION_OPENERS.iter().any(|opener| line.contains(opener)) {
            return (body[..offset].trim_matches('\n'), &body[offset..]);
        }
        offset += line.len();
    }
    ("", body)
}

/// Render one group's fenced body.
///
/// The default is the pre-#45 rendering: the lexicographically smallest raw body,
/// byte for byte. Restructuring happens **only** when that would lose something —
/// when a member carries a copyright notice the chosen body does not — and then
/// the body becomes every distinct notice, in lexicographic order, followed by the
/// permission notice once.
///
/// That restraint is what makes the pass a fixed point: a restructured section is
/// a group of one on the next run, its members all carry the same (whole) notice
/// block, and it is emitted verbatim.
fn render_body(members: &[Member]) -> String {
    let chosen = members.iter().map(|m| &m.body).min().expect("a group always has one member");
    let chosen_notice =
        members.iter().find(|m| m.body == *chosen).map(|m| normalize(&m.notice)).unwrap_or_default();
    if members.iter().all(|m| normalize(&m.notice) == chosen_notice) {
        return chosen.clone();
    }

    // Distinct notices: keyed on the normalized text, so two renderings of one
    // holder's line count once, and the smallest raw rendering is the one kept
    // (the same content-determined rule the body choice uses).
    let mut keyed: Vec<(String, &str)> = members
        .iter()
        .map(|m| m.notice.trim())
        .filter(|n| !n.is_empty() && !TITLE_ONLY_NOTICES.contains(n))
        .map(|n| (normalize(n), n))
        .collect();
    keyed.sort();
    keyed.dedup_by(|a, b| a.0 == b.0);
    let mut notices: Vec<&str> = keyed.into_iter().map(|(_, raw)| raw).collect();
    notices.sort_unstable();
    if notices.is_empty() {
        // Nothing to preserve after all (every member's notice was a bare title):
        // fall back to the untouched body rather than emitting a stray blank.
        return chosen.clone();
    }

    let permission =
        members.iter().map(|m| m.permission.as_str()).min().expect("a group always has one member");
    format!("{}\n\n{permission}", notices.join("\n\n"))
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

    /// An MIT-shaped section: one holder, the real opener, one crate.
    fn mit(krate: &str, notice: &str) -> String {
        format!(
            "\n## MIT License\n\nUsed by:\n\n- {krate} 1.0\n\n```text\n{notice}\n\n\
             Permission is hereby granted, free of charge, to any person obtaining a copy\n\
             of this software, subject to the following conditions: the above copyright\n\
             notice and this permission notice shall be included in all copies.\n```\n"
        )
    }

    #[test]
    fn one_permission_notice_carries_every_copyright_notice() {
        // Issue #45: the same grant with a different holder is one licence with
        // several attributions, not several licences.
        let raw = doc(&format!(
            "{}{}{}",
            mit("a", "Copyright (c) Alice"),
            mit("b", "Copyright (c) Bob"),
            mit("c", "Copyright (c) Carol")
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## MIT License").count(), 1, "one grant, one section");
        assert_eq!(
            out.matches("Permission is hereby granted").count(),
            1,
            "the permission notice appears once"
        );
        for holder in ["Alice", "Bob", "Carol"] {
            assert!(out.contains(holder), "{holder}'s notice was dropped:\n{out}");
        }
        for krate in ["- a 1.0", "- b 1.0", "- c 1.0"] {
            assert!(out.contains(krate), "{krate} lost its credit:\n{out}");
        }
    }

    #[test]
    fn no_copyright_notice_is_dropped() {
        // The property the whole issue turns on, checked as a set comparison
        // rather than by reading the output: every `Copyright` line that went in
        // comes out. Deleting a holder to save a line is the one unacceptable
        // outcome, so it is pinned rather than reviewed.
        let holders: Vec<String> =
            (0..40).map(|i| format!("Copyright (c) 20{i:02} Holder Number {i}")).collect();
        let sections: String =
            holders.iter().enumerate().map(|(i, h)| mit(&format!("c{i}"), h)).collect();
        let out = merge(&doc(&sections)).unwrap();
        let survivors: std::collections::BTreeSet<&str> =
            out.lines().filter(|l| l.starts_with("Copyright (c) 20")).collect();
        let expected: std::collections::BTreeSet<&str> =
            holders.iter().map(String::as_str).collect();
        assert_eq!(survivors, expected, "a copyright notice was lost or invented");
        assert_eq!(out.matches("## MIT License").count(), 1);
    }

    #[test]
    fn genuinely_different_permission_texts_stay_separate() {
        // Grouping is on the grant itself: two different grants are two licences,
        // however alike their headings. (`\u{2014}` here stands in for any wording
        // difference the collapsed-whitespace comparison can see.)
        let a = mit("a", "Copyright (c) Alice");
        let b = mit("b", "Copyright (c) Bob").replace("subject to the", "subject only to the");
        let out = merge(&doc(&format!("{a}{b}"))).unwrap();
        assert_eq!(out.matches("## MIT License").count(), 2, "different grants stay apart");
        assert!(out.contains("Alice") && out.contains("Bob"));
    }

    #[test]
    fn a_body_with_no_recognized_grant_keeps_whole_body_keying() {
        // The fallback: nothing in `PERMISSION_OPENERS` matches, so the body is
        // never split and the pre-#45 behaviour stands — two holders, two
        // sections, no restructuring on a guess.
        let raw = doc(concat!(
            "\n## Weird License\n\nUsed by:\n\n- a 1.0\n\n```text\nCopyright (c) Alice\nYou may do things\n```\n",
            "\n## Weird License\n\nUsed by:\n\n- b 1.0\n\n```text\nCopyright (c) Bob\nYou may do things\n```\n",
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## Weird License").count(), 2);
        assert!(out.contains("Alice") && out.contains("Bob"));
    }

    #[test]
    fn one_holder_in_two_renderings_is_still_one_notice() {
        // The ISC shape: the same holder, wrapped differently by two crates. That
        // is a typography difference on both halves of the body, so the section is
        // emitted untouched rather than growing a duplicated notice.
        let raw = doc(concat!(
            "\n## ISC License\n\nUsed by:\n\n- b 1.0\n\n```text\n  Copyright (c) Zoe\n\n  Permission to use, copy, modify this thing\n```\n",
            "\n## ISC License\n\nUsed by:\n\n- a 1.0\n\n```text\nCopyright (c) Zoe\n\nPermission to use, copy, modify this thing\n```\n",
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## ISC License").count(), 1);
        assert_eq!(out.matches("Copyright (c) Zoe").count(), 1, "one holder, one notice");
        assert!(out.contains("- a 1.0") && out.contains("- b 1.0"));
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
    fn grouping_by_notice_is_idempotent() {
        // The restructured section must be a fixed point too: it re-parses as one
        // member whose notice block is all of the notices, and is re-emitted
        // verbatim. A pass that is not a fixed point fails CI's generate → merge →
        // diff guard forever, and the failure mode here would be dropping a holder
        // on the second run.
        let raw = doc(&format!(
            "{}{}{}",
            mit("a", "Copyright (c) Alice"),
            mit("b", "Copyright (c) Bob"),
            mit("c", "MIT License\n\nCopyright (c) Carol")
        ));
        let once = merge(&raw).unwrap();
        let twice = merge(&once).unwrap();
        assert_eq!(twice, once, "the second pass changed the document");
        for holder in ["Alice", "Bob", "Carol"] {
            assert!(twice.contains(holder), "{holder} vanished on the second pass");
        }
    }

    #[test]
    fn a_bare_title_is_not_carried_as_a_notice() {
        // One crate in the real tree ships an MIT body headed `MIT License` with no
        // copyright line. That heading preserves no attribution, and carrying it
        // would put a bare `MIT License` between two real notices.
        let raw = doc(&format!(
            "{}{}",
            mit("a", "Copyright (c) Alice"),
            mit("b", "MIT License")
        ));
        let out = merge(&raw).unwrap();
        assert_eq!(out.matches("## MIT License").count(), 1);
        let body = out.split("```text").nth(1).expect("a fenced body");
        assert!(body.contains("Copyright (c) Alice"));
        assert!(!body.contains("\nMIT License\n"), "a bare title rode along:\n{body}");
    }

    // ------------------------------------------------------- the real tree ---

    fn committed_notices() -> String {
        let path = crate::corpus::repo_root().join("THIRD-PARTY-LICENSES.md");
        std::fs::read_to_string(&path).expect("THIRD-PARTY-LICENSES.md is committed")
    }

    #[test]
    fn the_committed_file_is_a_fixed_point() {
        // Idempotence verified against the real tree, not only against fixtures:
        // CI runs generate → merge → diff, and the committed file is the merged
        // output, so merging it again must change nothing.
        let committed = committed_notices();
        assert_eq!(merge(&committed).unwrap(), committed, "the committed file is not a fixed point");
    }

    #[test]
    fn the_committed_file_keeps_every_mit_holder_under_one_grant() {
        // The measured outcome of #45 on this tree: one MIT section, one permission
        // notice, and the copyright notices of every crate that shares it.
        let committed = committed_notices();
        assert_eq!(committed.matches("\n## MIT License").count(), 1, "MIT is one section");
        let mit_section = committed
            .split("\n## MIT License")
            .nth(1)
            .and_then(|s| s.split("\n## ").next())
            .expect("the MIT section");
        assert_eq!(
            mit_section.matches("Permission is hereby granted").count(),
            1,
            "one permission notice"
        );
        let notices = mit_section.lines().filter(|l| l.starts_with("Copyright")).count();
        assert!(notices >= 30, "expected the holders to survive, found {notices}");
        // Named holders rather than a count alone: a stub that dropped the notices
        // and kept the number would pass a count check.
        for holder in [
            "Copyright (c) 2014 Alex Crichton",
            "Copyright (c) 2019 The Crossbeam Project Developers",
            "Copyright 2018 Developers of the Rand project",
        ] {
            assert!(mit_section.contains(holder), "missing `{holder}`");
        }
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
