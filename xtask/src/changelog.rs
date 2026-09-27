//! The changelog's two gates: one of each `###` heading per `CHANGELOG.md`
//! section, and a conforming entry in every `changelog.d/` fragment.
//!
//! The file merges by union (`.gitattributes`, #588): where two branches both
//! insert at one place, git keeps both sides instead of stopping. That is right
//! for bullets and wrong for headings, so two branches that each open the same
//! new `### Changed` under `## [Unreleased]` land as two copies of it, with no
//! conflict raised. It was repaired by hand three times (66d419ce, f5874d36,
//! 6bd8aac3) before this test made it loud (issue #777). The released sections
//! are held to the same rule; none of them has ever broken it.
//!
//! GitHub ignores the union merge when it decides whether a PR can merge, so
//! entries now land as fragments, `changelog.d/<section>/<slug>.md`, and only
//! release prep writes `[Unreleased]` (`changelog.d/README.md`). A fragment is
//! the entry itself, so its gate holds the entry grammar at landing. The
//! validator is pinned by inline examples, so the grammar holds while the
//! directory is empty, which it is right after a release.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// Each `###` heading repeated under one `##` section, as `section: heading`.
    fn repeated_headings(changelog: &str) -> Vec<String> {
        let mut repeated = Vec::new();
        let mut section = "";
        let mut seen: Vec<&str> = Vec::new();
        for line in changelog.lines().map(str::trim_end) {
            if line.starts_with("## ") {
                section = line;
                seen.clear();
            } else if line.starts_with("### ") {
                if seen.contains(&line) {
                    repeated.push(format!("{section}: {line}"));
                } else {
                    seen.push(line);
                }
            }
        }
        repeated
    }

    #[test]
    fn no_section_repeats_a_heading() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../CHANGELOG.md");
        let changelog =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(changelog.contains("\n## [Unreleased]\n"), "no `## [Unreleased]` section");
        let repeated = repeated_headings(&changelog);
        assert!(
            repeated.is_empty(),
            "CHANGELOG.md repeats a heading within one section, which is what the union merge \
             leaves when two branches open the same one; move the bullets under the first and \
             delete the second:\n  {}",
            repeated.join("\n  ")
        );
    }

    /// The shape 66d419ce repaired, beside the same heading in another section.
    #[test]
    fn repeated_headings_reads_each_section_apart() {
        let src = "## [Unreleased]\n\n### Changed\n\n- a\n\n### Changed\n\n- b\n\n\
                   ## [0.1.0] - 2026-07-25\n\n### Changed\n\n- c\n";
        assert_eq!(repeated_headings(src), ["## [Unreleased]: ### Changed"]);
    }

    /// Keep a Changelog 1.1.0's headings, lowercased: the section directories.
    const SECTIONS: [&str; 6] = ["added", "changed", "deprecated", "removed", "fixed", "security"];

    /// Why the fragment at `relative` (under `changelog.d/`, e.g.
    /// `fixed/my-branch.md`) does not conform, one reason per entry; empty when
    /// it does.
    fn fragment_offenses(relative: &str, content: &str) -> Vec<String> {
        let mut offenses = Vec::new();
        let segments: Vec<&str> = relative.split('/').collect();
        if segments.len() != 2 || !SECTIONS.contains(&segments[0]) {
            offenses.push(format!(
                "{relative}: expected <section>/<slug>.md, section one of {}",
                SECTIONS.join(", ")
            ));
        }
        let slug = segments.last().copied().unwrap_or_default();
        let stem = slug.strip_suffix(".md").unwrap_or_default();
        let slug_ok = stem.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c));
        if !slug_ok {
            offenses.push(format!("{relative}: the slug must be lowercase [a-z0-9._-] ending in .md"));
        }
        let mut lines = content.lines();
        let entry = lines.next().unwrap_or_default();
        let bold_lead = entry
            .strip_prefix("- **")
            .is_some_and(|rest| rest.find("**").is_some_and(|end| end > 0));
        if !bold_lead || entry.trim_end() != entry {
            offenses.push(format!(
                "{relative}: the first line must be one `- **Summary.** Detail.` bullet, on ONE line"
            ));
        }
        for line in lines {
            let child = line.strip_prefix("  - ").is_some_and(|rest| !rest.starts_with(' '));
            if !child || line.trim_end() != line {
                offenses.push(format!(
                    "{relative}: `{line}` — only `  - ` child items may follow the entry, each on one line"
                ));
            }
        }
        offenses
    }

    #[test]
    fn a_conforming_fragment_passes() {
        let entry = "- **A finding moved.** It moved because a rule changed.\n";
        assert!(fragment_offenses("fixed/my-branch.md", entry).is_empty());
        let with_child = format!("{entry}  - Measured on the public corpora.\n");
        assert!(fragment_offenses("added/issue-42.md", &with_child).is_empty());
    }

    #[test]
    fn a_wrapped_entry_or_a_blank_line_is_refused() {
        let wrapped = "- **A finding moved.** It moved\nbecause a rule changed.\n";
        assert!(!fragment_offenses("fixed/my-branch.md", wrapped).is_empty());
        let spaced = "- **A finding moved.**\n\n  - A child.\n";
        assert!(!fragment_offenses("fixed/my-branch.md", spaced).is_empty());
    }

    #[test]
    fn an_entry_without_a_bold_lead_is_refused() {
        assert!(!fragment_offenses("fixed/my-branch.md", "- A finding moved.\n").is_empty());
        assert!(!fragment_offenses("fixed/my-branch.md", "- **** Empty lead.\n").is_empty());
        assert!(!fragment_offenses("fixed/my-branch.md", "").is_empty());
    }

    #[test]
    fn an_unknown_section_a_top_level_file_or_an_uppercase_slug_is_refused() {
        let entry = "- **A finding moved.** Detail.\n";
        assert!(!fragment_offenses("performance/my-branch.md", entry).is_empty());
        assert!(!fragment_offenses("my-branch.md", entry).is_empty());
        assert!(!fragment_offenses("fixed/My-Branch.md", entry).is_empty());
        assert!(!fragment_offenses("fixed/my-branch.txt", entry).is_empty());
    }

    /// Every file under `dir`, recursively, as paths relative to `root`.
    fn files_under(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for entry in entries.map(|e| e.expect("a directory entry")) {
            let path = entry.path();
            if path.is_dir() {
                files_under(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).expect("under the root");
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_fragment_conforms() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../changelog.d");
        assert!(root.join("README.md").is_file(), "changelog.d/README.md keeps the directory");
        let mut files = Vec::new();
        files_under(&root, &root, &mut files);
        let offenses: Vec<String> = files
            .iter()
            .filter(|relative| relative.as_str() != "README.md")
            .flat_map(|relative| {
                let content = fs::read_to_string(root.join(relative))
                    .unwrap_or_else(|e| panic!("changelog.d/{relative}: {e}"));
                fragment_offenses(relative, &content)
            })
            .collect();
        assert!(
            offenses.is_empty(),
            "non-conforming changelog fragments (see changelog.d/README.md):\n  {}",
            offenses.join("\n  ")
        );
    }
}
