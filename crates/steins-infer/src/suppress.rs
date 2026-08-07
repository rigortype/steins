//! Inline `@steins-ignore` suppression (ADR-0023), following `@phpstan-ignore`'s
//! spec verbatim.
//!
//! A comment `@steins-ignore <id-list> (optional reason)` suppresses matching
//! object-level diagnostics. Placement copies `@phpstan-ignore`: a comment that
//! **trails code on a line** suppresses matching findings reported on *that* line;
//! a comment **alone on its own line** suppresses findings on the *next* line
//! ([`SourceTree::is_line_leading`] draws the distinction).
//!
//! IDs are registry-governed ([`DIAGNOSTIC_IDS`]) with ADR-0022 prefix semantics:
//! `type.*` (and bare `type`) matches `type.argument-mismatch`. Two always-on
//! meta-diagnostics keep the channel from rotting:
//!
//! * [`SUPPRESS_UNMATCHED_ID`] — an ignore id that matches nothing on its target
//!   line (the anti-rot mechanism), reported at the comment.
//! * [`SUPPRESS_UNKNOWN_ID`] — an unknown/malformed id in an ignore, reported at
//!   the comment.
//!
//! Meta-diagnostics are themselves **exempt** from both suppression channels
//! (suppressing the suppressor is a loop — ADR-0023's channels govern only
//! object-level findings), so this module never re-feeds them through matching.

use std::collections::HashSet;

use steins_syntax::SourceTree;

use crate::Diagnostic;
use crate::{
    ARRAY_DUPLICATE_KEY_ID, CALL_ON_NULL_ID, CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_TOO_MANY_ARGUMENTS_ID, CALL_UNDEFINED_FUNCTION_ID,
    CALL_UNDEFINED_METHOD_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID, CLASS_UNDEFINED_ID,
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, EFFECT_ID,
    CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_EXTENDS_FINAL_ID,
    EFFECT_LISKOV_ID, ID, OFFSET_MAYBE_MISSING_ID, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID,
    OFFSET_UNDECLARED_ID, PARAM_MISMATCH_ID,
    PHPDOC_PROP_MISMATCH_ID, PHPDOC_UNDEFINED_METHOD_ID, PROP_MISMATCH_ID, READONLY_REASSIGNED_ID,
    RETURN_ID, RETURN_MISMATCH_ID, THROW_LISKOV_ID, THROW_UNDECLARED_ID, UNKNOWN_LABEL_ID,
};
// docblock hygiene (ADR-0078, issue #186)
use crate::{
    CLOSURE_UNUSED_USE_ID, PHPDOC_MISPLACED_VAR_ID, PHPDOC_STALE_PARAM_ID, PHPDOC_STALE_VAR_ID,
    PHPDOC_THROWS_NOT_THROWABLE_ID, PHPDOC_UNPARSABLE_ID,
};

/// The registry id for an `@steins-ignore` whose diagnostic id matches nothing on
/// its target line (ADR-0023 anti-rot). Exempt from suppression.
pub const SUPPRESS_UNMATCHED_ID: &str = "suppress.unmatched";

/// The registry id for an unknown/malformed diagnostic id in an `@steins-ignore`
/// (ADR-0022 registry-governed). Exempt from suppression.
pub const SUPPRESS_UNKNOWN_ID: &str = "suppress.unknown-id";

/// The **diagnostic layer** an id carries (ADR-0050 §1): its semantic identity —
/// *what kind of claim it makes* — not a severity grade. The layer, not a string
/// prefix, is the carrier both the fp-gate (ADR-0050 §9) and the user-facing
/// surfaces key on; prefix spellings (`throw.*`) stay a config convenience only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Runtime survivability: the program provably breaks on a live path. Held to
    /// the zero-FP bar; gates red on sight (ADR-0013).
    Proof,
    /// Declared-contract acceptance: a proven behavior violates something the code
    /// *declares* about itself; the program still works. TRUE findings legitimately
    /// abound in released code, so these gate as increase tripwires, never on sight.
    Contract,
    /// The apparatus's own hygiene: a finding whose absence would silently rot
    /// another channel. Gates red on sight (apparatus rot on corpus code).
    Mechanics,
    /// Requested introspection — an **answered question** (ADR-0053 §1): the
    /// finding-shaped report exists *because a call site asked for it*
    /// (`PHPStan\dumpType()`, `var_dump()`), and its content is a fact rendering,
    /// not a claim about the program. Neither a proof (nothing breaks) nor a
    /// contract claim (nothing is declared) nor mechanics (nothing rots if absent).
    /// A layer, not a boolean, precisely so every exhaustive `Layer` match is forced
    /// to state its debug posture at compile time (the point-1 discipline). fp-gate:
    /// excluded from every counter (§8). Emitted from D3/D4; registered but unemitted
    /// in the D1 groundwork.
    Debug,
}

impl Layer {
    /// The lowercase wire spelling (`"proof"|"contract"|"mechanics"|"debug"`) used by
    /// the `--format json` `layer` field (ADR-0050 §2 / ADR-0053 §4).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Layer::Proof => "proof",
            Layer::Contract => "contract",
            Layer::Mechanics => "mechanics",
            Layer::Debug => "debug",
        }
    }
}

/// The value of the `origin` facet (ADR-0050 §4): whether a `throw.undeclared`
/// finding's escaping-throw origin site lies in the annotated declaration's **own
/// body** ([`Origin::Direct`]) or arrived up one or more call edges
/// ([`Origin::Propagated`]). This productionizes the direct-vs-propagated
/// measurement (`docs/notes/20260724-g1-throw-origin-measurement.md`): 158 direct
/// vs 43,805 propagated on the legacy monorepo, the split the `throws-direct`
/// built-in profile selects on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The origin lies in the annotated declaration's own body.
    Direct,
    /// The origin lies elsewhere, reached through a call hop.
    Propagated,
}

impl Origin {
    /// The wire spelling (`"direct"|"propagated"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Direct => "direct",
            Origin::Propagated => "propagated",
        }
    }
}

/// The name of the only facet v1 defines (ADR-0050 §4/§11), carried solely by
/// `throw.undeclared`. Kept as a named constant so the emitter, the JSON key, and
/// [`declared_facet`] agree on one spelling.
pub const FACET_ORIGIN: &str = "origin";

/// A registry-declared **facet** value a finding carries (ADR-0050 §4): an
/// additional classification axis, recorded by the emitter at emit time from
/// walk-local data, that profile entries may select on. v1 declares exactly one
/// facet — `origin`, carried only by `throw.undeclared`. The `default`/`contracts`
/// built-ins ignore it; the `throws-direct` built-in selects `origin = direct`.
///
/// Kept a small enum (not an open string) so a second facet is an ADR-forced
/// variant, never an ad-hoc key. The `--format json` output shows it additively as
/// `"<key>": "<value>"` only on ids that declare a facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facet {
    /// The `origin` facet value (`direct|propagated`).
    Origin(Origin),
}

impl Facet {
    /// The wire key (`"origin"`) the additive JSON facet field uses.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Facet::Origin(_) => FACET_ORIGIN,
        }
    }

    /// The wire value (`"direct"|"propagated"`).
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Facet::Origin(o) => o.as_str(),
        }
    }
}

/// The facet an emitted `id` **declares**, if any (ADR-0050 §4). v1: only
/// `throw.undeclared` declares the `origin` facet; every other id declares none,
/// so its findings never carry — nor show — a facet key. Returns the facet *name*
/// (a profile-selectable axis's identity), not a value. Registering that an id
/// carries a facet is what lets the emitter attach one and profiles select on it.
#[must_use]
pub fn declared_facet(id: &str) -> Option<&'static str> {
    if id == THROW_UNDECLARED_ID { Some(FACET_ORIGIN) } else { None }
}

/// The **lowest profile rung** on which a registered id may reach the surface
/// (ADR-0062 A-G10). The profile ladder is cumulative — `default ⊂ contracts ⊂
/// strict` — so an id is admitted by a profile exactly when its floor is at or
/// below that profile's rung. Ordered smallest-first: `Default < Contracts <
/// Strict`, and the `Ord` derive is what the admission test uses.
///
/// A floor is finer than a layer *set*: a built-in profile admits an id when the
/// id's floor is at or below the profile's rung. This lets a *single* layer
/// straddle two rungs — the contract layer holds both floor-`Contracts` ids and
/// floor-`Strict` ids (the offset family's strict leg), which naming layer sets
/// could not express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Floor {
    /// On the bare `steins check` surface (and therefore every surface above it).
    Default,
    /// Reached first by the `contracts` opt-up stage.
    Contracts,
    /// Reached only by the `strict` opt-up stage (ADR-0062 A-G10).
    Strict,
}

impl Floor {
    /// The wire/config spelling (`"default"|"contracts"|"strict"`) — also the rung
    /// name a baseline entry records as its capture surface (A-G10).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Floor::Default => "default",
            Floor::Contracts => "contracts",
            Floor::Strict => "strict",
        }
    }

    /// Parse a rung spelling back (the baseline round-trip). Deliberately an inherent
    /// method, not `FromStr`: there is no error type worth naming and the tolerant
    /// `None` reading is the whole contract. Unknown spellings
    /// yield `None`; the caller decides the tolerant reading.
    #[must_use]
    pub fn parse(s: &str) -> Option<Floor> {
        match s {
            "default" => Some(Floor::Default),
            "contracts" => Some(Floor::Contracts),
            "strict" => Some(Floor::Strict),
            _ => None,
        }
    }
}

/// The diagnostic-id registry (ADR-0022/0050/0062): the closed set of ids Steins
/// emits, each paired with its [`Layer`] (ADR-0050 §2 makes the layer a first-class
/// registry attribute) and its [`Floor`] (ADR-0062 A-G10's one added attribute).
/// This is the **single source of truth** — `DIAGNOSTIC_IDS` is derived from it,
/// `layer()` and `surface_floor()` read it, and registering an id here without both
/// attributes does not compile (every entry is an `(id, Layer, Floor)` triple). A
/// workspace totality test asserts every *emittable* id constant appears here.
///
/// **The floor column reproduces the pre-S6 profile behavior exactly** and is
/// pinned by `tests/registry.rs`: proof, mechanics and debug ids carry
/// `Floor::Default` (the `default` built-in's layer set), every contract id that
/// existed before S6 carries `Floor::Contracts` (reached first by the `contracts`
/// built-in), and only the two S6 ids carry `Floor::Strict`.
///
/// `@steins-ignore` ids are validated against it (prefix-aware), and the baseline
/// records these ids verbatim.
pub const DIAGNOSTIC_REGISTRY: &[(&str, Layer, Floor)] = &[
    // proof — runtime survivability (zero-FP, red on sight).
    (ID, Layer::Proof, Floor::Default),
    (RETURN_ID, Layer::Proof, Floor::Default),
    (CALL_ON_NULL_ID, Layer::Proof, Floor::Default),
    (PROP_MISMATCH_ID, Layer::Proof, Floor::Default),
    (READONLY_REASSIGNED_ID, Layer::Proof, Floor::Default),
    // proof — finding-breadth family (ADR-0049).
    (CALL_UNDEFINED_FUNCTION_ID, Layer::Proof, Floor::Default),
    (CALL_UNDEFINED_METHOD_ID, Layer::Proof, Floor::Default),
    (CLASS_UNDEFINED_ID, Layer::Proof, Floor::Default),
    (CALL_TOO_FEW_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    (CALL_TOO_MANY_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    (CALL_UNKNOWN_NAMED_ARGUMENT_ID, Layer::Proof, Floor::Default),
    (OFFSET_MISSING_ID, Layer::Proof, Floor::Default),
    (OFFSET_ON_UNSUPPORTED_ID, Layer::Proof, Floor::Default),
    // printf arity (ADR-0078, issue #188)
    (CALL_PRINTF_TOO_FEW_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    // end printf arity (ADR-0078, issue #188)
    // declaration-incompatibility fatals (ADR-0078, issue #183): load-time fatals
    // read off the declaration graph alone — proof layer at the `default` floor,
    // per the ADR's floor table.
    (CLASS_ABSTRACT_UNIMPLEMENTED_ID, Layer::Proof, Floor::Default),
    (CLASS_EXTENDS_FINAL_ID, Layer::Proof, Floor::Default),
    // contract — declared-contract acceptance (increase tripwires).
    (PARAM_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    (RETURN_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    (PHPDOC_PROP_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    (THROW_UNDECLARED_ID, Layer::Contract, Floor::Contracts),
    (THROW_LISKOV_ID, Layer::Contract, Floor::Contracts),
    (EFFECT_ID, Layer::Contract, Floor::Contracts),
    (EFFECT_LISKOV_ID, Layer::Contract, Floor::Contracts),
    // contract — finding-breadth declared-receiver lane (ADR-0049 §8).
    (PHPDOC_UNDEFINED_METHOD_ID, Layer::Contract, Floor::Contracts),
    // contract — the offset family's STRICT leg (ADR-0062 A-G10, issue #51):
    // Asserted declared-envelope evidence. `offset.undeclared` sits at `Contracts`
    // (a corpus sweep measured zero findings); `offset.maybe-missing` stays at
    // `Strict` until the `isset`→`@phpstan-assert` discharge-ladder gap is closed.
    (OFFSET_UNDECLARED_ID, Layer::Contract, Floor::Contracts),
    (OFFSET_MAYBE_MISSING_ID, Layer::Contract, Floor::Strict),
    // mechanics — apparatus hygiene (red on sight, suppression-exempt).
    (SUPPRESS_UNMATCHED_ID, Layer::Mechanics, Floor::Default),
    (SUPPRESS_UNKNOWN_ID, Layer::Mechanics, Floor::Default),
    (UNKNOWN_LABEL_ID, Layer::Mechanics, Floor::Default),
    // mechanics — the member-kind port wave's first id (ADR-0078, issue #187):
    // works-but-drops-a-value intent/behaviour drift, not a runtime break.
    (ARRAY_DUPLICATE_KEY_ID, Layer::Mechanics, Floor::Default),
    // docblock hygiene (ADR-0078, issue #186)
    // Anti-rot ids about annotations that drifted from the code. The `phpdoc.*`
    // prefix now spans two layers (ADR-0078 §1.5): these five are mechanics —
    // `disable`-proof and undemotable — while `phpdoc.param-mismatch` and its
    // siblings above stay contract. The layer, never the prefix, decides.
    (PHPDOC_UNPARSABLE_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_STALE_PARAM_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_STALE_VAR_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_MISPLACED_VAR_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_THROWS_NOT_THROWABLE_ID, Layer::Mechanics, Floor::Default),
    (CLOSURE_UNUSED_USE_ID, Layer::Mechanics, Floor::Default),
    // debug — the dump surface (ADR-0053): requested introspection, not a finding.
    // Suppression-, baseline-, and fp-gate-exempt (§4/§8). The `Default` floor is
    // inert for capture: the debug lane's exclusion from `surfaces_id` is a *layer*
    // property, decided before the ladder is consulted.
    (DEBUG_TYPE_ID, Layer::Debug, Floor::Default),
    (DEBUG_PHPDOC_TYPE_ID, Layer::Debug, Floor::Default),
    (DEBUG_VAR_DUMP_ID, Layer::Debug, Floor::Default),
    // The trace annotation (ADR-0074 §4): the docblock spelling of the dump
    // surface's question.
    (DEBUG_TRACE_ID, Layer::Debug, Floor::Default),
];

/// The flat id list, **derived** from [`DIAGNOSTIC_REGISTRY`] so there is exactly
/// one source of truth. Kept as a `&[&str]` for the prefix-matching consumers and
/// the baseline.
pub const DIAGNOSTIC_IDS: &[&str] = &derive_ids();

/// Project the registry down to its ids at compile time (keeps `DIAGNOSTIC_IDS` a
/// pure derivation of [`DIAGNOSTIC_REGISTRY`], never a parallel hand-list).
const fn derive_ids() -> [&'static str; DIAGNOSTIC_REGISTRY.len()] {
    let mut arr = [""; DIAGNOSTIC_REGISTRY.len()];
    let mut i = 0;
    while i < DIAGNOSTIC_REGISTRY.len() {
        arr[i] = DIAGNOSTIC_REGISTRY[i].0;
        i += 1;
    }
    arr
}

/// The [`Layer`] a diagnostic `id` carries, or `None` if `id` is not a registered
/// diagnostic id (ADR-0050 §2). An exact-id lookup — prefix/family subsumption is
/// [`pattern_is_known`]'s concern, not the layer attribute's.
#[must_use]
pub fn layer(id: &str) -> Option<Layer> {
    DIAGNOSTIC_REGISTRY.iter().find(|(i, ..)| *i == id).map(|(_, l, _)| *l)
}

/// The [`Floor`] a diagnostic `id` carries — the lowest profile rung that admits
/// it (ADR-0062 A-G10) — or `None` if `id` is not registered. Exact-id lookup, the
/// sibling of [`layer`].
#[must_use]
pub fn surface_floor(id: &str) -> Option<Floor> {
    DIAGNOSTIC_REGISTRY.iter().find(|(i, ..)| *i == id).map(|(.., f)| *f)
}

/// The result of applying inline ignores to a batch of object-level findings.
pub struct InlineOutcome {
    /// Object-level findings that were **not** suppressed (fed onward to the
    /// baseline channel and, ultimately, printed).
    pub kept: Vec<Diagnostic>,
    /// How many object-level findings inline ignores suppressed.
    pub suppressed: usize,
    /// The meta-diagnostics produced (`suppress.unmatched` / `suppress.unknown-id`).
    /// Never suppressed or baselined themselves.
    pub meta: Vec<Diagnostic>,
}

/// A parsed `@steins-ignore` directive from one comment.
struct Directive {
    /// The raw id tokens (comma-separated list; may include unknown/malformed).
    patterns: Vec<String>,
    /// The line the directive suppresses on (its own line if trailing, else next).
    target_line: u32,
    /// The comment's own 1-based line/column (where meta-diagnostics are reported).
    line: u32,
    column: u32,
}

/// Whether an ignore `pattern` (`type`, `type.*`, or `type.argument-mismatch`)
/// **matches** a concrete diagnostic `id` under ADR-0022 prefix subsumption: a
/// bare/`.*` family matches every id beneath it; an exact id matches itself.
/// Segment-aware, so `type` does not match `typex.*`. Shared by the inline-ignore
/// channel and the profile engine's `enable`/`disable`/`warn` id-arrays (ADR-0050
/// §5), so both read the ADR-0022 prefix semantics from one place.
#[must_use]
pub fn pattern_matches(pattern: &str, id: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    id == norm || id.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether an ignore `pattern` is **registry-governed** (ADR-0022): after
/// stripping a trailing `.*`, it equals a registry id or is a family prefix of at
/// least one. Unknown/malformed patterns earn `suppress.unknown-id`.
#[must_use]
pub fn pattern_is_known(pattern: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    if norm.is_empty() {
        return false;
    }
    DIAGNOSTIC_IDS
        .iter()
        .any(|&r| r == norm || r.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.')))
}

/// Extract the text following `@steins-ignore` in a comment, trimming the comment
/// terminator (`*/`) and surrounding whitespace. `None` if the marker is absent.
fn extract_directive(text: &str) -> Option<&str> {
    let idx = text.find("@steins-ignore")?;
    let mut rest = &text[idx + "@steins-ignore".len()..];
    if let Some(end) = rest.find("*/") {
        rest = &rest[..end];
    }
    Some(rest.trim())
}

/// Parse the id list from a directive body: everything before an optional
/// parenthesized reason, comma-separated, trimmed, non-empty tokens.
fn parse_id_list(rest: &str) -> Vec<String> {
    let id_part = rest.find('(').map_or(rest, |p| &rest[..p]);
    id_part
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Collect every `@steins-ignore` directive in a file, resolving placement.
fn directives(tree: &SourceTree) -> Vec<Directive> {
    let mut out = Vec::new();
    for c in tree.comments() {
        let Some(body) = extract_directive(&c.text) else { continue };
        let patterns = parse_id_list(body);
        let pos = tree.position(c.span.start);
        // Trailing comment → this line; own-line comment → next line.
        let target_line =
            if tree.is_line_leading(c.span.start) { pos.line + 1 } else { pos.line };
        out.push(Directive { patterns, target_line, line: pos.line, column: pos.column });
    }
    out
}

/// Apply inline `@steins-ignore` suppression to object-level `findings`. `files`
/// pairs every analyzed file's diagnostic path with its parsed tree, so each
/// finding's comments can be consulted. Findings whose path is not among `files`
/// are kept untouched.
#[must_use]
pub fn apply_inline_ignores(
    findings: Vec<Diagnostic>,
    files: &[(String, &SourceTree)],
) -> InlineOutcome {
    let mut kept = Vec::new();
    let mut suppressed = 0usize;
    let mut meta = Vec::new();
    let known_paths: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    for (path, tree) in files {
        let dirs = directives(tree);
        // Per-directive, per-pattern "used" flags, to drive `suppress.unmatched`.
        let mut used: Vec<Vec<bool>> = dirs.iter().map(|d| vec![false; d.patterns.len()]).collect();

        for f in findings.iter().filter(|f| &f.path == path) {
            // ADR-0053 §4: the debug lane is exempt from inline ignores — a dump is an
            // answered question, not a finding; the remedy is deleting the call, one
            // keystroke away. A debug finding is never suppressed and never marks a
            // directive pattern used, so an `@steins-ignore debug.type` naming it stays
            // unmatched and earns `suppress.unmatched` (the anti-rot channel's normal job).
            if matches!(layer(f.id), Some(Layer::Debug)) {
                kept.push(f.clone());
                continue;
            }
            let mut is_suppressed = false;
            for (di, d) in dirs.iter().enumerate() {
                if d.target_line != f.line {
                    continue;
                }
                for (pi, pat) in d.patterns.iter().enumerate() {
                    if pattern_is_known(pat) && pattern_matches(pat, f.id) {
                        used[di][pi] = true;
                        is_suppressed = true;
                    }
                }
            }
            if is_suppressed {
                suppressed += 1;
            } else {
                kept.push(f.clone());
            }
        }

        // Meta-diagnostics: unknown ids, then unmatched (still-unused) valid ids.
        for (di, d) in dirs.iter().enumerate() {
            if d.patterns.is_empty() {
                meta.push(meta_diag(
                    SUPPRESS_UNKNOWN_ID,
                    path,
                    d,
                    "malformed @steins-ignore (no diagnostic id given)".to_owned(),
                ));
                continue;
            }
            for (pi, pat) in d.patterns.iter().enumerate() {
                if !pattern_is_known(pat) {
                    meta.push(meta_diag(
                        SUPPRESS_UNKNOWN_ID,
                        path,
                        d,
                        format!("@steins-ignore names unknown diagnostic id '{pat}'"),
                    ));
                } else if !used[di][pi] {
                    meta.push(meta_diag(
                        SUPPRESS_UNMATCHED_ID,
                        path,
                        d,
                        format!(
                            "@steins-ignore of {pat} matches no diagnostic on line {}",
                            d.target_line
                        ),
                    ));
                }
            }
        }
    }

    // Findings for files not in the batch (should not arise) pass through.
    for f in findings {
        if !known_paths.contains(f.path.as_str()) {
            kept.push(f);
        }
    }

    InlineOutcome { kept, suppressed, meta }
}

/// Build a meta-diagnostic at a directive's comment location.
fn meta_diag(id: &'static str, path: &str, d: &Directive, message: String) -> Diagnostic {
    // Mechanics meta-diagnostics declare no facet (ADR-0050 §4: only
    // `throw.undeclared` does).
    Diagnostic { id, path: path.to_owned(), line: d.line, column: d.column, message, facet: None, fix: None }
}

#[cfg(test)]
mod tests {
    use super::{extract_directive, parse_id_list, pattern_is_known, pattern_matches};

    #[test]
    fn prefix_and_bare_family_match() {
        assert!(pattern_matches("type.argument-mismatch", "type.argument-mismatch"));
        assert!(pattern_matches("type.*", "type.argument-mismatch"));
        assert!(pattern_matches("type", "type.argument-mismatch"));
        // The `type.return-mismatch` id joins the same `type.*` family.
        assert!(pattern_matches("type.return-mismatch", "type.return-mismatch"));
        assert!(pattern_matches("type.*", "type.return-mismatch"));
        assert!(pattern_matches("type", "type.return-mismatch"));
        assert!(pattern_matches("effect", "effect.envelope-exceeded"));
        // Segment-aware: `type` must not match a differently-rooted family.
        assert!(!pattern_matches("type", "typex.foo"));
        assert!(!pattern_matches("effect", "type.argument-mismatch"));
    }

    #[test]
    fn known_vs_unknown_ids() {
        assert!(pattern_is_known("type.argument-mismatch"));
        assert!(pattern_is_known("type.return-mismatch"));
        assert!(pattern_is_known("type.*"));
        assert!(pattern_is_known("type"));
        assert!(pattern_is_known("effect.envelope-exceeded"));
        assert!(pattern_is_known("suppress.unmatched"));
        // The ADR-0053 debug ids are registry-governed, so an `@steins-ignore`
        // naming one is a *known* pattern (never `suppress.unknown-id`). It matches
        // no dump finding (a dump is suppression-exempt, §4), so it reports
        // `suppress.unmatched` — the anti-rot channel doing its normal job.
        assert!(pattern_is_known("debug.type"));
        assert!(pattern_is_known("debug.phpdoc-type"));
        assert!(pattern_is_known("debug.var-dump"));
        assert!(pattern_is_known("debug.*"));
        assert!(pattern_is_known("debug"));
        // Typos and unknown families.
        assert!(!pattern_is_known("type.bogus"));
        assert!(!pattern_is_known("nope"));
        assert!(!pattern_is_known(""));
    }

    #[test]
    fn directive_extraction_handles_all_comment_shapes() {
        assert_eq!(extract_directive("// @steins-ignore type.x"), Some("type.x"));
        assert_eq!(extract_directive("# @steins-ignore type.x (why)"), Some("type.x (why)"));
        assert_eq!(extract_directive("/* @steins-ignore type.x */"), Some("type.x"));
        assert_eq!(extract_directive("// unrelated comment"), None);
    }

    #[test]
    fn id_list_splits_comma_and_strips_reason() {
        assert_eq!(parse_id_list("type.x, effect.y (reason here)"), vec!["type.x", "effect.y"]);
        assert_eq!(parse_id_list("type.x"), vec!["type.x"]);
        assert!(parse_id_list("(only a reason)").is_empty());
    }
}
