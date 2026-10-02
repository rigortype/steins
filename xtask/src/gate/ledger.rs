//! The local ledger: baselines for the private corpus, kept out of the tree.
//!
//! The tracked tables under `xtask/fp-gate/` describe what anyone can
//! reproduce — the pinned public corpus. A private project's counts and
//! triage pins describe code this repository must not name, and each reseed
//! of them (a count moved, a finding triaged, the reason written above the
//! row) is a ledger entry about that code. Those rows live in
//! `fp-gate.local.toml` at the repository root instead, gitignored like
//! `corpus.local.toml` beside it. The gate reads it when it is present and
//! merges it with the built-in tables, so a checkout without it (CI, a fresh
//! clone, an agent worktree until the file is copied in) runs on the public
//! baselines alone — which the report says, so a green gate cannot be mistaken
//! for one that held the private ledger.
//!
//! ```toml
//! # Pins: the same `[[finding]]` rows `expected_proof_findings.toml` takes.
//! [[finding]]
//! package = "<a corpus.local.toml project name>"
//! id = "variable.undefined"
//! path_suffix = "src/A.php"
//! line = 3
//! message_contains = "$x is never bound"
//!
//! # One table per count family, the same `"<name>" = <count>` rows the
//! # built-in files take. Each row keeps the comment block above it.
//! [phpdoc]
//! [throw]
//! [effect]
//! [possibly]
//! ```
//!
//! The rules, each of which refuses before any analysis runs:
//!
//! - **Local rows only.** A name in the overlay must be a project that
//!   `corpus.local.toml` lists. A public package's baseline is reviewed in the
//!   tracked file; letting it live in an untracked one would let it drift
//!   unseen, which is the property the tracked tables exist for.
//! - **One home per project.** A project name that has rows in a built-in table
//!   may not have rows in the same table of the overlay (and a package with
//!   built-in pins may not have overlay pins): a ledger split across two files
//!   reads as two answers.
//! - **Same validation.** Overlay pins go through the checks the built-in pins
//!   do, and an unknown field or a misspelt table is an error, not a row short
//!   of a field.
//! - **Absent is empty, unreadable is an error.** A missing file is the CI
//!   case and says nothing; a file that exists but cannot be read or parsed
//!   stops the gate, naming it.

use std::path::{Path, PathBuf};

use super::{BASELINE_DIR, Baselines, CountTable, ExpectedProofFinding, validate_pins};
use crate::corpus::repo_root;

/// The overlay's file name, at the repository root.
pub const OVERLAY_FILE: &str = "fp-gate.local.toml";

/// Where the overlay is looked for.
pub fn overlay_path() -> PathBuf { repo_root().join(OVERLAY_FILE) }

/// `fp-gate.local.toml`'s shape.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OverlayFile {
    #[serde(default)]
    finding: Vec<ExpectedProofFinding>,
    #[serde(default)]
    phpdoc: CountTable,
    #[serde(default)]
    throw: CountTable,
    #[serde(default)]
    effect: CountTable,
    #[serde(default)]
    possibly: CountTable,
}

/// Whether, and with how much, the overlay took part in this run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Overlay {
    /// No file: the built-in baselines alone.
    #[default]
    Absent,
    /// A file was read and merged; the counts are the rows it contributed.
    Loaded { pins: usize, phpdoc: usize, throw: usize, effect: usize, possibly: usize },
}

impl Overlay {
    /// The report line saying which of the two the run was.
    pub fn report_line(&self, local_projects: usize) -> String {
        match self {
            Overlay::Absent if local_projects == 0 => format!(
                "ledger: {OVERLAY_FILE} absent — built-in baselines only (no local projects in \
                 corpus.local.toml, so there is no private ledger to hold)."
            ),
            Overlay::Absent => format!(
                "ledger: {OVERLAY_FILE} ABSENT — built-in baselines only; the private-corpus pins \
                 and reseed ledger are NOT in force. {local_projects} local project(s) are \
                 measured without it, and one with no built-in row expects zero everywhere."
            ),
            Overlay::Loaded { pins, phpdoc, throw, effect, possibly } => format!(
                "ledger: {OVERLAY_FILE} loaded — {} row(s) merged with the built-in baselines \
                 ({pins} pin(s), {phpdoc} phpdoc, {throw} throw, {effect} effect, {possibly} \
                 possibly).",
                pins + phpdoc + throw + effect + possibly
            ),
        }
    }
}

/// Read the overlay's text. A missing file is `None` (the CI case); any other
/// failure is an error, because an overlay that exists and was skipped would be
/// the private ledger silently dropped.
pub fn read_overlay(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} cannot be read: {e}", path.display())),
    }
}

/// The names a merge checks the overlay against.
pub struct Roster<'a> {
    /// Every public corpus package.
    pub public: &'a [&'a str],
    /// Every project `corpus.local.toml` lists.
    pub local: &'a [&'a str],
}

impl Roster<'_> {
    /// Why `name` may not carry an overlay row, or `None` when it may.
    fn refusal(&self, name: &str, tracked_file: &str) -> Option<String> {
        if self.public.contains(&name) {
            Some(format!(
                "`{name}` is a public corpus package: its baseline belongs in \
                 {BASELINE_DIR}/{tracked_file}, where a change to it is reviewed"
            ))
        } else if !self.local.contains(&name) {
            Some(format!(
                "`{name}` is not a project listed in corpus.local.toml, so there is no local \
                 project for the row to describe"
            ))
        } else {
            None
        }
    }
}

/// Merge the overlay's text (if any) into the built-in `base`. Every refusal
/// names [`OVERLAY_FILE`] and, for a clash, the tracked file it clashes with.
pub fn merge(
    base: Baselines,
    text: Option<&str>,
    roster: &Roster<'_>,
) -> Result<Baselines, String> {
    let Some(text) = text else { return Ok(base) };
    let file: OverlayFile =
        toml::from_str(text).map_err(|e| format!("{OVERLAY_FILE} is malformed: {e}"))?;
    validate_pins(OVERLAY_FILE, &file.finding)?;

    let mut base = base;
    let summary = Overlay::Loaded {
        pins: file.finding.len(),
        phpdoc: file.phpdoc.len(),
        throw: file.throw.len(),
        effect: file.effect.len(),
        possibly: file.possibly.len(),
    };

    let pin_names: Vec<&str> = file.finding.iter().map(|p| p.package.as_str()).collect();
    for name in &pin_names {
        if let Some(why) = roster.refusal(name, "expected_proof_findings.toml") {
            return Err(format!("{OVERLAY_FILE}: a [[finding]] pin: {why}"));
        }
        if base.proof.iter().any(|p| p.package == *name) {
            return Err(clash(name, "[[finding]] pins", "expected_proof_findings.toml"));
        }
    }
    let tables = [
        ("phpdoc", "phpdoc_expected.toml", &mut base.phpdoc, file.phpdoc),
        ("throw", "throw_expected.toml", &mut base.throw, file.throw),
        ("effect", "effect_expected.toml", &mut base.effect, file.effect),
        ("possibly", "possibly_expected.toml", &mut base.possibly, file.possibly),
    ];
    for (table, tracked, into, from) in tables {
        for name in from.0.keys() {
            if let Some(why) = roster.refusal(name, tracked) {
                return Err(format!("{OVERLAY_FILE}: [{table}]: {why}"));
            }
            if into.0.contains_key(name) {
                return Err(clash(name, &format!("[{table}] rows"), tracked));
            }
        }
        into.0.extend(from.0);
    }
    base.proof.extend(file.finding);
    base.overlay = summary;
    Ok(base)
}

/// The error for a project name that has rows in both homes.
fn clash(name: &str, what: &str, tracked_file: &str) -> String {
    format!(
        "`{name}` has {what} in both {BASELINE_DIR}/{tracked_file} and {OVERLAY_FILE}: a \
         project's rows live in one of them, so move the other's"
    )
}

impl CountTable {
    fn len(&self) -> usize { self.0.len() }
}

#[cfg(test)]
mod tests {
    use super::{Overlay, Roster, merge, read_overlay};
    use crate::gate::{Baselines, parse_pins, parse_table};

    const PUBLIC: &[&str] = &["pub/pkg"];
    const LOCAL: &[&str] = &["local-a", "local-b"];

    fn roster() -> Roster<'static> { Roster { public: PUBLIC, local: LOCAL } }

    const PIN: &str = "[[finding]]\npackage = \"local-a\"\nid = \"variable.undefined\"\n\
                       path_suffix = \"src/A.php\"\nline = 3\n\
                       message_contains = \"$x is never bound\"\n";

    /// A built-in baseline small enough to reason about: a public package with
    /// a `phpdoc` row and a pin, and one local project (`local-b`) whose
    /// `throw` row is built in.
    fn base() -> Baselines {
        let pin = PIN.replace("local-a", "pub/pkg");
        Baselines {
            phpdoc: parse_table("t.toml", "\"pub/pkg\" = 2\n").unwrap(),
            throw: parse_table("t.toml", "\"local-b\" = 7\n").unwrap(),
            effect: parse_table("t.toml", "").unwrap(),
            possibly: parse_table("t.toml", "").unwrap(),
            proof: parse_pins("p.toml", &pin).unwrap(),
            overlay: Overlay::Absent,
        }
    }

    fn merged(text: &str) -> Result<Baselines, String> { merge(base(), Some(text), &roster()) }

    #[test]
    fn an_absent_overlay_leaves_the_built_in_tables_alone() {
        let b = merge(base(), None, &roster()).unwrap();
        assert_eq!((b.phpdoc.total(), b.throw.total(), b.proof.len()), (2, 7, 1));
        assert_eq!(b.overlay, Overlay::Absent);
        let none = std::env::temp_dir().join("steins-xtask-test-no-such-fp-gate-local.toml");
        assert_eq!(read_overlay(&none), Ok(None));
    }

    #[test]
    fn an_overlay_adds_its_rows_and_reports_how_many() {
        let text =
            format!("{PIN}\n[phpdoc]\n# why\n\"local-a\" = 5\n[possibly]\n\"local-a\" = 1\n");
        let b = merged(&text).unwrap();
        assert_eq!(b.phpdoc.expected("local-a"), 5);
        assert_eq!(b.phpdoc.expected("pub/pkg"), 2, "the built-in row is still there");
        assert_eq!(b.possibly.expected("local-a"), 1);
        assert_eq!(b.proof.len(), 2);
        assert_eq!(
            b.overlay,
            Overlay::Loaded { pins: 1, phpdoc: 1, throw: 0, effect: 0, possibly: 1 }
        );
        let line = b.overlay.report_line(2);
        assert!(line.contains("loaded") && line.contains("3 row(s)"), "{line}");
        assert!(line.contains("1 pin(s)") && line.contains("1 phpdoc"), "{line}");
    }

    #[test]
    fn the_report_line_tells_a_gate_without_the_ledger_from_one_with_it() {
        let absent = Overlay::Absent.report_line(1);
        assert!(absent.contains("ABSENT") && absent.contains("NOT in force"), "{absent}");
        assert!(absent.contains("1 local project(s)"), "{absent}");
        // Nothing local to hold a ledger for: still says so, without the alarm.
        let ci = Overlay::Absent.report_line(0);
        assert!(ci.contains("absent") && !ci.contains("NOT in force"), "{ci}");
    }

    #[test]
    fn a_project_with_rows_in_both_homes_is_refused_naming_both() {
        let err = merged("[throw]\n\"local-b\" = 9\n").unwrap_err();
        assert!(err.contains("`local-b`") && err.contains("[throw] rows"), "{err}");
        assert!(err.contains("xtask/fp-gate/throw_expected.toml"), "{err}");
        assert!(err.contains("fp-gate.local.toml"), "{err}");
        // The same project in a table where only one home has it is fine.
        assert!(merged("[phpdoc]\n\"local-b\" = 9\n").is_ok());
    }

    #[test]
    fn a_package_with_pins_in_both_homes_is_refused_naming_both() {
        let mut b = base();
        b.proof.extend(parse_pins("p.toml", PIN).unwrap());
        let err = merge(b, Some(PIN), &roster()).unwrap_err();
        assert!(err.contains("`local-a`") && err.contains("[[finding]] pins"), "{err}");
        assert!(err.contains("expected_proof_findings.toml"), "{err}");
        assert!(err.contains("fp-gate.local.toml"), "{err}");
    }

    #[test]
    fn a_public_package_may_not_carry_an_overlay_row() {
        let table = merged("[phpdoc]\n\"pub/pkg\" = 99\n").unwrap_err();
        assert!(table.contains("public corpus package"), "{table}");
        assert!(table.contains("xtask/fp-gate/phpdoc_expected.toml"), "{table}");
        let pin = merged(&PIN.replace("local-a", "pub/pkg")).unwrap_err();
        assert!(pin.contains("public corpus package"), "{pin}");
    }

    #[test]
    fn a_name_corpus_local_does_not_list_may_not_carry_an_overlay_row() {
        let err = merged("[effect]\n\"nowhere\" = 1\n").unwrap_err();
        assert!(err.contains("`nowhere`") && err.contains("corpus.local.toml"), "{err}");
        let pin = merged(&PIN.replace("local-a", "nowhere")).unwrap_err();
        assert!(pin.contains("corpus.local.toml"), "{pin}");
    }

    #[test]
    fn overlay_pins_are_validated_like_built_in_ones() {
        let twice = merged(&format!("{PIN}\n{PIN}")).unwrap_err();
        assert!(twice.contains("is pinned twice"), "{twice}");
        assert!(twice.starts_with("fp-gate.local.toml:"), "{twice}");
        let blank = merged(&PIN.replace("$x is never bound", "")).unwrap_err();
        assert!(blank.contains("needs a path suffix, a line and a message"), "{blank}");
    }

    #[test]
    fn a_malformed_overlay_is_an_error_naming_the_file() {
        for bad in [
            "[phpdoc\n",                      // not TOML
            "[phpdoc]\n\"local-a\" = \"x\"\n", // a count that is not a number
            "[phpdco]\n\"local-a\" = 1\n",     // a misspelt table
            &PIN.replace("path_suffix", "path_sufix"), // a misspelt pin field
        ] {
            let err = merged(bad).unwrap_err();
            assert!(err.starts_with("fp-gate.local.toml is malformed:"), "{bad:?}: {err}");
        }
    }
}
