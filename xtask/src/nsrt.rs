//! `nsrt`: the assertType harness (oracle idea B).
//!
//! It consumes PHPStan's own `PHPStan\Testing\assertType('Type', $expr)` assertion
//! corpus (the `tests/PHPStan/Analyser/nsrt/` directory of a checked-out
//! phpstan-src) as an *oracle for inference*: PHPStan asserts the type it infers
//! for `$expr`, and this harness measures Steins' own rendering of the same
//! expression against it. The product is a ranked inventory of inference gaps to
//! drive the pre-release fix hunt.
//!
//! Recognition is the D3 dump-family seam extended (`steins_infer::collect_assert_types`):
//! `assertType` is matched by resolved FQN and `$expr` is rendered through the exact
//! `PHPStan\dumpType` path (best-fact + speller). It is **harness-only** — a normal
//! `check` never recognizes `assertType`.
//!
//! Each nsrt file is a standalone single-file universe (its own namespace, classes,
//! and `use function PHPStan\Testing\assertType;`), so files are analyzed as
//! SEPARATE single-file projects sharing one resident sidecar folder — fast, and
//! free of cross-file namespace collisions.
//!
//! Five-verdict taxonomy (see [`classify`]):
//!
//! - `match` — semantically equal after normalization (case, `|` order, nullable
//!   forms, int-range spelling). Generous only where equivalence is certain.
//! - `unsupported` — the expected string uses vocabulary Steins deliberately does
//!   not model (`*ERROR*`/`*NEVER*`, `mixed`, non-array generics (`Traversable<K,
//!   V>` and friends), intersections, `object`, …), named by pattern. As of S1.5
//!   (ADR-0062), the full array vocabulary (`array{…}`, `list{…}`,
//!   `array<K, V>`, `list<T>`, bare `array`/`list`, and their `non-empty-`
//!   forms) is **not** on this list — S1 taught the speller to spell it, so it
//!   now flows into the normal match/equal/subsumed/differ comparison below.
//! - `equal` — proven-equal-but-differently-spelled (issue #172): the acceptance
//!   relation proves **both** directions (`expected ⊇ got` and `got ⊇ expected`,
//!   each `Certainty::Yes`) while the normalized strings differ. The proof is the
//!   relation's, never a string trick — no normalization rule may claim this
//!   bucket. The canonical inhabitants are the D4-native spelling pairs
//!   (oracle: `array{X}`, Steins: `list{X}` for the same denotation).
//! - `subsumed` — Steins is strictly **more precise** than the oracle: what Steins
//!   renders is a proper subtype of what PHPStan asserts (issue #47). Mutual
//!   subsumption is claimed by `equal` before `subsumed` is consulted, so this
//!   bucket stays strict.
//! - `differ` — Steins renders something semantically different (the gap
//!   inventory), including `unknown` where PHPStan asserts a concrete type (a
//!   reach gap). A pair a human reads as equal that the relation answers
//!   asymmetrically stays here — that is a relation gap to file, not a
//!   normalization to add.
//!
//! ## `subsumed`: why it is not `differ`, and why it is not `match` either
//!
//! PHPStan asserts `bool` for `in_array('foo', ['foo', 'bar'])` because it declines
//! to constant-fold a loose comparison; Steins proves `true`. Scoring that as a
//! `differ` makes the instrument argue against the analyzer: as folding widens
//! (#39) and the argument-dependent return rung lands (ADR-0061), every gain in
//! precision would be booked as a regression. `true` is admissible under `bool` —
//! Steins did not get it wrong, it answered a question the oracle left open.
//!
//! **The relation is the checker's own.** [`subsumption_directions`] lowers both
//! strings through `steins_contract::lower_str` and asks `normalize::subsumes` —
//! in both directions, once per pair — the single
//! acceptance relation the contract layer already uses for param contravariance /
//! return covariance, and the same one behind ADR-0056's envelope subset check. A
//! harness-local notion of "narrower than" would measure something the analyzer
//! does not enforce.
//!
//! **The asymmetry is the point.** Steins answering `bool` where PHPStan asserts
//! `true` is a real gap and stays a `differ`; only `expected ⊇ got` (strictly, and
//! with `Certainty::Yes` on the covering direction) earns `subsumed`. Laundering
//! the reverse direction would turn every widening regression into a "we're more
//! precise" row — the worst possible failure for this instrument. Pinned by
//! `reverse_direction_is_never_subsumption`.
//!
//! ## Headline decision (settled here; do not re-argue per slice)
//!
//! **`subsumed` does NOT count toward the headline `match` number.** The headline
//! is *agreement with the oracle*: a `match` is a claim PHPStan independently
//! confirms. A `subsumed` row is only *unfalsified* by the oracle — the corpus
//! says `bool` is admissible, it does not say `true` is right. A fold bug that
//! produced `'bar'` where the truth is `'foo'` and PHPStan says `string` would land
//! in `subsumed` too, so merging the two would make the headline unfalsifiable and
//! `match` would stop meaning "we reproduce PHPStan".
//!
//! What fixes issue #47 is that these rows leave `differ`, not that they join
//! `match`: a slice that converts ten `differ`s into `subsumed`s now reads as
//! differ falling and subsumed rising, never as a regression. The report prints
//! `match + equal + subsumed` as an explicit secondary **admissible** figure so
//! that movement is visible without unverified claims entering the headline.
//! `equal` sits in admissible on stronger footing than `subsumed` — the relation
//! proves agreement in both directions — but it stays out of the headline for
//! the same reason: the headline counts string-level reproduction of the oracle,
//! and an `equal` row reproduces the denotation, not the spelling.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{AssertObservation, SidecarFolder, collect_assert_types};

use crate::corpus::{collect_php_files, repo_root};

/// Entry point for `cargo xtask nsrt [DIR]`. `DIR` overrides the default nsrt path.
pub fn run(dir_arg: Option<&str>) -> Result<(), String> {
    let dir = match dir_arg {
        Some(d) => PathBuf::from(d),
        None => default_nsrt_dir(),
    };
    if !dir.is_dir() {
        return Err(format!(
            "nsrt directory not found: {}\n  pass the path explicitly: `cargo xtask nsrt <DIR>`",
            dir.display()
        ));
    }

    let mut files = Vec::new();
    collect_php_files(&dir, &mut files);
    files.sort();
    if files.is_empty() {
        return Err(format!("no .php files under {}", dir.display()));
    }
    println!("nsrt: analyzing {} files under {}\n", files.len(), dir.display());

    let start = Instant::now();

    // One resident sidecar folder, reused across every single-file project (the
    // fold posture the gate uses; ADR-0004). Analysis is single-threaded here — the
    // whole nsrt dir folds in seconds — so one folder is enough.
    let mut folder = SidecarFolder::enabled();

    let mut records: Vec<Record> = Vec::new();
    for f in &files {
        let name = f.strip_prefix(&dir).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => continue, // unreadable → contributes nothing
        };

        // Each file is its own single-file project (a standalone universe).
        let db = SteinsDatabase::default();
        let input = SourceFile::new(&db, name.clone(), text);
        let project = Project::new(&db, vec![input], steins_db::ProjectLayout::fallback(), steins_db::PluginFacts::none());
        let observations = collect_assert_types(&db, project, &mut folder);
        for obs in observations {
            records.push(Record::classify(&name, obs));
        }
    }

    let elapsed = start.elapsed();

    report(&records, elapsed.as_secs_f64());
    write_json(&records)?;
    Ok(())
}

/// The default nsrt directory: a sibling phpstan-src checkout, relative to the repo.
fn default_nsrt_dir() -> PathBuf {
    // repo_root = …/repo/rust/steins ; php sibling = …/repo/php/phpstan-src.
    repo_root()
        .join("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../../php/phpstan-src/tests/PHPStan/Analyser/nsrt"))
}

// ----------------------------------------------------------------------------
// classification
// ----------------------------------------------------------------------------

/// The five-verdict taxonomy. Observations whose expected slot could not be
/// resolved to a plain string (`::class`/concat) never reach [`classify`] — they are
/// recorded as the `"skipped"` housekeeping bucket directly in [`Record::classify`]
/// and kept out of the measurement denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Match,
    Unsupported,
    /// Proven-equal-but-differently-spelled (issue #172): the acceptance relation
    /// answers `Yes` in **both** directions while the normalized strings differ.
    ///
    /// The proof is [`is_subsumption`]'s own relation run both ways — never a
    /// string comparison. The bucket exists so the D4-native spelling class
    /// (ADR-0062 §6, as amended) is countable and listable instead of buried in
    /// `differ` among genuine gaps.
    Equal,
    /// Steins' answer is a proper subtype of the assertion (issue #47; see the
    /// module docs for why this is neither `Match` nor `Differ`).
    ///
    /// The verdict names a **type relation, not a quality**. Narrower is usually
    /// better and sometimes not: a fold bug producing the wrong literal under a
    /// correct base type lands here, and so does an over-narrowing that drops a
    /// reachable arm. That is exactly why `subsumed` does not count toward the
    /// headline — calling the bucket "more precise" would smuggle back the
    /// conclusion the headline decision refuses to draw.
    Subsumed,
    Differ,
}

/// One classified assertType observation.
#[derive(Debug, Clone, serde::Serialize)]
struct Record {
    file: String,
    line: u32,
    verdict: &'static str,
    /// The raw PHPStan expected string (or `<unresolved>` when skipped).
    expected: String,
    /// Steins' rendering.
    got: String,
    asserted: bool,
    /// For `unsupported`: the named vocabulary pattern. For
    /// `differ`/`subsumed`/`equal`: the coarse gap-class key (for `equal` it names
    /// the spelling pair). Empty for `match`/`skipped`.
    class: String,
}

impl Record {
    fn classify(file: &str, obs: AssertObservation) -> Record {
        let AssertObservation { line, expected, got, asserted, .. } = obs;
        let Some(expected) = expected else {
            return Record {
                file: file.to_owned(),
                line,
                verdict: "skipped",
                expected: "<unresolved>".to_owned(),
                got,
                asserted,
                class: String::new(),
            };
        };

        let (verdict, class) = classify(&expected, &got);
        Record {
            file: file.to_owned(),
            line,
            verdict: verdict_name(verdict),
            expected,
            got,
            asserted,
            class,
        }
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Match => "match",
        Verdict::Unsupported => "unsupported",
        Verdict::Equal => "equal",
        Verdict::Subsumed => "subsumed",
        Verdict::Differ => "differ",
    }
}

/// Classify one (expected, got) pair. Unsupported-vocabulary expected strings are
/// classified first (Steins does not aim there); otherwise the two are normalized
/// and compared for certain semantic equivalence, then asked the acceptance
/// relation's question in both directions: both `Yes` is proven equality
/// (issue #172), the strict covering direction alone is subsumption (issue #47),
/// anything else is the gap inventory. `equal` claims mutual subsumption before
/// `subsumed` is consulted, which is what keeps `subsumed` strict.
fn classify(expected: &str, got: &str) -> (Verdict, String) {
    if let Some(pattern) = unsupported_pattern(expected) {
        return (Verdict::Unsupported, pattern.to_owned());
    }
    if normalize(expected) == normalize(got) {
        return (Verdict::Match, String::new());
    }
    if is_proven_equal(expected, got) {
        return (Verdict::Equal, gap_class(expected, got));
    }
    if is_subsumption(expected, got) {
        return (Verdict::Subsumed, gap_class(expected, got));
    }
    (Verdict::Differ, gap_class(expected, got))
}

// ----------------------------------------------------------------------------
// subsumption (issue #47) — the checker's own acceptance relation
// ----------------------------------------------------------------------------

/// Steins' own sentinel renderings. They are not type strings: `unknown` is the
/// reach gap this harness exists to inventory, and lowering it would parse as a
/// *class named `unknown`*, which no expected type subsumes with `Yes` — but the
/// guard is explicit so a future sentinel cannot quietly become "more precise".
const STEINS_SENTINELS: &[&str] = &["unknown", "no declared contract"];

/// Both directions of the acceptance question for one pair, asked once. Named so
/// the two verdicts built on it ([`Verdict::Equal`], [`Verdict::Subsumed`]) read
/// off the same evidence instead of re-deriving it.
#[derive(Debug, Clone, Copy)]
struct SubsumptionDirections {
    /// `expected ⊇ got` answered `Certainty::Yes`.
    covers: bool,
    /// `got ⊇ expected` answered `Certainty::Yes`.
    covered: bool,
}

/// Ask the checker's own acceptance relation in both directions for one
/// (expected, got) pair (issues #47 and #172).
///
/// Both strings are lowered through the ordinary phpdoc path
/// (`steins_contract::lower_str`) and judged by `normalize::subsumes`, the single
/// acceptance relation the contract layer already enforces (param contravariance /
/// return covariance, and ADR-0056's envelope subset check). No second definition
/// of narrowing — and no definition of equality other than mutual `Yes` — lives in
/// this harness: if the checker would not call one side an acceptable inhabitant
/// of the other, neither does the instrument.
///
/// Only `Certainty::Yes` counts in either direction. Anything the relation cannot
/// decide (`Maybe`, the honest floor for `Opaque`, for class hierarchies
/// steins-contract carries no oracle for) yields `false` for that direction — the
/// pair stays in the `differ` inventory where it can be triaged, which is the
/// FP-safe direction for a metric. The sentinel and int/float-coercion guards
/// veto the question entirely (both directions `false`): a sentinel is not a type
/// string, and a coercion-crossing pair is answered by a rule this harness is not
/// asking about (see [`crosses_int_float`]).
fn subsumption_directions(expected: &str, got: &str) -> SubsumptionDirections {
    const NEITHER: SubsumptionDirections = SubsumptionDirections { covers: false, covered: false };
    if STEINS_SENTINELS.contains(&got.trim()) {
        return NEITHER;
    }
    if crosses_int_float(expected, got) {
        return NEITHER;
    }
    let (Some(exp_ty), Some(got_ty)) =
        (steins_contract::lower_str(expected), steins_contract::lower_str(got))
    else {
        return NEITHER; // one side does not parse as a type — not a comparison at all
    };
    use steins_contract::normalize::subsumes;
    SubsumptionDirections {
        covers: subsumes(&exp_ty, &got_ty).is_yes(),
        covered: subsumes(&got_ty, &exp_ty).is_yes(),
    }
}

/// Whether the relation proves the pair equal in both directions (issue #172):
/// `expected ⊇ got` and `got ⊇ expected`, each `Certainty::Yes`. This — and only
/// this — awards [`Verdict::Equal`]; no string comparison is consulted.
fn is_proven_equal(expected: &str, got: &str) -> bool {
    let dirs = subsumption_directions(expected, got);
    dirs.covers && dirs.covered
}

/// Whether `got` is strictly narrower than `expected` — Steins answering a question
/// the oracle left open (issue #47).
///
/// Strictness is the covering direction answering `Yes` while the reverse does
/// not; a mutual `Yes` is proven equality and belongs to [`Verdict::Equal`], never
/// here (pinned by `mutual_subsumption_is_not_strict`).
fn is_subsumption(expected: &str, got: &str) -> bool {
    let dirs = subsumption_directions(expected, got);
    dirs.covers && !dirs.covered
}

/// Whether the pair straddles the int/float boundary in the widening direction —
/// the one place the acceptance relation answers a question this harness is not
/// asking.
///
/// `admits_val(float, Int) = Yes` ("float accepts ints", PHPStan core semantics) is
/// the rule for a value crossing into a **declared** `float` slot: PHP coerces it
/// there. It is not a claim that an int value *is* a float, and PHPStan's own
/// hierarchy answers the membership question `No`
/// (`FloatType::isSuperTypeOf(IntegerType)`). So when the oracle asserts `float` and
/// Steins renders `int`, the oracle has *contradicted* Steins, not left the question
/// open — `bug-12393.php:40` is exactly that: `$this->float = $i` on a
/// `private float $float`, where the runtime value is a float and Steins is missing
/// the typed-property coercion. Booking it as precision would launder a live
/// analyzer bug into the good bucket.
///
/// This introduces no second notion of narrowing: [`subsumes`] remains the only
/// relation consulted. It declines to *ask* it across the coercion boundary, which
/// is the direction that keeps a real gap visible in the `differ` inventory.
///
/// [`subsumes`]: steins_contract::normalize::subsumes
fn crosses_int_float(expected: &str, got: &str) -> bool {
    fn int_flavored(s: &str) -> bool {
        normalize(s)
            .split('|')
            .any(|a| matches!(atom_kind(a), "int" | "int-literal" | "int-range"))
    }
    // Only the widening direction is blocked: an int-flavored `got` needs an
    // int-flavored arm in `expected` to be a *member*, not merely coercible.
    int_flavored(got) && !int_flavored(expected)
}

// ----------------------------------------------------------------------------
// unsupported-vocabulary detection (named patterns)
// ----------------------------------------------------------------------------

/// If `expected` uses vocabulary Steins deliberately does not model, return the
/// named pattern; else `None` (it is a supported comparison). An expected string is
/// unsupported iff ANY of its top-level union atoms is unsupported; the returned
/// name is the category of the first such atom (priority order below).
fn unsupported_pattern(expected: &str) -> Option<&'static str> {
    let s = strip_outer_parens(expected.trim());
    // `?X` nullable prefix is supported (handled by the normalizer); everything
    // after this operates on the union atoms.
    let s = s.strip_prefix('?').map(str::trim).unwrap_or(s);
    for atom in split_union(s) {
        if let Some(cat) = atom_unsupported_category(atom.trim()) {
            return Some(cat);
        }
    }
    None
}

/// The unsupported category of a single atom, or `None` if the atom is one Steins
/// can model (a scalar/refined/int-range keyword, a literal, or a plain class name).
fn atom_unsupported_category(atom: &str) -> Option<&'static str> {
    let a = strip_outer_parens(atom).trim();
    let a = a.strip_prefix('?').map(str::trim).unwrap_or(a);

    // Supported shapes first — a positive test keeps the negative list honest.
    if is_supported_atom(a) {
        return None;
    }

    // PHPStan sentinels and set-algebra vocabulary.
    if a.contains('*') {
        return Some("phpstan-special"); // *ERROR*, *NEVER*
    }
    if a.contains('~') {
        return Some("subtraction"); // e.g. mixed~null
    }
    if a.contains('&') {
        return Some("intersection"); // int&object, T&hasMethod(...)
    }
    // `{` no longer implies unsupported (S1.5): a well-formed `array{...}` /
    // `list{...}` / `non-empty-*{...}` already returned `None` above via
    // `is_supported_atom`. Anything still reaching here with a `{` is some
    // other shape-like PHPStan vocabulary this harness has not named yet
    // (kept out of `array-shape`'s old catch-all so it is visible, not
    // silently folded back in) — group it with the generic bucket.
    if a.contains('{') {
        return Some("shape-other");
    }
    // A generic `Name<...>` (an int-range `int<lo, hi>` is supported and handled by
    // `is_supported_atom`, and so is a well-formed `array<...>`/`list<...>`/
    // `non-empty-*<...>` — S1.5 — so any `<` reaching here is a true non-array
    // generic).
    if a.contains('<') {
        if a.contains("class-string") {
            return Some("class-string");
        }
        return Some("generic-other");
    }
    if a.starts_with("callable") || a.contains("Closure") || a.contains("\\Closure") {
        return Some("callable");
    }
    if a.contains("key-of") || a.contains("value-of") {
        return Some("key-of-value-of");
    }
    if a.contains("class-string") {
        return Some("class-string");
    }

    // Bare keyword atoms Steins does not render.
    let low = a.to_ascii_lowercase();
    match low.as_str() {
        "mixed" => Some("mixed"),
        "object" => Some("object"),
        "void" | "never" | "resource" | "scalar" | "empty" | "iterable" => Some("other-keyword"),
        "static" | "self" | "parent" | "$this" => Some("self-static"),
        "callable" => Some("callable"),
        "class-string" => Some("class-string"),
        "" => Some("empty-atom"),
        _ => {
            // A leftover token that is not a plain class name — anything with an
            // interior space or an unexpected punctuation lands here.
            if a.chars().any(|c| c.is_whitespace()) {
                Some("compound")
            } else {
                Some("other")
            }
        }
    }
}

/// Whether a single atom is one Steins can render (so it is fair to *compare*, not
/// classify unsupported). Scalar/refined/int-range keywords, scalar literals,
/// plain class names, and — as of S1.5 (ADR-0062), the array vocabulary the
/// speller now spells (`array`/`list` and their `non-empty-` forms, bare or
/// applied as `array{…}`/`list{…}`/`array<K, V>`/`list<T>`) — all qualify.
fn is_supported_atom(a: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "int",
        "float",
        "string",
        "bool",
        "true",
        "false",
        "null",
        "non-empty-string",
        "non-falsy-string",
        "numeric-string",
        // The casing pair (issue #77): `preds_keyword` has spelled
        // `lowercase-string` / `uppercase-string` since the casing predicates
        // landed, so gating them here measured the harness's own vocabulary rather
        // than the analyzer's — the identical defect S1.5 fixed for the array
        // atoms. Their `non-empty-` intersections are NOT listed: PHPStan spells
        // that set `lowercase-string&non-empty-string`, which is an intersection
        // and stays unsupported on its own terms.
        "lowercase-string",
        "uppercase-string",
        "positive-int",
        "negative-int",
        "non-negative-int",
        // Bare array/list keywords (S1.5): the speller now spells the full array
        // vocabulary, so these are a fair comparison, not an automatic Unsupported.
        "array",
        "non-empty-array",
        "list",
        "non-empty-list",
    ];
    let low = a.to_ascii_lowercase();
    if KEYWORDS.contains(&low.as_str()) {
        return true;
    }
    if is_int_range(&low) {
        return true;
    }
    if is_int_literal(a) || is_float_literal(a) {
        return true;
    }
    if a.starts_with('\'') && a.ends_with('\'') && a.len() >= 2 {
        return true; // a string literal
    }
    if is_array_shape_atom(a) || is_array_generic_atom(a) {
        return true;
    }
    // Reserved lowercase keywords look class-like but name vocabulary Steins does
    // not render as a class — they must NOT pass as a plain class name.
    if RESERVED_UNSUPPORTED_KEYWORDS.contains(&low.as_str()) {
        return false;
    }
    // A plain class name: `Foo`, `\Foo\Bar`, `Foo\Bar` — letters/digits/underscore
    // and namespace separators only, starting class-like.
    is_plain_class_name(a)
}

/// `array{…}` / `list{…}` / `non-empty-array{…}` / `non-empty-list{…}` — the full
/// shape vocabulary the speller now renders (S1, ADR-0062). Structural only: a
/// recognized keyword prefix plus a matching closing brace. `split_union` upstream
/// already hands out brace-balanced atoms, so this never mis-detects a
/// truncated/malformed shape as one of ours.
///
/// What is genuinely unrepresentable *inside* a shape (a conditional type or a
/// template as a field value, a PHPStan-internal pseudo-type such as
/// `oversized-array`) does not make `steins_contract::lower_str` fail or panic —
/// `lib.rs`'s `TypeKind::Conditional`/`TypeKind::Unsupported`/`ConstExpr::Fetch`
/// arms lower it to `Opaque` instead of erroring — so admitting the outer shape
/// here never crashes the classifier; the mismatch just fails to compare equal
/// and lands in `differ`, which is where a real gap belongs.
fn is_array_shape_atom(a: &str) -> bool {
    const PREFIXES: &[&str] = &["array{", "list{", "non-empty-array{", "non-empty-list{"];
    let low = a.to_ascii_lowercase();
    PREFIXES.iter().any(|p| low.starts_with(p)) && a.ends_with('}')
}

/// `array<...>` / `list<...>` / `non-empty-array<...>` / `non-empty-list<...>` —
/// the generic-array/list vocabulary the speller now renders (S1). Same
/// structural-only caveat as [`is_array_shape_atom`].
fn is_array_generic_atom(a: &str) -> bool {
    const PREFIXES: &[&str] = &["array<", "list<", "non-empty-array<", "non-empty-list<"];
    let low = a.to_ascii_lowercase();
    PREFIXES.iter().any(|p| low.starts_with(p)) && a.ends_with('>')
}

/// Bare lowercase keywords that are syntactically class-like but denote vocabulary
/// Steins does not model — never a plain class name, always an unsupported atom.
/// `array`/`list` (and their `non-empty-` forms) are deliberately NOT here as of
/// S1.5: they are recognized earlier, in [`is_supported_atom`]'s `KEYWORDS` list.
const RESERVED_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "mixed", "object", "void", "never", "resource", "scalar", "empty", "iterable",
    "callable", "static", "self", "parent",
];

/// `int<lo, hi>` where lo/hi are `min`/`max`/signed integers (whitespace-tolerant).
fn is_int_range(low: &str) -> bool {
    let Some(inner) = low.strip_prefix("int<").and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    let mut parts = inner.split(',');
    let (Some(lo), Some(hi), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    is_range_bound(lo.trim()) && is_range_bound(hi.trim())
}

fn is_range_bound(b: &str) -> bool {
    b == "min" || b == "max" || is_int_literal(b)
}

fn is_int_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())
}

fn is_float_literal(a: &str) -> bool {
    let t = a.strip_prefix('-').unwrap_or(a);
    let mut dot = false;
    let mut digit = false;
    for c in t.chars() {
        match c {
            '0'..='9' => digit = true,
            '.' if !dot => dot = true,
            _ => return false,
        }
    }
    dot && digit
}

fn is_plain_class_name(a: &str) -> bool {
    let t = a.strip_prefix('\\').unwrap_or(a);
    if t.is_empty() {
        return false;
    }
    let first = t.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\\')
}

// ----------------------------------------------------------------------------
// normalization (certain equivalences only)
// ----------------------------------------------------------------------------

/// Canonicalize a supported type string for equivalence comparison: strip enclosing
/// parens, expand a leading `?` nullable, normalize int-range spelling and each
/// atom, then sort the union (so `|` order and duplicate atoms do not matter).
fn normalize(s: &str) -> String {
    let s = strip_outer_parens(s.trim());
    // `?X` ⇒ `X|null`. The union split below then carries the `null` atom.
    let expanded: String = if let Some(rest) = s.strip_prefix('?') {
        format!("{}|null", rest.trim())
    } else {
        s.to_owned()
    };
    let mut atoms: Vec<String> =
        split_union(&expanded).into_iter().map(|a| normalize_atom(a.trim())).collect();
    atoms.sort();
    atoms.dedup();
    atoms.join("|")
}

/// Canonicalize a single atom. String literals are kept verbatim (case-sensitive);
/// int-range spellings collapse to the named keyword form; everything else lowercases
/// (PHP scalar keywords and class names are case-insensitive).
fn normalize_atom(a: &str) -> String {
    let a = strip_outer_parens(a).trim();
    // A string literal: keep exactly (case & quoting are semantic).
    if a.starts_with('\'') {
        return a.to_owned();
    }
    let a = a.strip_prefix('\\').unwrap_or(a); // drop a leading namespace slash
    let low = a.to_ascii_lowercase();
    // Collapse the three int-range spellings onto one canonical keyword, and vice
    // versa, so `positive-int` == `int<1, max>` etc.
    match canonical_int_range(&low) {
        Some(canon) => canon,
        None => low,
    }
}

/// Map an int-range atom (either the named keyword or the `int<lo, hi>` interval)
/// to one canonical spelling, so the two forms compare equal. Returns `None` for a
/// non-int-range atom.
fn canonical_int_range(low: &str) -> Option<String> {
    match low {
        "positive-int" => return Some("int<1,max>".to_owned()),
        "non-negative-int" => return Some("int<0,max>".to_owned()),
        "negative-int" => return Some("int<min,-1>".to_owned()),
        _ => {}
    }
    let inner = low.strip_prefix("int<")?.strip_suffix('>')?;
    let mut parts = inner.split(',');
    let lo = parts.next()?.trim();
    let hi = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    if !is_range_bound(lo) || !is_range_bound(hi) {
        return None;
    }
    Some(format!("int<{lo},{hi}>"))
}

/// Strip fully-enclosing `(...)` pairs (repeatedly). A pair only encloses when its
/// opening paren matches the final closing paren at depth zero.
fn strip_outer_parens(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        let bytes = s.as_bytes();
        if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
            return s;
        }
        // Verify the opening paren closes exactly at the end (not two siblings).
        let mut depth = 0i32;
        let mut encloses = true;
        for (i, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i != s.len() - 1 {
                        encloses = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !encloses {
            return s;
        }
        s = s[1..s.len() - 1].trim();
    }
}

/// Split a type string on top-level `|`, respecting `'...'` string literals and the
/// nesting depth of `<>`, `{}`, and `()`. (Supported comparison strings carry no
/// brackets, but the splitter stays correct for the unsupported-detector's pass.)
fn split_union(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '\'' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_str = true,
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

// ----------------------------------------------------------------------------
// gap-class heuristic (differ grouping)
// ----------------------------------------------------------------------------

/// A coarse gap-class key for a `differ`, keyed by the expected string's shape and
/// Steins' rendering kind — the ranking axis that drives the fix hunt.
fn gap_class(expected: &str, got: &str) -> String {
    format!("expected:{} | steins:{}", shape_of(expected), kind_of(got))
}

/// The coarse shape of the expected type: single-atom category, or a union label.
fn shape_of(s: &str) -> String {
    let norm = normalize(s);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    let has_null = atoms.contains(&"null");
    let non_null: Vec<&str> = atoms.iter().copied().filter(|a| *a != "null").collect();
    if has_null && non_null.len() == 1 {
        return format!("nullable-{}", atom_kind(non_null[0]));
    }
    if non_null.iter().all(|a| is_scalarish(a)) {
        return if has_null { "scalar-union-null".to_owned() } else { "scalar-union".to_owned() };
    }
    "union".to_owned()
}

/// The coarse kind of Steins' rendering (its own vocabulary).
fn kind_of(got: &str) -> String {
    if got == "unknown" {
        return "unknown".to_owned();
    }
    if got == "no declared contract" {
        return "no-contract".to_owned();
    }
    let norm = normalize(got);
    let atoms: Vec<&str> = norm.split('|').collect();
    if atoms.len() == 1 {
        return atom_kind(atoms[0]).to_owned();
    }
    if atoms.iter().all(|a| is_scalarish(a)) {
        "scalar-union".to_owned()
    } else {
        "union".to_owned()
    }
}

/// The category of one normalized atom.
fn atom_kind(a: &str) -> &'static str {
    if a == "null" {
        return "null";
    }
    if a == "true" || a == "false" {
        return "bool-literal";
    }
    if a == "bool" {
        return "bool";
    }
    if a.starts_with('\'') {
        return "string-literal";
    }
    if is_int_literal(a) {
        return "int-literal";
    }
    if is_float_literal(a) {
        return "float-literal";
    }
    if a.starts_with("int<") {
        return "int-range";
    }
    // Array vocabulary (S1.5): give shapes and generic-array/list atoms their own
    // gap-class label instead of falling into the catch-all `other` bucket — that
    // is what makes the differ ranking legible now that these atoms are compared.
    if is_array_shape_atom(a) {
        return "array-shape";
    }
    if is_array_generic_atom(a) {
        return "array-generic";
    }
    match a {
        "int" => "int",
        "float" => "float",
        "string" => "string",
        "non-empty-string" | "non-falsy-string" | "numeric-string" => "refined-string",
        "array" | "non-empty-array" => "array-bare",
        "list" | "non-empty-list" => "list-bare",
        _ => {
            if is_plain_class_name(a) {
                "class"
            } else {
                "other"
            }
        }
    }
}

fn is_scalarish(a: &str) -> bool {
    !matches!(
        atom_kind(a),
        "class" | "other" | "array-shape" | "array-generic" | "array-bare" | "list-bare"
    )
}

// ----------------------------------------------------------------------------
// reporting
// ----------------------------------------------------------------------------

fn report(records: &[Record], elapsed: f64) {
    let total = records.len();
    let count = |v: &str| records.iter().filter(|r| r.verdict == v).count();
    let (m, u, d, s) = (count("match"), count("unsupported"), count("differ"), count("skipped"));
    let sub = count("subsumed");
    let eq = count("equal");
    // The measurement denominator excludes skipped (unresolvable expected slots).
    let measured = m + u + eq + sub + d;
    let pct = |n: usize| if measured == 0 { 0.0 } else { 100.0 * n as f64 / measured as f64 };

    println!("=== nsrt assertType harness — verdict summary ===\n");
    println!("total assertType observations: {total}");
    println!("  skipped (expected unresolvable ::class/concat): {s}");
    println!("measured (match + unsupported + equal + subsumed + differ): {measured}\n");
    println!("  {:<13} {:>6}   {:>6}", "verdict", "count", "% meas");
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   {:>5.1}%", "match", m, pct(m));
    println!("  {:<13} {:>6}   {:>5.1}%", "unsupported", u, pct(u));
    println!("  {:<13} {:>6}   {:>5.1}%", "equal", eq, pct(eq));
    println!("  {:<13} {:>6}   {:>5.1}%", "subsumed", sub, pct(sub));
    println!("  {:<13} {:>6}   {:>5.1}%", "differ", d, pct(d));
    println!("  {}", "-".repeat(30));
    println!("  {:<13} {:>6}   ({:.2}s)", "TOTAL meas", measured, elapsed);
    // The headline stays `match` — oracle-confirmed agreement at the string level.
    // `equal` rows are proven by the relation and `subsumed` rows are only
    // *unfalsified* by the oracle; both are reported beside the headline, never
    // inside it (issues #47/#172; the argument is in this module's docs).
    println!("\n  HEADLINE (match, oracle-confirmed):   {m}");
    println!("  admissible (match + equal + subsumed): {}\n", m + eq + sub);

    // Unsupported pattern breakdown.
    let mut unsup: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "unsupported") {
        *unsup.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut unsup_sorted: Vec<(&&str, &usize)> = unsup.iter().collect();
    unsup_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("=== unsupported-vocabulary patterns ({u} total) ===\n");
    for (pat, n) in &unsup_sorted {
        println!("  {:<20} {:>6}", pat, n);
    }

    // Equal listing — the whole point of the bucket (issue #172) is that the
    // proven-equal-but-differently-spelled class is countable and listable, so
    // print it whole: each row is a spelling divergence the relation proves
    // denotation-equal in both directions.
    let equals: Vec<&Record> = records.iter().filter(|r| r.verdict == "equal").collect();
    println!("\n=== equal: proven equal, differently spelled ({eq} total) ===\n");
    for r in &equals {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      phpstan: {}\n      steins:  {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }

    // Subsumption listing — small enough to print whole, and worth reading row by
    // row: each one is a place Steins decided something PHPStan left open.
    let subsumed: Vec<&Record> = records.iter().filter(|r| r.verdict == "subsumed").collect();
    println!("\n=== subsumed: Steins narrower than the assertion ({sub} total) ===\n");
    for r in &subsumed {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      phpstan: {}\n      steins:  {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }

    // Differ gap-class ranking.
    let mut gaps: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| r.verdict == "differ") {
        *gaps.entry(r.class.as_str()).or_insert(0) += 1;
    }
    let mut gaps_sorted: Vec<(&&str, &usize)> = gaps.iter().collect();
    gaps_sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n=== differ gap-class ranking ({d} total) ===\n");
    for (gc, n) in &gaps_sorted {
        println!("  {:>6}  {}", n, gc);
    }

    // Top-30 differ listing (file:line, expected vs got).
    let differs: Vec<&Record> = records.iter().filter(|r| r.verdict == "differ").collect();
    println!("\n=== top-30 differs (expected vs got) ===\n");
    for r in differs.iter().take(30) {
        let mark = if r.asserted { " (asserted)" } else { "" };
        println!(
            "  {}:{}\n      expected: {}\n      got:      {}{}",
            r.file, r.line, r.expected, r.got, mark
        );
    }
    if differs.len() > 30 {
        println!("\n  … and {} more differs (see the JSON dump).", differs.len() - 30);
    }
}

/// Write the full machine-readable record set for the follow-up fix slices.
fn write_json(records: &[Record]) -> Result<(), String> {
    let scratch = scratch_dir();
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create scratch dir {}: {e}", scratch.display()))?;
    let path = scratch.join("nsrt-asserttype.json");
    let json = serde_json::to_string_pretty(records)
        .map_err(|e| format!("serializing records: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!("\nnsrt: wrote {} records to {}", records.len(), path.display());
    Ok(())
}

/// The session scratchpad directory (falls back to the repo `target/` if unset).
fn scratch_dir() -> PathBuf {
    std::env::var_os("CLAUDE_SCRATCHPAD")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target").join("nsrt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullable_forms_are_equivalent() {
        assert_eq!(normalize("int|null"), normalize("?int"));
        assert_eq!(normalize("?int"), normalize("null|int"));
    }

    #[test]
    fn union_order_and_dedup() {
        assert_eq!(normalize("string|int"), normalize("int|string"));
        assert_eq!(normalize("int|int|string"), normalize("string|int"));
    }

    #[test]
    fn int_range_spellings_collapse() {
        assert_eq!(normalize("positive-int"), normalize("int<1, max>"));
        assert_eq!(normalize("non-negative-int"), normalize("int<0, max>"));
        assert_eq!(normalize("negative-int"), normalize("int<min, -1>"));
        // A genuinely different interval must NOT collapse to a named class.
        assert_ne!(normalize("positive-int"), normalize("int<2, max>"));
    }

    #[test]
    fn case_insensitivity_for_keywords_and_classes() {
        assert_eq!(normalize("INT"), normalize("int"));
        assert_eq!(normalize("\\Foo\\Bar"), normalize("Foo\\Bar"));
        assert_eq!(normalize("STDCLASS"), normalize("stdClass"));
    }

    #[test]
    fn string_literals_keep_case_and_order() {
        // Case is semantic for string literals.
        assert_ne!(normalize("'A'|'B'"), normalize("'a'|'b'"));
        // But order still does not matter.
        assert_eq!(normalize("'a'|'b'"), normalize("'b'|'a'"));
    }

    #[test]
    fn parenthesized_union_strips() {
        assert_eq!(normalize("(float|int)"), normalize("int|float"));
        assert_eq!(normalize("(DOMAttr|false)"), normalize("false|DOMAttr"));
    }

    #[test]
    fn classify_match_and_differ() {
        assert_eq!(classify("int", "int").0, Verdict::Match);
        assert_eq!(classify("positive-int", "int<1, max>").0, Verdict::Match);
        assert_eq!(classify("int", "unknown").0, Verdict::Differ);
        assert_eq!(classify("int", "string").0, Verdict::Differ);
    }

    // ---- subsumption (issue #47) --------------------------------------------

    #[test]
    fn strictly_narrower_is_subsumed_not_differ() {
        // The row that motivated the verdict: binary.php:547, `in_array('foo',
        // ['foo', 'bar'])` — PHPStan declines to fold the loose comparison and
        // asserts `bool`; Steins proves `true`, which is admissible under `bool`.
        assert_eq!(classify("bool", "true").0, Verdict::Subsumed);
        assert_eq!(classify("int", "5").0, Verdict::Subsumed);
        assert_eq!(classify("string", "'foo'").0, Verdict::Subsumed);
        assert_eq!(classify("int|null", "int").0, Verdict::Subsumed);
        assert_eq!(classify("int", "int<1, max>").0, Verdict::Subsumed);
        assert_eq!(classify("string", "non-empty-string").0, Verdict::Subsumed);
    }

    /// The asymmetry is the whole point of the verdict: Steins *wider* than the
    /// assertion is a real gap. If this ever flipped, every widening regression
    /// would launder itself into the "we're more precise" bucket.
    #[test]
    fn reverse_direction_is_never_subsumption() {
        assert_eq!(classify("true", "bool").0, Verdict::Differ);
        assert_eq!(classify("5", "int").0, Verdict::Differ);
        assert_eq!(classify("'foo'", "string").0, Verdict::Differ);
        assert_eq!(classify("int", "int|null").0, Verdict::Differ);
        assert_eq!(classify("int<1, max>", "int").0, Verdict::Differ);
        assert_eq!(classify("non-empty-string", "string").0, Verdict::Differ);
        assert!(!is_subsumption("true", "bool"));
    }

    #[test]
    fn unrelated_types_and_sentinels_stay_differ() {
        assert_eq!(classify("int", "string").0, Verdict::Differ);
        assert_eq!(classify("'a'", "'b'").0, Verdict::Differ);
        // The reach gap must never read as precision, whatever was asserted.
        assert_eq!(classify("int", "unknown").0, Verdict::Differ);
        assert_eq!(classify("stdClass", "unknown").0, Verdict::Differ);
        assert_eq!(classify("int", "no declared contract").0, Verdict::Differ);
        for s in STEINS_SENTINELS {
            assert!(!is_subsumption("bool", s), "{s} must not be a subsumption");
        }
    }

    /// An equal-but-differently-spelled pair is mutual subsumption, not a *strict*
    /// subtype — it must not enter the subsumed bucket through the back door. (The
    /// normalizer catches the spellings it is certain about before this test runs;
    /// since issue #172, mutual subsumption is claimed by `equal` before `subsumed`
    /// is consulted, so this exclusion is load-bearing for the ladder order.)
    #[test]
    fn mutual_subsumption_is_not_strict() {
        assert!(!is_subsumption("int", "int"));
        assert!(!is_subsumption("positive-int", "int<1, max>"));
    }

    /// The coercion boundary: `float ⊇ int` is `Yes` in the acceptance relation
    /// (PHP coerces at a declared `float` slot) but `No` in PHPStan's hierarchy.
    /// `bug-12393.php:40/56` are Steins missing a typed-property coercion, so they
    /// must stay `differ` — precision must never be inferred from a coercion rule.
    #[test]
    fn int_where_float_is_asserted_is_a_gap_not_precision() {
        assert_eq!(classify("float", "int").0, Verdict::Differ);
        assert_eq!(classify("1.0", "1").0, Verdict::Differ);
        assert_eq!(classify("float|null", "int").0, Verdict::Differ);
        // …but an int-flavored expected arm makes it a genuine membership question.
        assert_eq!(classify("float|int|string", "int").0, Verdict::Subsumed);
        assert_eq!(classify("int|float", "1").0, Verdict::Subsumed);
        // The float side of a mixed-numeric expected is unaffected.
        assert_eq!(classify("float|int|string", "string").0, Verdict::Subsumed);
    }

    #[test]
    fn unsupported_expected_wins_over_subsumption() {
        // `mixed` subsumes everything, but Steins does not aim at `mixed` — the
        // vocabulary verdict is decided first, so the denominator is unchanged.
        assert_eq!(classify("mixed", "int").0, Verdict::Unsupported);
    }

    #[test]
    fn unsupported_patterns_are_named() {
        assert_eq!(unsupported_pattern("*ERROR*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("*NEVER*"), Some("phpstan-special"));
        assert_eq!(unsupported_pattern("mixed"), Some("mixed"));
        assert_eq!(unsupported_pattern("object"), Some("object"));
        assert_eq!(unsupported_pattern("int&object"), Some("intersection"));
        assert_eq!(unsupported_pattern("mixed~null"), Some("subtraction"));
        assert_eq!(unsupported_pattern("class-string<T>"), Some("class-string"));
        // Still-gated: a non-array generic (S1.5 only opened the array vocabulary;
        // Steins runs no template solver over an arbitrary generic class).
        assert_eq!(unsupported_pattern("Traversable<int, string>"), Some("generic-other"));
        // Supported vocab returns None (fair to compare).
        assert_eq!(unsupported_pattern("int|null"), None);
        assert_eq!(unsupported_pattern("positive-int"), None);
        assert_eq!(unsupported_pattern("stdClass"), None);
        assert_eq!(unsupported_pattern("'foo'|'bar'"), None);
        // The casing pair (issue #77): spelled by `preds_keyword`, so measured.
        assert_eq!(unsupported_pattern("lowercase-string"), None);
        assert_eq!(unsupported_pattern("uppercase-string"), None);
        // …and their PHPStan spelling as an intersection stays gated on its own
        // terms — this opened one keyword each, not the `&` operator.
        assert_eq!(
            unsupported_pattern("lowercase-string&non-empty-string"),
            Some("intersection")
        );
    }

    /// S1.5 (ADR-0062): the array vocabulary itself is no longer gated — the
    /// speller spells it (S1), so it is a fair comparison now.
    #[test]
    fn array_vocabulary_is_no_longer_unsupported() {
        for a in [
            "array{}",
            "array{a: int}",
            "list{}",
            "list{int, string}",
            "non-empty-array{a: int}",
            "non-empty-list{0: int}",
            "array<string>",
            "array<string, int>",
            "list<int>",
            "non-empty-array<int>",
            "non-empty-list<int>",
            "array",
            "non-empty-array",
            "list",
            "non-empty-list",
        ] {
            assert_eq!(unsupported_pattern(a), None, "{a} should be a supported comparison now");
        }
    }

    #[test]
    fn supported_atoms_are_not_flagged_unsupported() {
        for a in ["int", "float", "string", "bool", "true", "false", "null",
                  "non-empty-string", "numeric-string", "int<0, 5>", "-3", "1.5",
                  "'x'", "stdClass", "\\Foo\\Bar",
                  "array", "list", "non-empty-array", "non-empty-list",
                  "array{a: int}", "list<int>"] {
            assert!(is_supported_atom(a), "{a} should be supported");
        }
    }

    // ---- array vocabulary now classifiable (S1.5, ADR-0062) ----------------

    /// An array expectation now flows into the normal classify path instead of
    /// being gated Unsupported before `got` is ever read.
    #[test]
    fn array_expectation_is_classifiable() {
        assert_eq!(classify("array{a: int}", "array{a: int}").0, Verdict::Match);
        assert_eq!(classify("list<int>", "list<int>").0, Verdict::Match);
        assert_eq!(classify("array<string, int>", "array<string, int>").0, Verdict::Match);
    }

    /// A genuine D4-native divergence — Steins spells an empty/sequential array
    /// value as `list{…}` where PHPStan stable asserts `array{…}` — lands in the
    /// dedicated `equal` verdict and stays visible, never normalized away
    /// (ADR-0062 §6 as amended 2026-08-07, issue #172). The award is the
    /// relation's own proof of both directions, not a spelling rule: `normalize`
    /// still distinguishes the two strings, so `match` never claims the pair.
    #[test]
    fn d4_native_list_vs_array_divergence_is_equal() {
        assert_eq!(classify("array{}", "list{}").0, Verdict::Equal);
        assert_ne!(normalize("array{}"), normalize("list{}"));
        // The proof, spelled out: mutual Yes through the checker's own relation.
        let dirs = subsumption_directions("array{}", "list{}");
        assert!(dirs.covers && dirs.covered);
    }

    /// The boundary of `equal` (issue #172): the verdict is *proven* equality
    /// through the relation run both ways, never a string trick. A pair the
    /// relation answers asymmetrically — however equal a human reads it — stays
    /// `differ` (a relation gap to file), and sentinels never qualify.
    #[test]
    fn equal_requires_mutual_proof_never_spelling() {
        // Strict narrowing is still `subsumed`, not `equal`.
        assert_eq!(classify("bool", "true").0, Verdict::Subsumed);
        // Widening is still `differ` — `equal` opens no reverse-direction door.
        assert_eq!(classify("true", "bool").0, Verdict::Differ);
        // Sentinels are not type strings; no direction is ever proven.
        assert_eq!(classify("array{}", "unknown").0, Verdict::Differ);
        // String-level identical pairs stay `match`; `equal` needs the spellings
        // to actually differ.
        assert_eq!(classify("array{}", "array{}").0, Verdict::Match);
    }

    /// A pattern the speller still cannot spell (a non-array generic class, here
    /// with a type argument Steins' `lower_generic` would drop) stays Unsupported
    /// — S1.5 narrowed the gate, it did not remove it.
    #[test]
    fn still_gated_pattern_stays_unsupported() {
        assert_eq!(classify("Traversable<int, string>", "unknown").0, Verdict::Unsupported);
    }

    #[test]
    fn skipped_is_kept_out_of_measurement() {
        let obs = AssertObservation {
            path: "f.php".into(),
            line: 3,
            column: 1,
            expected: None,
            got: "unknown".into(),
            asserted: false,
        };
        let rec = Record::classify("f.php", obs);
        assert_eq!(rec.verdict, "skipped");
    }
}
