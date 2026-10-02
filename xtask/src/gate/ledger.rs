//! The local ledger: baselines for the private corpus, kept out of the tree.
//!
//! The tracked tables under `xtask/fp-gate/` describe what anyone can
//! reproduce — the pinned public corpus. A private-corpus project's counts and
//! triage pins (a *private-corpus project* being a `corpus.local.toml` project
//! whose code is not public) describe code this repository must not name, and
//! each reseed of them (a count moved, a finding triaged, the reason written
//! above the row) is a ledger entry about that code. Those rows live in
//! `fp-gate.local.toml` at the repository root instead, gitignored like
//! `corpus.local.toml` beside it. The gate merges it with the built-in tables
//! at run time. `phpstan/phpstan-src` is listed in `corpus.local.toml` too but
//! is public code, so its rows stay in the tracked tables.
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
//! The rules. Every refusal happens before any analysis runs; one rule, about
//! unlisted names, is deliberately not a refusal:
//!
//! - **A listed local project needs the file.** When `corpus.local.toml` lists
//!   any project and the ledger is absent, the gate stops: a private project
//!   measured without its baselines expects zero everywhere, and a wall of
//!   "regressions" that are really a missing file is worse than no run. An
//!   *empty* `fp-gate.local.toml` is the explicit way to run with no local
//!   rows. With no `corpus.local.toml` project (CI, a fresh clone) an absent
//!   ledger is simply the public baselines, and the report says so.
//! - **No public package.** A row for a package in the pinned public corpus is
//!   refused: its baseline is reviewed in the tracked file, and letting it
//!   live in an untracked one would let it drift unseen, which is the property
//!   the tracked tables exist for.
//! - **Unlisted names are unused, not errors.** A row for a name
//!   `corpus.local.toml` does not list (a project removed from it, or one that
//!   exists only on another machine) matches no report row, so it never gates;
//!   the report line counts them so a typo is visible, and they are not merged,
//!   so they touch no table, total or section. A ledger none of whose rows
//!   applies to a listed project is reported as present but NOT applied.
//! - **One home per project.** A project name that has rows in a built-in table
//!   may not have rows in the same table of the overlay (and a package with
//!   built-in pins may not have overlay pins): a ledger split across two files
//!   reads as two answers.
//! - **Same validation.** Overlay pins go through the checks the built-in pins
//!   do, and an unknown field or a misspelt table is an error, not a row short
//!   of a field.
//! - **Unreadable is an error.** A file that exists but cannot be read or
//!   parsed stops the gate, naming it.

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
    /// No file, and no local project that needed one: the built-in baselines
    /// alone.
    #[default]
    Absent,
    /// A file was read and merged; the counts are the rows applied to listed
    /// projects, and `unlisted` the rows for a project `corpus.local.toml` does
    /// not list, which were left out.
    Loaded {
        pins: usize,
        phpdoc: usize,
        throw: usize,
        effect: usize,
        possibly: usize,
        unlisted: usize,
    },
}

impl Overlay {
    /// The report line saying which of the two the run was.
    pub fn report_line(&self) -> String {
        match self {
            Overlay::Absent => format!(
                "ledger: {OVERLAY_FILE} absent — built-in baselines only (corpus.local.toml lists \
                 no project, so there is no private ledger to hold)."
            ),
            Overlay::Loaded { pins, phpdoc, throw, effect, possibly, unlisted } => {
                let applied = pins + phpdoc + throw + effect + possibly;
                if applied == 0 && *unlisted > 0 {
                    return format!(
                        "ledger: {OVERLAY_FILE} present but NOT applied — corpus.local.toml lists \
                         none of its projects ({unlisted} row(s) unused); this run measured no \
                         private corpus."
                    );
                }
                let mut line = format!(
                    "ledger: {OVERLAY_FILE} loaded — {applied} row(s) applied on top of the \
                     built-in baselines ({pins} pin(s), {phpdoc} phpdoc, {throw} throw, \
                     {effect} effect, {possibly} possibly).",
                );
                if *unlisted > 0 {
                    line.push_str(&format!(
                        " {unlisted} row(s) for unlisted project(s), unused."
                    ));
                }
                line
            }
        }
    }
}

/// Read the overlay's text. A missing file is `None`; any other failure is an
/// error, because an overlay that exists and was skipped would be the private
/// ledger silently dropped.
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
        self.public.contains(&name).then(|| {
            format!(
                "`{name}` is a public corpus package: its baseline belongs in \
                 {BASELINE_DIR}/{tracked_file}, where a change to it is reviewed"
            )
        })
    }

    fn lists(&self, name: &str) -> bool { self.local.contains(&name) }
}

/// Merge the overlay's text (`None` when the file is absent) into the built-in
/// `base`. Every refusal names [`OVERLAY_FILE`] and, for a clash, the tracked
/// file it clashes with.
pub fn merge(
    base: Baselines,
    text: Option<&str>,
    roster: &Roster<'_>,
) -> Result<Baselines, String> {
    let Some(text) = text else {
        if roster.local.is_empty() {
            return Ok(base);
        }
        return Err(format!(
            "{OVERLAY_FILE} is absent, but corpus.local.toml lists {} project(s), and the gate \
             requires {OVERLAY_FILE} whenever local projects are listed: without it a private \
             project's baselines would be missing and every finding would read as a regression. \
             Restore the file, or create an empty one to run with no local rows.",
            roster.local.len()
        ));
    };
    let file: OverlayFile =
        toml::from_str(text).map_err(|e| format!("{OVERLAY_FILE} is malformed: {e}"))?;
    validate_pins(OVERLAY_FILE, &file.finding)?;

    let mut base = base;
    let mut unlisted = 0;

    let mut pins = 0;
    for p in &file.finding {
        let name = p.package.as_str();
        if let Some(why) = roster.refusal(name, "expected_proof_findings.toml") {
            return Err(format!("{OVERLAY_FILE}: a [[finding]] pin: {why}"));
        }
        if base.proof.iter().any(|b| b.package == name) {
            return Err(clash(name, "[[finding]] pins", "expected_proof_findings.toml"));
        }
        pins += usize::from(roster.lists(name));
    }
    unlisted += file.finding.len() - pins;
    base.proof.extend(file.finding.into_iter().filter(|p| roster.lists(&p.package)));

    let tables = [
        ("phpdoc", "phpdoc_expected.toml", &mut base.phpdoc, file.phpdoc),
        ("throw", "throw_expected.toml", &mut base.throw, file.throw),
        ("effect", "effect_expected.toml", &mut base.effect, file.effect),
        ("possibly", "possibly_expected.toml", &mut base.possibly, file.possibly),
    ];
    let mut applied = [0usize; 4];
    for (n, (table, tracked, into, from)) in tables.into_iter().enumerate() {
        for (name, count) in from.0 {
            if let Some(why) = roster.refusal(&name, tracked) {
                return Err(format!("{OVERLAY_FILE}: [{table}]: {why}"));
            }
            if into.0.contains_key(&name) {
                return Err(clash(&name, &format!("[{table}] rows"), tracked));
            }
            if roster.lists(&name) {
                into.0.insert(name, count);
                applied[n] += 1;
            } else {
                unlisted += 1;
            }
        }
    }
    let [phpdoc, throw, effect, possibly] = applied;
    base.overlay = Overlay::Loaded { pins, phpdoc, throw, effect, possibly, unlisted };
    Ok(base)
}

/// The error for a project name that has rows in both homes.
fn clash(name: &str, what: &str, tracked_file: &str) -> String {
    format!(
        "`{name}` has {what} in both {BASELINE_DIR}/{tracked_file} and {OVERLAY_FILE}: a \
         project's rows live in one of them, so move the other's"
    )
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
    /// a `phpdoc` row and a pin, and one listed project (`local-b`) whose
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
    fn an_absent_overlay_is_the_built_in_tables_when_no_local_project_needs_one() {
        // CI and a fresh clone: no corpus.local.toml project, no ledger, no fuss.
        let nobody = Roster { public: PUBLIC, local: &[] };
        let b = merge(base(), None, &nobody).unwrap();
        assert_eq!((b.phpdoc.total(), b.throw.total(), b.proof.len()), (2, 7, 1));
        assert_eq!(b.overlay, Overlay::Absent);
        let none = std::env::temp_dir().join("steins-xtask-test-no-such-fp-gate-local.toml");
        assert_eq!(read_overlay(&none), Ok(None));
    }

    #[test]
    fn an_absent_overlay_with_a_listed_local_project_is_refused() {
        let err = merge(base(), None, &roster()).unwrap_err();
        assert!(err.starts_with("fp-gate.local.toml is absent"), "{err}");
        assert!(err.contains("corpus.local.toml lists 2 project(s)"), "{err}");
        assert!(err.contains("empty one"), "{err}");
    }

    #[test]
    fn an_empty_overlay_is_the_explicit_way_to_run_with_no_local_rows() {
        let b = merged("").unwrap();
        assert_eq!((b.phpdoc.total(), b.throw.total(), b.proof.len()), (2, 7, 1));
        let zero =
            Overlay::Loaded { pins: 0, phpdoc: 0, throw: 0, effect: 0, possibly: 0, unlisted: 0 };
        assert_eq!(b.overlay, zero);
        assert!(b.overlay.report_line().contains("0 row(s)"));
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
            Overlay::Loaded { pins: 1, phpdoc: 1, throw: 0, effect: 0, possibly: 1, unlisted: 0 }
        );
        let line = b.overlay.report_line();
        assert!(line.contains("loaded") && line.contains("3 row(s)"), "{line}");
        assert!(line.contains("1 pin(s)") && line.contains("1 phpdoc"), "{line}");
    }

    #[test]
    fn the_report_line_tells_a_gate_without_the_ledger_from_one_with_it() {
        let absent = Overlay::Absent.report_line();
        assert!(absent.contains("absent") && absent.contains("built-in baselines only"), "{absent}");
        let loaded = merged("").unwrap().overlay.report_line();
        assert!(loaded.contains("loaded"), "{loaded}");
        assert!(!loaded.contains("unlisted"), "{loaded}");
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
    fn rows_for_an_unlisted_project_are_counted_and_left_out_of_the_tables() {
        let text = format!(
            "{}\n{PIN}\n[effect]\n\"nowhere\" = 1\n[possibly]\n\"local-a\" = 1\n",
            PIN.replace("local-a", "nowhere")
        );
        let b = merged(&text).unwrap();
        // The unlisted rows touch no table, no total, and so no section.
        assert_eq!(b.effect.expected("nowhere"), 0);
        assert!(b.effect.is_empty() && b.effect.total() == 0);
        assert_eq!(b.proof.len(), 2, "the built-in pin and the listed one; not the unlisted pin");
        assert!(b.proof.iter().all(|p| p.package != "nowhere"));
        assert_eq!(b.possibly.expected("local-a"), 1);
        assert_eq!(
            b.overlay,
            Overlay::Loaded { pins: 1, phpdoc: 0, throw: 0, effect: 0, possibly: 1, unlisted: 2 }
        );
        let line = b.overlay.report_line();
        assert!(line.contains("loaded") && line.contains("2 row(s) applied"), "{line}");
        assert!(line.contains("2 row(s) for unlisted project(s), unused"), "{line}");
        // The duplicate refusal still covers an unlisted name.
        let mut dup = base();
        dup.effect = parse_table("t.toml", "\"nowhere\" = 1\n").unwrap();
        assert!(merge(dup, Some("[effect]\n\"nowhere\" = 2\n"), &roster()).is_err());
    }

    #[test]
    fn a_ledger_applying_to_no_listed_project_says_it_was_not_applied() {
        let nobody = Roster { public: PUBLIC, local: &[] };
        let b = merge(base(), Some("[effect]\n\"nowhere\" = 1\n"), &nobody).unwrap();
        let line = b.overlay.report_line();
        assert!(line.contains("present but NOT applied"), "{line}");
        assert!(line.contains("no private corpus") && !line.contains("loaded"), "{line}");
        // A deliberately empty ledger is not that: nothing was left unapplied.
        let empty = merge(base(), Some(""), &nobody).unwrap().overlay.report_line();
        assert!(empty.contains("loaded") && empty.contains("0 row(s)"), "{empty}");
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
