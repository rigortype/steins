//! `preg.invalid-pattern` (ADR-0078, issue #189) — a `preg_*` call whose pattern is
//! a proven literal that the project's OWN PCRE refuses to compile.
//!
//! The refusal is never Steins' own reading of the pattern: the pattern reader
//! (`steins_catalog::preg`, #148/#149/#156/#168/#177) only decides the pattern IS
//! a proven literal worth asking about; the answer comes from the engine through
//! the sidecar (ADR-0004). Two halves:
//!
//! * **live** — a real `php` on `PATH` answering a real `preg_compile`, the only
//!   thing that proves the id fires at all (skipped with a marker when `php` is
//!   absent, the convention `sidecar_recovery.rs` set);
//! * **mocked** — a [`Pcre`] folder standing in for the engine, for postures and
//!   entry-point coverage that must be pinned deterministically (as
//!   `offset_family.rs` mocks the boot surface).

use std::cell::RefCell;

use steins_infer::{
    Diagnostic, Folder, PREG_INVALID_PATTERN_ID, SidecarFolder, check_full, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

// The mocked engine.

/// A boot surface that is live and monkey-patch-free, with a stand-in PCRE: any
/// pattern containing an unbalanced `(` is "refused", everything else compiles.
/// Deliberately crude — real PCRE's verdicts are the live half's business; this
/// pins the plumbing between the fold gate, the gates, and the emitter.
#[derive(Default)]
struct Pcre {
    /// Every pattern the check actually asked about, in order — the dedupe witness.
    asked: RefCell<Vec<String>>,
}

impl Folder for Pcre {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn preg_pattern_refusal(&mut self, pattern: &str) -> Option<String> {
        self.asked.borrow_mut().push(pattern.to_owned());
        let opens = pattern.matches('(').count();
        let closes = pattern.matches(')').count();
        (opens != closes).then(|| {
            "Compilation failed: missing closing parenthesis at offset 9".to_owned()
        })
    }
}

/// The sound subset: no engine answers, so nothing may be claimed — even though
/// this folder would happily name a refusal if asked. Pins that the gate is the
/// *availability*, not the absence of an answer.
struct NoBootSurface;

impl Folder for NoBootSurface {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        false
    }
    fn preg_pattern_refusal(&mut self, _pattern: &str) -> Option<String> {
        Some("Compilation failed: missing closing parenthesis at offset 9".to_owned())
    }
}

fn findings(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == PREG_INVALID_PATTERN_ID)
        .collect()
}

fn mocked(src: &str) -> Vec<Diagnostic> {
    findings(src, &mut Pcre::default())
}

// Live: the project's own PCRE answers.

/// Spawn a real folder, or print a skip marker. The probe is a pattern the snippets
/// never use, so the per-run dedupe memo cannot answer a later question from it.
fn live_or_skip(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.preg_pattern_refusal("/(probe/").is_none() {
        eprintln!("SKIP {test}: no PHP engine answered preg_compile — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

/// The acceptance criterion, end to end on the real engine: PCRE's own compile
/// message reaches the finding.
#[test]
fn fires_on_a_real_pcre_refusal() {
    let Some(mut folder) = live_or_skip("fires_on_a_real_pcre_refusal") else { return };
    let d = findings(
        "<?php\nfunction f(string $s): void {\n    preg_match('/(unclosed/', $s);\n}\n",
        &mut folder,
    );
    assert_eq!(d.len(), 1, "the refused pattern fires exactly once: {d:#?}");
    assert_eq!(d[0].line, 3, "{d:#?}");
    // PCRE's own words, not a paraphrase.
    assert!(
        d[0].message.contains("Compilation failed: missing closing parenthesis"),
        "{}",
        d[0].message
    );
    // The site's own function name, as PHP itself would print it.
    assert!(d[0].message.contains("preg_match():"), "{}", d[0].message);
    assert!(d[0].message.contains("returns false"), "{}", d[0].message);
}

/// The delimiter and modifier legs — the parts of a pattern the reader already
/// handles, and therefore the parts a reader-only check would be most tempted to
/// judge itself.
#[test]
fn fires_on_real_delimiter_and_modifier_refusals() {
    let Some(mut folder) = live_or_skip("fires_on_real_delimiter_and_modifier_refusals") else {
        return;
    };
    for (src, needle) in [
        ("<?php\npreg_match('nodelim', $s);\n", "Delimiter must not be alphanumeric"),
        ("<?php\npreg_match('/a/Z', $s);\n", "Unknown modifier"),
    ] {
        let d = findings(src, &mut folder);
        assert_eq!(d.len(), 1, "{src} → {d:#?}");
        assert!(d[0].message.contains(needle), "{}", d[0].message);
    }
}

/// A valid pattern is silent — including the delimiters and modifiers the reader
/// handles, which is the acceptance criterion's own wording.
#[test]
fn silent_on_valid_patterns_including_exotic_delimiters() {
    let Some(mut folder) = live_or_skip("silent_on_valid_patterns") else { return };
    for src in [
        "<?php\npreg_match('/\\d+/', $s);\n",
        "<?php\npreg_match('~(a)(b)?~iu', $s);\n",
        "<?php\npreg_match_all('#[[:alpha:]]+#', $s, $m);\n",
        "<?php\npreg_split('/,\\s*/', $s);\n",
        "<?php\npreg_replace(['/a/', '/b/'], 'z', $s);\n",
    ] {
        assert!(findings(src, &mut folder).is_empty(), "{src} must be silent");
    }
}

/// `--no-php` is the sound subset: nothing is asked, so nothing is claimed.
#[test]
fn silent_under_no_php() {
    let mut folder = SidecarFolder::new(true);
    let d = findings("<?php\npreg_match('/(unclosed/', $s);\n", &mut folder);
    assert!(d.is_empty(), "--no-php never spawns PHP, so the check is silent: {d:#?}");
}

// Gates.

/// The sound subset via the availability lever itself (ADR-0049 A9 / ADR-0004): a
/// folder that WOULD name a refusal still reports nothing while the boot surface
/// is unavailable or monkey-patched.
#[test]
fn silent_without_a_live_boot_surface() {
    let d = findings("<?php\npreg_match('/(unclosed/', $s);\n", &mut NoBootSurface);
    assert!(d.is_empty(), "no legitimate boot surface ⇒ silence: {d:#?}");
}

/// Under `warning-handler = "null"` the application tolerates the warning, so
/// the finding leaves the proof surface — wired as `offset.missing` is (ADR-0049 §7).
#[test]
fn warning_handler_null_silences_the_finding() {
    let tree = SourceTree::parse("<?php\npreg_match('/(unclosed/', $s);\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Pcre::default(), false)
        .into_iter()
        .filter(|d| d.id == PREG_INVALID_PATTERN_ID)
        .collect();
    assert!(d.is_empty(), "\"null\" posture demotes the warning-grade finding: {d:#?}");
}

#[test]
fn warning_handler_abort_emits() {
    let tree = SourceTree::parse("<?php\npreg_match('/(unclosed/', $s);\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Pcre::default(), true)
        .into_iter()
        .filter(|d| d.id == PREG_INVALID_PATTERN_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}

// The fold gate: only a PROVEN literal is asked about.

#[test]
fn silent_on_an_unproven_pattern() {
    // A parameter is not a proven value — nothing to ask PCRE about.
    let d = mocked("<?php\nfunction f(string $p, string $s): void {\n    preg_match($p, $s);\n}\n");
    assert!(d.is_empty(), "an unproven pattern expression is silence: {d:#?}");
}

#[test]
fn fires_through_a_variable_bound_to_the_pattern() {
    // The same fold-gate fact the capture-group reader consumes: a variable bound to
    // a proven literal IS the literal.
    let d = mocked("<?php\n$re = '/(unclosed/';\npreg_match($re, $s);\n");
    assert_eq!(d.len(), 1, "an env-resolved pattern is proven: {d:#?}");
}

/// Nothing is asked about a pattern that is not proven — the request budget is not
/// spent on unknowns, and no answer is invented for them.
#[test]
fn an_unproven_pattern_is_never_sent_to_the_engine() {
    let mut folder = Pcre::default();
    let tree = SourceTree::parse("<?php\nfunction f(string $p): void {\n    preg_match($p, 'x');\n}\n");
    let _ = check_with(&tree, &[], "test.php", &mut folder);
    assert!(folder.asked.borrow().is_empty(), "asked: {:?}", folder.asked.borrow());
}

/// One question per distinct pattern per run, however many sites spell it.
#[test]
fn identical_patterns_are_asked_once_per_run() {
    let mut folder = SidecarFolder::enabled();
    if folder.preg_pattern_refusal("/(probe/").is_none() {
        eprintln!("SKIP identical_patterns_are_asked_once_per_run: is `php` on PATH?");
        return;
    }
    // The memo lives on the folder, so a second spelling of the same pattern is
    // answered without a round trip — and both sites still report.
    let d = findings(
        "<?php\npreg_match('/(unclosed/', $a);\npreg_match('/(unclosed/', $b);\n",
        &mut folder,
    );
    assert_eq!(d.len(), 2, "every site reports, even though the engine was asked once: {d:#?}");
}

// Entry-point coverage.

/// Every `preg_*` entry point that takes a pattern in argument 0 fires, and the
/// message states that entry point's own refusal value (`false` for the matchers and
/// splitters, `null` for the replacers — measured at 8.5.9).
#[test]
fn every_pattern_taking_entry_point_is_covered() {
    for (src, ret) in [
        ("<?php\npreg_match('/(x/', $s);\n", "false"),
        ("<?php\npreg_match_all('/(x/', $s, $m);\n", "false"),
        ("<?php\npreg_split('/(x/', $s);\n", "false"),
        ("<?php\npreg_grep('/(x/', $a);\n", "false"),
        ("<?php\npreg_replace('/(x/', 'z', $s);\n", "null"),
        ("<?php\npreg_replace_callback('/(x/', $cb, $s);\n", "null"),
        ("<?php\npreg_filter('/(x/', 'z', $s);\n", "null"),
        ("<?php\npreg_replace_callback_array(['/(x/' => $cb], $s);\n", "null"),
    ] {
        let d = mocked(src);
        assert_eq!(d.len(), 1, "{src} must fire: {d:#?}");
        assert!(d[0].message.contains(&format!("returns {ret}")), "{src} → {}", d[0].message);
    }
}

/// The idiom the id exists for: the pattern sits in a guard condition, which is not
/// a statement position at all.
#[test]
fn fires_at_guard_position() {
    let d = mocked("<?php\nif (preg_match('/(unclosed/', $s)) {\n    echo 'hit';\n}\n");
    assert_eq!(d.len(), 1, "a guard-position call is judged once: {d:#?}");
    assert_eq!(d[0].line, 2, "{d:#?}");
}

#[test]
fn fires_at_guard_position_through_negation_and_conjunction() {
    let d = mocked("<?php\nfunction f($s) {\n    if ($s && !preg_match('/(unclosed/', $s)) {\n        return 1;\n    }\n    return 0;\n}\n");
    assert_eq!(d.len(), 1, "{d:#?}");
}

/// An `elseif` chain recurses through `walk_if`, so each link is judged exactly
/// once — not zero times, and not once per branch clone.
#[test]
fn fires_once_per_elseif_link() {
    let d = mocked(
        "<?php\nfunction f($s) {\n    if (rand()) {\n        return 0;\n    } elseif (preg_match('/(unclosed/', $s)) {\n        return 1;\n    }\n    return 2;\n}\n",
    );
    assert_eq!(d.len(), 1, "exactly one finding for the elseif guard: {d:#?}");
}

/// `preg_quote` takes the text to escape, not a pattern — the recorded omission.
#[test]
fn silent_on_preg_quote() {
    assert!(mocked("<?php\n$q = preg_quote('/(x/');\n").is_empty());
}

// The array pattern form.

#[test]
fn the_array_form_fires_on_the_bad_element_only() {
    let d = mocked("<?php\npreg_replace(['/ok/', '/(bad/'], 'z', $s);\n");
    assert_eq!(d.len(), 1, "only the refused element reports: {d:#?}");
    assert!(d[0].message.contains("'/(bad/'"), "{}", d[0].message);
}

/// The partial array: `resolve_literal` is all-or-nothing, so the whole argument
/// resolves to nothing — yet the literal element is still refused, unproven sibling silent.
#[test]
fn the_array_form_reports_the_literal_beside_an_unproven_element() {
    let d = mocked("<?php\nfunction f($dyn, $s) {\n    return preg_replace(['/(bad/', $dyn], 'z', $s);\n}\n");
    assert_eq!(d.len(), 1, "the proven element fires, the unproven one is silent: {d:#?}");
    assert!(d[0].message.contains("'/(bad/'"), "{}", d[0].message);
}

#[test]
fn the_array_form_is_silent_when_every_element_is_unproven() {
    let d = mocked("<?php\nfunction f($a, $b, $s) {\n    return preg_replace([$a, $b], 'z', $s);\n}\n");
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn two_bad_elements_report_twice() {
    let d = mocked("<?php\npreg_replace(['/(one/', '/(two/'], 'z', $s);\n");
    assert_eq!(d.len(), 2, "{d:#?}");
    assert!(d.iter().any(|d| d.message.contains("'/(one/'")), "{d:#?}");
    assert!(d.iter().any(|d| d.message.contains("'/(two/'")), "{d:#?}");
}

/// A `preg_replace_callback_array` key that isn't a string key can't be a pattern
/// (opens with a non-alphanumeric delimiter, never integer-like) — nothing to claim.
#[test]
fn callback_array_ignores_non_string_keys() {
    assert!(mocked("<?php\npreg_replace_callback_array([$cb], $s);\n").is_empty());
}

// Argument-shape conservatism.

/// A named `pattern:` argument defeats the positional mapping this check reasons
/// with, so the call is skipped — the same conservatism the out-param seed applies.
#[test]
fn silent_on_a_named_pattern_argument() {
    let d = mocked("<?php\npreg_match(pattern: '/(unclosed/', subject: $s);\n");
    assert!(d.is_empty(), "a named argument is not read: {d:#?}");
}

/// A first-class callable builds a Closure and compiles no pattern.
#[test]
fn silent_on_a_first_class_callable() {
    assert!(mocked("<?php\n$f = preg_match(...);\n").is_empty());
}

/// A project function of the same simple name is a DIFFERENT function; asking PCRE
/// about its first argument would be a claim about code we did not analyze.
///
/// **Both names below are load-bearing, and the second is the one that was
/// broken.** The shadow question used to be asked through a resolution whose
/// notion of a known builtin is the effect catalog, and its global-fallback arm
/// answers `Unknown` — not `User` — when a project declaration shadows a name
/// the catalog carries. A caller testing "not `User`" therefore respected the
/// shadow only for names the catalog did *not* know. `preg_match` was
/// uncatalogued and passed; `preg_split` is foldable and reported this finding
/// against the user's own function. Whether a shadow is respected is a property
/// of the source, never of how much the catalog happens to know about the name,
/// so the pair is pinned rather than one of them.
#[test]
fn silent_when_a_project_function_shadows_the_builtin() {
    for name in ["preg_match", "preg_split"] {
        let d = mocked(&format!(
            "<?php\nfunction {name}($p, $s) {{ return 0; }}\n{name}('/(unclosed/', $s);\n"
        ));
        assert!(d.is_empty(), "a shadowed {name} is not the builtin: {d:#?}");
        // …and the same call without the declaration still fires, so the
        // assertion above cannot pass by the check being off altogether.
        let live = mocked(&format!("<?php\n{name}('/(unclosed/', $s);\n"));
        assert_eq!(live.len(), 1, "unshadowed {name} still reports: {live:#?}");
    }
}
