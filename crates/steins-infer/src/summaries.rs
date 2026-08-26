//! The `summaries` section (ADR-0092 §5, issue #489 slice B): the per-file
//! walk block a warm run replays instead of walking, and the two stamps that
//! say when replaying it is licensed.
//!
//! Issue #487 pinned `symbols`, `contracts` and `trace` and left this section
//! deliberately open, "issue #489 owns that schema". It lives in `steins-infer`
//! rather than beside the other three in `steins-db::persist` because its
//! payload is this crate's vocabulary end to end — [`Diagnostic`], the
//! [`DIAGNOSTIC_IDS`] registry, [`Facet`], [`Fix`] — and `steins-db` neither
//! knows nor should know any of it. That is the same line the `sources` section
//! already draws (it lives with the orchestrator that reads it); the house
//! pattern the payload follows — [`steins_db::wire`] inside the section,
//! strict inverses, every decode failure a [`Miss`] the caller degrades to a
//! walk — is `steins-db::persist`'s, unchanged (issue #504 swapped both at
//! once; the section is 1.4% of the artifact, and one codec across every typed
//! payload is one fewer thing to remember).
//!
//! **What a row holds, and why that exactly.** A replayed file's diagnostics
//! must be byte-identical to a walked file's, every field of every
//! [`Diagnostic`] included, so the row stores the findings themselves — never
//! anything a reader would have to reconstruct a message from. The one thing
//! stored beside them is the file's `uncovered_matches` entry (ADR-0088 §5),
//! which the walk produces and `throw_diagnostics` consumes at the end of the
//! run; `None` distinguishes "made no entry" (an unparsable file, whose block
//! does not run) from "made an empty one", because the two are different keys
//! in that map. The file's content fingerprint rides along so the next run can
//! tell which files changed *within* a changed package.
//!
//! **Two stamps, not one.** A tree fingerprint licenses loading a parse,
//! because parsing is a pure function of bytes. Replaying a *finding* needs
//! more: every non-package input of the generation identity (`stamp`) and every
//! whole-universe verdict a walk can read (`universe`). Slice A's identity
//! under-coverage was harmless while findings were never loaded and stops being
//! harmless here, which is exactly what the issue's closing note asks to be
//! re-audited — the answer is that the replay gate is the *whole* identity, not
//! the package half of it.
//!
//! **Interning is a gate, not a convenience.** `Diagnostic::id` and
//! `Fix::title` are `&'static str`, so decoding one means finding it in a
//! compiled-in table. A spelling outside the table is a [`Miss`] and the file
//! walks — so a new id or a new fix title that nobody remembered to register
//! here costs a walk, never a wrong finding.

use serde::{Deserialize, Serialize};
use steins_gen::{ArtifactBuilder, ArtifactReader, Fingerprint, Miss, SectionName};

use crate::project::{Diagnostic, Fix, FixEdit};
use crate::suppress::{FACET_ORIGIN, Facet, Origin};
use crate::walk_plan::FileWalk;
use crate::{DIAGNOSTIC_IDS, REGISTERED_NOT_YET_EMITTED};

/// The section holding the package's per-file walk blocks: the two stamps and
/// one row per file.
pub const SUMMARIES_SECTION: &str = "summaries";

/// [`SUMMARIES_SECTION`] as a validated [`SectionName`].
#[must_use]
pub fn summaries_section() -> SectionName {
    SectionName::new(SUMMARIES_SECTION).expect("the summaries section name is valid")
}

/// Every [`Fix::title`] an emitter can produce, for the decoder to intern
/// against. One entry today — ADR-0010's dump removal is the only fix-it v1
/// ships — and `a_fix_title_outside_the_table_is_a_miss` pins what happens to
/// a title that is not here: the package's summaries miss and every one of its
/// files walks. Cost, never meaning; adding a fix means adding its title.
const FIX_TITLES: &[&str] = &["remove the dump statement"];

// ---------------------------------------------------------------------------
// The wire form.
// ---------------------------------------------------------------------------

/// The whole section: the two stamps, then the rows in file-slot order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummariesPayload {
    /// Hex of the run-identity stamp — every generation input but the
    /// per-package source fingerprints (which the `sources` section already
    /// gates, per package).
    stamp: String,
    /// Hex of the whole-universe verdict digest.
    universe: String,
    files: Vec<StoredFile>,
}

/// One file's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFile {
    /// The file's diagnostic path — the lookup key, matching the `trace`
    /// section's.
    path: String,
    /// The file's universe slot when the row was written.
    slot: usize,
    /// Hex of the file's own content fingerprint
    /// ([`steins_gen::SourceEntry::content`]), so a warm run can tell which
    /// files of a changed package actually changed.
    content: String,
    /// Everything the file's walk block appended, in the order it appended it.
    diagnostics: Vec<StoredDiagnostic>,
    /// The file's `uncovered_matches` entry (ADR-0088 §5), sorted; `null` for a
    /// file whose block never ran and so made no entry at all.
    uncovered: Option<Vec<u32>>,
}

/// One finding, every field of it. Nothing here is derived at decode time: a
/// replayed diagnostic that recomputed its own message would be a different
/// design with a different oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDiagnostic {
    id: String,
    path: String,
    line: u32,
    column: u32,
    message: String,
    facet: Option<StoredFacet>,
    fix: Option<StoredFix>,
}

/// The ADR-0050 §4 facet as its two wire spellings, decoded strictly back into
/// the closed [`Facet`] vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFacet {
    key: String,
    value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFix {
    title: String,
    edits: Vec<StoredEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEdit {
    path: String,
    start: u32,
    end: u32,
    replacement: String,
}

// ---------------------------------------------------------------------------
// Writing.
// ---------------------------------------------------------------------------

/// One file's row on its way in: what the ledger recorded, plus the identity
/// the next run compares against.
pub(crate) struct SummaryRow<'a> {
    pub(crate) path: &'a str,
    pub(crate) slot: usize,
    pub(crate) content: Fingerprint,
    pub(crate) walk: &'a FileWalk,
}

/// Serialize the section: the two stamps, then one row per file in the order
/// given.
pub(crate) fn summaries_payload(
    stamp: &Fingerprint,
    universe: &Fingerprint,
    rows: &[SummaryRow<'_>],
) -> Vec<u8> {
    let payload = SummariesPayload {
        stamp: stamp.to_hex(),
        universe: universe.to_hex(),
        files: rows
            .iter()
            .map(|row| StoredFile {
                path: row.path.to_owned(),
                slot: row.slot,
                content: row.content.to_hex(),
                diagnostics: row.walk.diagnostics.iter().map(store_diagnostic).collect(),
                uncovered: row.walk.uncovered.clone(),
            })
            .collect(),
    };
    // Infallible: every field is a string, a number, or a vector of those.
    steins_db::wire::to_vec(&payload).expect("a summaries payload serializes")
}

/// Add the section to a builder under construction.
pub(crate) fn write_summaries(
    builder: &mut ArtifactBuilder,
    stamp: &Fingerprint,
    universe: &Fingerprint,
    rows: &[SummaryRow<'_>],
) {
    builder
        .section(summaries_section(), summaries_payload(stamp, universe, rows))
        .expect("distinct section names");
}

fn store_diagnostic(d: &Diagnostic) -> StoredDiagnostic {
    StoredDiagnostic {
        id: d.id.to_owned(),
        path: d.path.clone(),
        line: d.line,
        column: d.column,
        message: d.message.clone(),
        facet: d.facet.map(|f| StoredFacet {
            key: f.key().to_owned(),
            value: f.value().to_owned(),
        }),
        fix: d.fix.as_ref().map(|fix| StoredFix {
            title: fix.title.to_owned(),
            edits: fix
                .edits
                .iter()
                .map(|e| StoredEdit {
                    path: e.path.clone(),
                    start: e.start,
                    end: e.end,
                    replacement: e.replacement.clone(),
                })
                .collect(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Reading.
// ---------------------------------------------------------------------------

/// One package's decoded summaries: the stamps to compare and the rows to
/// replay, keyed by diagnostic path.
pub(crate) struct Summaries {
    stamp: Fingerprint,
    universe: Fingerprint,
    files: Vec<(String, Fingerprint, FileWalk)>,
}

impl Summaries {
    /// Whether this section was written under the same run identity and the
    /// same whole-universe verdicts as the run now asking. Both must hold
    /// before any row of it may be replayed — the tree fingerprint the
    /// `sources` section gates is about parsing, and says nothing about what a
    /// walk would find.
    pub(crate) fn licensed_by(&self, stamp: &Fingerprint, universe: &Fingerprint) -> bool {
        &self.stamp == stamp && &self.universe == universe
    }

    /// Every row, `(path, content fingerprint, block)`.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (&str, &Fingerprint, &FileWalk)> {
        self.files.iter().map(|(path, content, walk)| (path.as_str(), content, walk))
    }
}

/// Decode the `summaries` section. Every way the bytes can be wrong — an
/// absent section, bytes that are not a summary set, a stamp that is not a
/// fingerprint, a diagnostic id or fix title
/// outside the compiled-in tables, a facet outside the closed vocabulary — is
/// a [`Miss`], which the orchestrator degrades to walking that package.
pub(crate) fn read_summaries(reader: &mut ArtifactReader) -> Result<Summaries, Miss> {
    let corrupt = || Miss::Corrupt("summaries section is not a summary set");
    let bytes = reader.section(&summaries_section())?;
    let payload: SummariesPayload = steins_db::wire::from_slice(&bytes).map_err(|_| corrupt())?;
    let stamp = Fingerprint::from_hex(&payload.stamp).ok_or_else(corrupt)?;
    let universe = Fingerprint::from_hex(&payload.universe).ok_or_else(corrupt)?;
    let mut files = Vec::with_capacity(payload.files.len());
    for file in payload.files {
        let content = Fingerprint::from_hex(&file.content).ok_or_else(corrupt)?;
        let mut diagnostics = Vec::with_capacity(file.diagnostics.len());
        for d in file.diagnostics {
            diagnostics.push(load_diagnostic(d)?);
        }
        let mut uncovered = file.uncovered;
        // The canonical form the walk records; a row that arrived any other
        // way would compare unequal to a fresh walk under the verifier for no
        // reason of meaning.
        if let Some(spans) = &mut uncovered {
            spans.sort_unstable();
            spans.dedup();
        }
        files.push((file.path, content, FileWalk { diagnostics, uncovered }));
    }
    Ok(Summaries { stamp, universe, files })
}

fn load_diagnostic(d: StoredDiagnostic) -> Result<Diagnostic, Miss> {
    Ok(Diagnostic {
        id: intern_id(&d.id).ok_or(Miss::Corrupt("summaries name an unregistered diagnostic id"))?,
        path: d.path,
        line: d.line,
        column: d.column,
        message: d.message,
        facet: d.facet.map(load_facet).transpose()?,
        fix: d.fix.map(load_fix).transpose()?,
    })
}

/// The registry spelling of `id` as a `&'static str`, or `None` for an id this
/// binary does not know. Both registry lists are searched: an id may be
/// registered ahead of its emitter, and the artifact's writer is a binary of
/// the same version as its reader (the analyzer version is in the stamp), so
/// this only ever fails on doctored bytes.
fn intern_id(id: &str) -> Option<&'static str> {
    DIAGNOSTIC_IDS
        .iter()
        .chain(REGISTERED_NOT_YET_EMITTED.iter())
        .find(|known| **known == id)
        .copied()
}

fn load_facet(f: StoredFacet) -> Result<Facet, Miss> {
    let bad = Miss::Corrupt("summaries name a facet outside the registry vocabulary");
    if f.key != FACET_ORIGIN {
        return Err(bad);
    }
    match f.value.as_str() {
        v if v == Origin::Direct.as_str() => Ok(Facet::Origin(Origin::Direct)),
        v if v == Origin::Propagated.as_str() => Ok(Facet::Origin(Origin::Propagated)),
        _ => Err(bad),
    }
}

fn load_fix(fix: StoredFix) -> Result<Fix, Miss> {
    let title = FIX_TITLES
        .iter()
        .find(|known| ***known == *fix.title)
        .copied()
        .ok_or(Miss::Corrupt("summaries name an unregistered fix title"))?;
    Ok(Fix {
        title,
        edits: fix
            .edits
            .into_iter()
            .map(|e| FixEdit {
                path: e.path,
                start: e.start,
                end: e.end,
                replacement: e.replacement,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use steins_gen::DecodeBudget;

    use super::*;

    fn fp(tag: &str) -> Fingerprint {
        Fingerprint::of_bytes("test", tag.as_bytes())
    }

    fn diagnostic() -> Diagnostic {
        Diagnostic {
            id: crate::DEBUG_TYPE_ID,
            path: "src/app.php".to_owned(),
            line: 3,
            column: 7,
            message: "a message with \"quotes\", a \u{1f600} and a \\ backslash".to_owned(),
            facet: Some(Facet::Origin(Origin::Propagated)),
            fix: Some(Fix {
                title: "remove the dump statement",
                edits: vec![FixEdit {
                    path: "src/app.php".to_owned(),
                    start: 10,
                    end: 24,
                    replacement: String::new(),
                }],
            }),
        }
    }

    fn round_trip(bytes: Vec<u8>) -> Result<Summaries, Miss> {
        let dir = std::env::temp_dir().join(format!(
            "steins-summaries-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pkg");
        let mut builder = ArtifactBuilder::new();
        builder.section(summaries_section(), bytes).unwrap();
        builder.write_to(&path).unwrap();
        let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
        let out = read_summaries(&mut reader);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// The strict inverse: every field of every finding survives the disk
    /// boundary, including the two additive payloads (`facet`, `fix`) and the
    /// `None`/`Some(empty)` distinction the `uncovered_matches` key depends on.
    #[test]
    fn a_summary_set_round_trips_through_its_section() {
        let with_findings =
            FileWalk { diagnostics: vec![diagnostic()], uncovered: Some(vec![4, 9]) };
        let empty_entry = FileWalk { diagnostics: Vec::new(), uncovered: Some(Vec::new()) };
        let no_entry = FileWalk { diagnostics: Vec::new(), uncovered: None };
        let rows = vec![
            SummaryRow { path: "src/app.php", slot: 0, content: fp("a"), walk: &with_findings },
            SummaryRow { path: "src/b.php", slot: 1, content: fp("b"), walk: &empty_entry },
            SummaryRow { path: "src/broken.php", slot: 2, content: fp("c"), walk: &no_entry },
        ];
        let bytes = summaries_payload(&fp("stamp"), &fp("universe"), &rows);
        let decoded = round_trip(bytes).expect("the section decodes");

        assert!(decoded.licensed_by(&fp("stamp"), &fp("universe")));
        assert!(!decoded.licensed_by(&fp("other"), &fp("universe")), "a moved stamp refuses");
        assert!(!decoded.licensed_by(&fp("stamp"), &fp("other")), "a moved verdict refuses");

        let read: Vec<(&str, &FileWalk)> =
            decoded.rows().map(|(path, _, walk)| (path, walk)).collect();
        assert_eq!(read, vec![
            ("src/app.php", &with_findings),
            ("src/b.php", &empty_entry),
            ("src/broken.php", &no_entry),
        ]);
        let contents: Vec<&Fingerprint> = decoded.rows().map(|(_, c, _)| c).collect();
        assert_eq!(contents, vec![&fp("a"), &fp("b"), &fp("c")]);
    }

    /// Every way the bytes can be wrong is a `Miss` — never a panic, never a
    /// partial value. The two interning legs are the point: an id or a fix
    /// title this binary does not know costs the package's walk.
    #[test]
    fn a_doctored_summaries_section_is_a_miss() {
        let ok = |body: &str| format!(r#"{{"stamp": "{}", "universe": "{}", "files": [{body}]}}"#, fp("s").to_hex(), fp("u").to_hex());
        let row = |diagnostics: &str| {
            format!(
                r#"{{"path": "a.php", "slot": 0, "content": "{}", "diagnostics": [{diagnostics}], "uncovered": null}}"#,
                fp("c").to_hex()
            )
        };
        let diagnostic = |id: &str, facet: &str, fix: &str| {
            format!(
                r#"{{"id": "{id}", "path": "a.php", "line": 1, "column": 1, "message": "m", "facet": {facet}, "fix": {fix}}}"#
            )
        };
        let cases: Vec<(&str, String)> = vec![
            ("not-json", "}".to_owned()),
            ("wrong-shape", "[1, 2]".to_owned()),
            (
                "extra-field",
                format!(r#"{{"stamp": "{}", "universe": "{}", "files": [], "extra": 1}}"#, fp("s").to_hex(), fp("u").to_hex()),
            ),
            ("stamp-not-a-fingerprint", ok("").replace(&fp("s").to_hex(), "zz")),
            (
                "unregistered-id",
                ok(&row(&diagnostic("no.such-id", "null", "null"))),
            ),
            (
                "facet-outside-the-vocabulary",
                ok(&row(&diagnostic(crate::THROW_UNDECLARED_ID, r#"{"key": "origin", "value": "sideways"}"#, "null"))),
            ),
            (
                "facet-key-outside-the-vocabulary",
                ok(&row(&diagnostic(crate::THROW_UNDECLARED_ID, r#"{"key": "colour", "value": "direct"}"#, "null"))),
            ),
            (
                "unregistered-fix-title",
                ok(&row(&diagnostic(crate::DEBUG_TYPE_ID, "null", r#"{"title": "reformat the universe", "edits": []}"#))),
            ),
            (
                "row-extra-field",
                ok(&format!(
                    r#"{{"path": "a.php", "slot": 0, "content": "{}", "diagnostics": [], "uncovered": null, "extra": 1}}"#,
                    fp("c").to_hex()
                )),
            ),
        ];
        for (tag, body) in cases {
            assert!(round_trip(body.into_bytes()).is_err(), "{tag} decoded when it should miss");
        }
    }

    /// The absent section is a `Miss` too — the shape a package artifact
    /// written before this section existed takes, and the one every artifact
    /// takes when the schema version has not yet obsoleted it.
    #[test]
    fn an_absent_summaries_section_is_a_miss() {
        let dir = std::env::temp_dir().join(format!("steins-summaries-absent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pkg");
        ArtifactBuilder::new().write_to(&path).unwrap();
        let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
        assert!(read_summaries(&mut reader).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every title an emitter can produce is in the interning table. The test
    /// is the enforcement: the fix machinery is small enough to enumerate, and
    /// a title missing here is a silent cost regression rather than a loud one.
    #[test]
    fn every_emittable_fix_title_is_internable() {
        for title in FIX_TITLES {
            assert!(
                load_fix(StoredFix { title: (*title).to_owned(), edits: Vec::new() }).is_ok(),
                "{title}"
            );
        }
        assert!(load_fix(StoredFix { title: "unknown".to_owned(), edits: Vec::new() }).is_err());
    }
}
