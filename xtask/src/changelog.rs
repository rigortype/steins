//! One of each `###` heading per `CHANGELOG.md` section.
//!
//! The file merges by union (`.gitattributes`, #588): where two branches both
//! insert at one place, git keeps both sides instead of stopping. That is right
//! for bullets and wrong for headings, so two branches that each open the same
//! new `### Changed` under `## [Unreleased]` land as two copies of it, with no
//! conflict raised. It was repaired by hand three times (66d419ce, f5874d36,
//! 6bd8aac3) before this test made it loud (issue #777). The released sections
//! are held to the same rule; none of them has ever broken it.

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
}
