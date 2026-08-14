//! ADR-0079 / issue #180: a file that fails to parse is named loudly
//! (`syntax.unparsable`) and, unless it is vendor, dams the absence family.
//!
//! Before this slice, a file `php -l` rejects analysed to **exit 0 with no
//! diagnostic** — `SourceTree::parse` recovered silently and inference kept
//! emitting proof-grade findings from the recovered tree. This file measures both
//! fixes: (1) **the silence** (§2.1), one mechanics finding per broken file at the
//! first error, naming the count of further ones; (2) **the unsoundness**
//! (§2.2/§2.4/§2.5), where a non-vendor break is a [`DamKind::Unparsable`] site that
//! silences existence-absence project-wide, emits nothing else, and makes its
//! class-likes member-incomplete so method-absence ladders decline through them
//! (recovered declarations still count as **presence**, which can only silence).
//! §2.3's vendor presumption (ADR-0046 §2, carried over verbatim) is the sharp
//! edge: a broken `vendor/` file is not a dam site and its classes are not
//! member-incomplete — parser test suites ship deliberately broken PHP. Every
//! silence fixture below is paired against a negative control that fires without
//! the break, so nothing here proves a silence by over-silencing.

use std::collections::BTreeMap;

use steins_db::{GoverningRoot, PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::profile::{Level, ProfileConfigs, UserProfile};
use steins_infer::{
    ARRAY_DUPLICATE_KEY_ID, CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, CLASS_UNDEFINED_ID,
    DamKind, Diagnostic, FileUnit, Folder, SYNTAX_UNPARSABLE_ID, check_project, dam_facts,
};
use steins_syntax::SourceTree;

// Harness

/// Boot mock: absence family available (A9), no boot-surface homonyms — isolates
/// the parse-failure leg from every other ladder leg.
struct Boot;

impl Folder for Boot {
    fn fold(&mut self, _n: &str, _a: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

/// Build a real multi-file salsa project (fallback layout, so a `vendor/` path
/// component is what makes a file vendor) and return every finding.
fn findings(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs, ProjectLayout::fallback(), PluginFacts::none());
    check_project(&db, project, &mut Boot)
}

/// Just the findings carrying `id`.
fn of(files: &[(&str, &str)], id: &str) -> Vec<Diagnostic> {
    findings(files).into_iter().filter(|d| d.id == id).collect()
}

/// [`findings`] with a caller-supplied layout — issue #181: the vendor presumption
/// (§2.3) follows the SAME [`ProjectLayout::is_vendor`] answer as every other
/// consumer (declared `composer.json` vendor-dir or `steins.toml` vendor-dirs),
/// not a second, independently-literal `vendor` check.
fn findings_with_layout(files: &[(&str, &str)], layout: ProjectLayout) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs, layout, PluginFacts::none());
    check_project(&db, project, &mut Boot)
}

fn of_with_layout(files: &[(&str, &str)], layout: ProjectLayout, id: &str) -> Vec<Diagnostic> {
    findings_with_layout(files, layout).into_iter().filter(|d| d.id == id).collect()
}

/// A layout with one governing root at `/proj` whose declared vendor-dir is
/// `3rdparty` — the nextcloud shape (composer.json `config.vendor-dir`).
fn composer_layout_with_vendor_dir(name: &str) -> ProjectLayout {
    let dir = std::path::PathBuf::from("/proj");
    let root = GoverningRoot::new(dir.join("composer.json"), dir.clone(), vec![dir.join(name)], vec![]);
    ProjectLayout::new(dir, vec![root])
}

/// The breakage every fixture reuses: `$this->s->` with no member name (one parse
/// error, line 3 col 45); recovery keeps the class and both methods — it *looks*
/// complete, which is dangerous.
const BROKEN: &str = "<?php\nclass Q {\n  public function f(): void { if ($this->s->) {} }\n  public function known(): void {}\n}\n";

/// The same class, spelled so it parses. The negative control for every silence.
const SOUND: &str =
    "<?php\nclass Q {\n  public function f(): void {}\n  public function known(): void {}\n}\n";

// 1. The silence ends (§2.1): one finding per broken file, at the first error.

#[test]
fn a_project_that_parses_says_nothing_about_parsing() {
    // The negative control for the whole file: the id is not merely always-on.
    assert!(of(&[("src/q.php", SOUND)], SYNTAX_UNPARSABLE_ID).is_empty());
}

#[test]
fn fires_once_per_broken_file_at_the_first_error() {
    let d = of(&[("src/q.php", BROKEN)], SYNTAX_UNPARSABLE_ID);
    assert_eq!(d.len(), 1, "one finding per file, never one per error: {d:?}");
    assert_eq!(d[0].path, "src/q.php");
    assert_eq!((d[0].line, d[0].column), (3, 45), "positioned at the first parse error");
    assert!(d[0].message.contains("this file does not parse"), "{}", d[0].message);
    // The parser's own complaint is quoted rather than paraphrased.
    assert!(d[0].message.contains("found `RightParenthesis`"), "{}", d[0].message);
}

#[test]
fn a_single_error_claims_no_further_ones() {
    let d = of(&[("src/q.php", BROKEN)], SYNTAX_UNPARSABLE_ID);
    assert!(!d[0].message.contains("further parse error"), "{}", d[0].message);
}

#[test]
fn the_message_counts_the_further_errors() {
    // Three recovered errors — only the first gets a position; the rest are a
    // COUNT, since cascading recovery makes later positions a guess.
    let src = "<?php\nfunction a( int $x {\n}\nfunction b( int $y {\n}\n";
    let d = of(&[("src/fns.php", src)], SYNTAX_UNPARSABLE_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].line, d[0].column), (4, 1), "still the FIRST error's position");
    assert!(
        d[0].message.contains("(and 2 further parse errors in this file)"),
        "{}",
        d[0].message
    );
}

#[test]
fn each_broken_file_gets_its_own_finding() {
    let d = of(&[("src/a.php", BROKEN), ("src/b.php", BROKEN), ("src/c.php", SOUND)], SYNTAX_UNPARSABLE_ID);
    let mut paths: Vec<&str> = d.iter().map(|x| x.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["src/a.php", "src/b.php"], "{d:?}");
}

#[test]
fn the_message_names_the_project_wide_consequence_only_when_it_holds() {
    let project = of(&[("src/q.php", BROKEN)], SYNTAX_UNPARSABLE_ID);
    assert!(
        project[0].message.contains("no existence-absence claim can be proven"),
        "{}",
        project[0].message
    );
    // A vendor break dams nothing (§2.3), so the finding must not claim it does.
    let vendored = of(&[("vendor/pkg/q.php", BROKEN)], SYNTAX_UNPARSABLE_ID);
    assert!(
        !vendored[0].message.contains("no existence-absence claim can be proven"),
        "{}",
        vendored[0].message
    );
}

// 2. The dam (§2.2): a non-vendor break silences existence-absence project-wide.

#[test]
fn the_broken_file_is_a_dam_site_of_its_own_kind() {
    let tree = SourceTree::parse(BROKEN);
    let units = [FileUnit { path: "src/q.php", tree: &tree }];
    let facts = dam_facts(&units, &ProjectLayout::fallback());
    assert_eq!(facts.len(), 1, "{:?}", facts.sites());
    assert_eq!(facts.sites()[0].kind, DamKind::Unparsable);
    assert_eq!((facts.sites()[0].line, facts.sites()[0].column), (3, 45));
    assert!(facts.file_is_unparsable("src/q.php"));
    assert!(!facts.file_is_unparsable("src/other.php"));
}

#[test]
fn a_broken_non_vendor_file_silences_call_undefined_function() {
    let caller = ("src/main.php", "<?php\ntyop();\n");
    // Control: alone, the call is a proven fatal.
    assert_eq!(of(&[caller, ("src/q.php", SOUND)], CALL_UNDEFINED_FUNCTION_ID).len(), 1);
    // With the break beside it, the enumeration is unprovable — the recovery point
    // may have swallowed a `function tyop()` outright.
    assert!(of(&[caller, ("src/q.php", BROKEN)], CALL_UNDEFINED_FUNCTION_ID).is_empty());
}

#[test]
fn a_broken_non_vendor_file_silences_class_undefined() {
    let caller = ("src/main.php", "<?php\nnew Widget();\n");
    assert_eq!(of(&[caller, ("src/q.php", SOUND)], CLASS_UNDEFINED_ID).len(), 1);
    assert!(of(&[caller, ("src/q.php", BROKEN)], CLASS_UNDEFINED_ID).is_empty());
}

// 3. Member incompleteness (§2.5): method-absence ladders decline through its class-likes.

#[test]
fn method_absence_through_a_class_from_the_broken_file_is_silent() {
    // `Q` survives recovery with both methods, but a mangled body can have LOST one
    // (asymmetry per §1.2: `eval` can't reopen a class; a parse failure can hide members).
    let caller = ("src/main.php", "<?php\n(new Q())->missing();\n");
    assert_eq!(
        of(&[caller, ("src/q.php", SOUND)], CALL_UNDEFINED_METHOD_ID).len(),
        1,
        "control: the same class, spelled soundly, still fires"
    );
    assert!(of(&[caller, ("src/q.php", BROKEN)], CALL_UNDEFINED_METHOD_ID).is_empty());
}

#[test]
fn member_incompleteness_reaches_down_an_inheritance_chain() {
    // Leg is on the chain WALK, not the receiver: a sound subclass of a broken-file
    // parent inherits the incompleteness, since the missing method could be the parent's.
    let caller = ("src/main.php", "<?php\n(new Sub())->missing();\n");
    let sub = ("src/sub.php", "<?php\nclass Sub extends Q {}\n");
    assert_eq!(of(&[caller, sub, ("src/q.php", SOUND)], CALL_UNDEFINED_METHOD_ID).len(), 1);
    assert!(of(&[caller, sub, ("src/q.php", BROKEN)], CALL_UNDEFINED_METHOD_ID).is_empty());
}

#[test]
fn a_sound_class_beside_a_broken_file_keeps_its_method_absence() {
    // Dam is whole-universe for NAME EXISTENCE, but member incompleteness is
    // per-declaration (§2.5): a class in a sound file stays enumerable even beside a
    // broken sibling — otherwise the method family would go dark project-wide on
    // any single broken file.
    let d = of(
        &[
            ("src/main.php", "<?php\n(new Sound())->missing();\n"),
            ("src/sound.php", "<?php\nclass Sound { public function known(): void {} }\n"),
            ("src/q.php", BROKEN),
        ],
        CALL_UNDEFINED_METHOD_ID,
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

// 4. The broken file emits nothing else (§2.4).

#[test]
fn the_broken_file_emits_nothing_but_the_parse_finding() {
    // Carries a duplicate array key (mechanics, purely syntactic) and a call to an
    // undefined function — both would fire from a file that parses; from this one,
    // neither may: a finding built on a misparse is the manufactured-FP shape
    // ADR-0002 forbids.
    let rot = "$a = [1 => 'x', 1 => 'y'];\ntyop();\n";
    let sound_file = format!("<?php\n{rot}");
    let broken_file = format!("{BROKEN}{rot}");

    let control = findings(&[("src/q.php", sound_file.as_str())]);
    assert!(control.iter().any(|d| d.id == ARRAY_DUPLICATE_KEY_ID), "{control:?}");
    assert!(control.iter().any(|d| d.id == CALL_UNDEFINED_FUNCTION_ID), "{control:?}");

    let broken = findings(&[("src/q.php", broken_file.as_str())]);
    let ids: Vec<&str> = broken.iter().map(|d| d.id).collect();
    assert_eq!(ids, [SYNTAX_UNPARSABLE_ID], "{broken:?}");
}

// 5. Presence survives (§2.4): a recovered declaration can still SILENCE.

#[test]
fn a_class_half_recovered_from_a_broken_file_still_answers_a_new_elsewhere() {
    // Presence can only silence an absence claim, never fire one, so a
    // half-recovered declaration is safe and cross-file resolution keeps working.
    // Measured through a VENDOR break (no dam masks the answer): the silence here
    // is the index doing its job, not the dam.
    let caller = ("src/main.php", "<?php\nnew Q();\n");
    assert_eq!(
        of(&[caller], CLASS_UNDEFINED_ID).len(),
        1,
        "control: with no declaration anywhere, `new Q()` is a proven fatal"
    );
    assert!(
        of(&[caller, ("vendor/pkg/q.php", BROKEN)], CLASS_UNDEFINED_ID).is_empty(),
        "the recovered declaration is presence evidence and silences the claim"
    );
}

// 6. The vendor presumption (§2.3), carried over verbatim from ADR-0046 §2.

#[test]
fn a_broken_vendor_file_still_reports() {
    // Rides the CLI's ordinary vendor filter like any other finding — the id
    // itself is not vendor-aware.
    let d = of(&[("vendor/pkg/q.php", BROKEN)], SYNTAX_UNPARSABLE_ID);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].path, "vendor/pkg/q.php");
}

#[test]
fn a_broken_vendor_file_is_not_a_dam_site() {
    let tree = SourceTree::parse(BROKEN);
    let units = [FileUnit { path: "vendor/pkg/q.php", tree: &tree }];
    assert!(dam_facts(&units, &ProjectLayout::fallback()).is_clear());
    // …and so the existence family keeps its findings.
    let caller = ("src/main.php", "<?php\ntyop();\n");
    assert_eq!(of(&[caller, ("vendor/pkg/q.php", BROKEN)], CALL_UNDEFINED_FUNCTION_ID).len(), 1);
}

#[test]
fn a_broken_vendor_files_classes_are_not_member_incomplete() {
    // Recorded trade, not a proof: the same break that makes a first-party class
    // unenumerable is presumed harmless in `vendor/`; pinned so a §2.3 change is
    // deliberate.
    let d = of(
        &[("src/main.php", "<?php\n(new Q())->missing();\n"), ("vendor/pkg/q.php", BROKEN)],
        CALL_UNDEFINED_METHOD_ID,
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

// 6b. Issue #181: the presumption follows the RESOLVED vendor answer, not a literal check.

#[test]
fn the_presumption_follows_a_composer_declared_vendor_dir() {
    // `3rdparty/` isn't literally `vendor`; a bespoke literal check would miss it
    // and wrongly dam the project. Routed through `ProjectLayout::is_vendor` instead.
    let layout = composer_layout_with_vendor_dir("3rdparty");
    let tree = SourceTree::parse(BROKEN);
    let units = [FileUnit { path: "3rdparty/pkg/q.php", tree: &tree }];
    assert!(dam_facts(&units, &layout).is_clear(), "a declared vendor dir is not a dam site");

    let caller = ("src/main.php", "<?php\ntyop();\n");
    assert_eq!(
        of_with_layout(&[caller, ("3rdparty/pkg/q.php", BROKEN)], layout, CALL_UNDEFINED_FUNCTION_ID).len(),
        1,
        "the existence family keeps its findings"
    );
}

#[test]
fn the_presumption_still_applies_to_a_first_party_break_under_a_composer_project() {
    // Flip side: a break OUTSIDE the declared vendor dir still dams even with a
    // composer.json present — having a manifest isn't a free pass.
    let layout = composer_layout_with_vendor_dir("3rdparty");
    let caller = ("src/main.php", "<?php\ntyop();\n");
    assert!(
        of_with_layout(&[caller, ("src/q.php", BROKEN)], layout, CALL_UNDEFINED_FUNCTION_ID).is_empty(),
        "src/q.php is not under the declared vendor root, so it still dams"
    );
}

#[test]
fn the_presumption_follows_the_steins_toml_no_manifest_vendor_dirs() {
    // No composer.json at all: `steins.toml [paths] vendor-dirs` is the only
    // reason `3rdparty/` reads as vendor here, and the dam has to agree with it.
    let layout = ProjectLayout::fallback().with_extra_vendor_dirs(vec!["3rdparty".to_owned()]);
    let tree = SourceTree::parse(BROKEN);
    let units = [FileUnit { path: "3rdparty/pkg/q.php", tree: &tree }];
    assert!(dam_facts(&units, &layout).is_clear());

    let caller = ("src/main.php", "<?php\ntyop();\n");
    assert_eq!(
        of_with_layout(&[caller, ("3rdparty/pkg/q.php", BROKEN)], layout, CALL_UNDEFINED_FUNCTION_ID).len(),
        1
    );
}

// 7. Mechanics semantics: no profile can turn it off or demote it.

#[test]
fn no_profile_can_disable_or_demote_it() {
    // ADR-0050 §1 / diagnostic-policy.md: "no profile disables OR DEMOTES mechanics
    // ids" — a file that doesn't parse is apparatus rot, not a style opinion; a
    // profile demoting it to a warning would let a project go green over a file
    // `php -l` rejects.
    let mut m = BTreeMap::new();
    m.insert(
        "quiet".to_owned(),
        UserProfile {
            disable: vec![SYNTAX_UNPARSABLE_ID.to_owned(), "syntax.*".to_owned()],
            warn: vec![SYNTAX_UNPARSABLE_ID.to_owned(), "syntax.*".to_owned()],
            ..Default::default()
        },
    );
    let s = ProfileConfigs(m).resolve(Some("quiet")).unwrap();
    assert!(s.surfaces_id(SYNTAX_UNPARSABLE_ID), "disable cannot turn it off");
    assert_eq!(s.level(SYNTAX_UNPARSABLE_ID), Level::Fail, "warn cannot demote it");
    // And it is on the bare `steins check` surface (floor `Default`).
    let bare = ProfileConfigs(BTreeMap::new()).resolve(None).unwrap();
    assert!(bare.surfaces_id(SYNTAX_UNPARSABLE_ID));
    assert_eq!(bare.level(SYNTAX_UNPARSABLE_ID), Level::Fail);
}
