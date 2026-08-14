//! Transform #3 — loop→`array_map` under a **proven-purity** precondition
//! (ADR-0076, issue #116). The flagship transform of ADR-0010:
//!
//! ```php
//! $out = [];
//! foreach ($xs as $x) { $out[] = f($x); }
//! // becomes
//! $out = array_map(fn ($x) => f($x), $xs);
//! ```
//!
//! Trivial to *spell*, impossible for a rule-driven codemod to *justify*: the
//! precondition is a whole-project effect judgment. This module asks the engine
//! for it and refuses, by name, everywhere it is not answered.
//!
//! ## The bar: proven, strictly stronger than ADR-0006 `Pure`
//!
//! 1. Body's **proven** effect lane empty on every label, exhaustiveness
//!    intact — an unresolved call refuses [`REASON_BODY_CALL_UNRESOLVED`],
//!    never assumed harmless.
//! 2. **Declared bounds never qualify** (ADR-0067): a non-empty **declared**
//!    lane also refuses [`REASON_BODY_CALL_UNRESOLVED`] — the effect pass
//!    discharges the exhaustiveness taint at a declared-answered call, so
//!    reading exhaustiveness alone would admit exactly what the lane wall
//!    exists to block.
//! 3. **Proven throw set empty**, stricter than `Pure`: a throw on element `k`
//!    leaves `$out` holding the first `k` results (observable in an enclosing
//!    `catch`, evaluated with guards stripped), while the rewrite's
//!    all-or-nothing assignment leaves it unassigned.
//!
//! ## Parity gates beyond purity
//!
//! - Subject must prove `array` **and** `is_list = Yes` at `Verified`:
//!   `array_map` preserves keys, `$out[] = …` renumbers `0..n-1`. A
//!   docblock-asserted shape refuses unless
//!   [`LoopToArrayMapOptions::asserted_subjects`] (issue #175) admits a
//!   declaration proving both halves at the Asserted stratum (`list<T>`),
//!   counted and labeled separately; `array` alone refuses at the list gate, a
//!   bare `array $xs` at the array gate.
//! - Iteration variable must not occur after the loop (`foreach` leaks it, the
//!   arrow function does not); v1 scans the remainder textually — sound in the
//!   refusing direction.
//! - Accumulator may occur **only** as the append target anywhere in the
//!   `foreach`, and its `$out = [];` initializer must immediately precede the
//!   loop with a whitespace-only gap (the rewrite consumes both statements, so
//!   a comment there refuses rather than being eaten).
//!
//! ## Enumeration domain
//!
//! **Every** `foreach` is a candidate ([`steins_syntax::SourceTree::foreach_sites`]) —
//! narrowing to append-shaped loops would hide the narrowness the completeness
//! oracle exists to expose; the refusal distribution is the v2 roadmap (ADR-0076 §6).

use steins_db::{Db, Project, SourceFile, parse};
use steins_infer::{RegionPurity, SubjectFact, probe_subjects, region_purity_project};
use steins_syntax::{ForeachSite, SourceTree, Span};

use crate::obstacles::VouchSet;
use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{
    AssertedAdmission, CompletenessOracle, Refusal, SiteRef, Transform, TransformReport,
};

// ---- Stable refusal reason names (ADR-0034 point 2 / ADR-0076 §4) ----------

/// `foreach ($xs as $k => $v)`: `array_map` passes only the value, so the key
/// form has no v1 spelling.
pub const REASON_KEY_BINDING: &str = "key-binding";
/// `foreach ($xs as &$v)`: writes back through the subject; `array_map` builds
/// a new array and writes nothing.
pub const REASON_REFERENCE_BINDING: &str = "reference-binding";
/// `foreach ($xs as [$a, $b])` / `list(...)`: a destructuring binding — an
/// arrow function cannot spell a destructuring parameter.
pub const REASON_VALUE_BINDING_NOT_VARIABLE: &str = "value-binding-not-variable";
/// The iterated expression is not a plain variable; v1 moves it verbatim into
/// `array_map`, so only a bare variable is guaranteed to evaluate once.
pub const REASON_SUBJECT_NOT_VARIABLE: &str = "subject-not-variable";
/// Subject not proven a plain `array` at the `Verified` stratum: `foreach`
/// iterates any `Traversable`, `array_map` `TypeError`s on one.
pub const REASON_SUBJECT_NOT_PROVEN_ARRAY: &str = "subject-not-proven-array";
/// Subject proves `array` but not `is_list = Yes`: `array_map` preserves keys,
/// the append renumbers `0..n-1`. Laundering via `array_values(...)` is
/// rejected (ADR-0076 §6) — a non-list subject is a refusal, not a shape v1
/// quietly fixes.
pub const REASON_SUBJECT_NOT_PROVEN_LIST: &str = "subject-not-proven-list";
/// The accumulator's `$out = [];` initializer is not the statement immediately
/// preceding the loop, or the gap between them holds more than whitespace.
pub const REASON_ACCUMULATOR_INIT_NOT_ADJACENT: &str = "accumulator-init-not-adjacent";
/// The adjacent initializer assigns something other than an empty array
/// literal; a compound rewrite (`array_merge($out, array_map(...))`) is a
/// later slice, not a v1 shape (ADR-0076 §6).
pub const REASON_ACCUMULATOR_NOT_EMPTY: &str = "accumulator-not-empty";
/// The accumulator occurs somewhere other than the append target (read in the
/// expression, or bound as the iteration variable), so the loop observes its
/// own partial state, which an all-at-once assignment cannot reproduce.
pub const REASON_ACCUMULATOR_READ_IN_BODY: &str = "accumulator-read-in-body";
/// The iteration variable occurs after the loop; `foreach` leaves it bound to
/// the last element, an arrow-function parameter does not escape.
pub const REASON_ITERATION_VAR_LIVE_AFTER: &str = "iteration-var-live-after";
/// The body carries `break` / `continue` / `return` / `goto`: the loop can end
/// early, a whole-array map cannot.
pub const REASON_EARLY_EXIT: &str = "early-exit";
/// Body is not exactly one `$out[] = <expr>;` statement, or the append
/// expression **writes** a variable (assignment / `++` / `--`) — `fn`
/// captures by value, so such a write would not carry.
pub const REASON_BODY_NOT_SINGLE_APPEND: &str = "body-not-single-append";
/// The body's **proven** effect lane is non-empty (detail names the labels).
pub const REASON_BODY_EFFECTS: &str = "body-effects";
/// The body's **proven** throw set is non-empty (detail names the classes) —
/// stricter than ADR-0006 `Pure`, which admits `throw` (see module docs).
pub const REASON_BODY_THROWS: &str = "body-throws";
/// A call in the body did not resolve: a dynamic callee, an opaque receiver, a
/// construct the effect scan does not model (`new` / `clone` / `yield` /
/// backticks / an ADR-0001 poison construct / a frame-sensitive builtin like
/// `compact`), or a call answered only by a **declared** bound (ADR-0067: a
/// cap is not an occurrence proof).
pub const REASON_BODY_CALL_UNRESOLVED: &str = "body-call-unresolved";

/// The loop→`array_map` transform (ADR-0076).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopToArrayMap;

impl Transform for LoopToArrayMap {
    fn id(&self) -> &'static str {
        "loop-to-array-map"
    }
}

/// Per-run options for [`plan_loop_to_array_map`] (issue #175). `Default` is
/// the proven-only v1 gate, byte-identical to a run before the options existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopToArrayMapOptions {
    /// Admit a subject whose `array` AND list evidence both hold at the
    /// **Asserted** stratum (a docblock `list<T>`), counted in
    /// [`CompletenessOracle::transformed_asserted`] and labeled in
    /// [`TransformReport::asserted_admissions`]. Off, the gate consumes the
    /// proven lane only. Declared evidence proving `array` alone still refuses
    /// at the list gate.
    pub asserted_subjects: bool,
}

/// Which trust lane admitted a rewritten site's subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubjectLane {
    /// The subject proved `array` + `is_list = Yes` at the `Verified` stratum.
    Proven,
    /// The subject qualified on declared evidence under the explicit opt-in.
    Asserted,
}

/// The trust label an admitted-under-opt-in site carries in its report entry
/// (amendment condition 3): must say list-ness is declared not proven, name the
/// concrete behavioral risk, and not pretend the post-check could catch it.
fn asserted_label(subject: &str) -> String {
    format!(
        "subject `${subject}` admitted on declared evidence: its array-ness and list-ness are asserted by a declaration, not proven. If the claim is wrong — the value is actually string-keyed or gapped — this rewrite changes behavior, because array_map preserves keys where the append renumbered them 0..n-1. The post-check cannot catch a wrong list claim; reviewing this diff is the gate."
    )
}

/// Plan the loop→`array_map` rewrite over `project`. Pure planning — the caller
/// (CLI) drives the dry-run diff, ADR-0034's dual-verification post-check, and
/// any `--apply` write. `vouches` and `partitions` are accepted for signature
/// parity but **not consumed**: this transform enumerates no callers, so
/// ADR-0046 §2's obstacles decide nothing here, and no planner yet reads the
/// ADR-0047 region map. `options` IS consumed (the issue-#175 opt-in); its
/// `Default` reproduces the proven-only gate byte for byte.
#[must_use]
pub fn plan_loop_to_array_map(
    db: &dyn Db,
    project: Project,
    vouches: &VouchSet,
    partitions: Option<&crate::regions::PartitionMap>,
    options: LoopToArrayMapOptions,
) -> TransformReport {
    let _ = (vouches, partitions);
    let files: Vec<SourceFile> = project.files(db).to_vec();

    // 1. Enumerate every `foreach` in the analyzed set (ADR-0076 §4).
    let mut candidates: Vec<Candidate> = Vec::new();
    for &file in &files {
        let path = file.path(db).to_owned();
        let tree = parse(db, file);
        let source = file.text(db);
        for site in tree.foreach_sites() {
            candidates.push(Candidate { path: path.clone(), tree, source, site });
        }
    }

    // 2. Batch the two engine queries: one whole-project pass each, not one per loop.
    let probe_sites: Vec<(String, u32, String)> = candidates
        .iter()
        .filter_map(|c| {
            c.site.subject.as_ref().map(|v| (c.path.clone(), c.site.span.start, v.clone()))
        })
        .collect();
    let subjects = probe_subjects(db, project, &probe_sites);
    let regions: Vec<(String, u32, u32)> =
        candidates.iter().map(|c| (c.path.clone(), c.site.span.start, c.site.span.end)).collect();
    let purities = region_purity_project(db, project, &regions);

    // 3. Decide each candidate: one edit, or exactly one named refusal.
    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut asserted_admissions: Vec<AssertedAdmission> = Vec::new();
    let mut oracle = CompletenessOracle::default();
    for (i, c) in candidates.iter().enumerate() {
        oracle.enumerated += 1;
        let site = c.site_ref();
        let subject = c
            .site
            .subject
            .as_ref()
            .map(|_| subjects.get(&(c.path.clone(), c.site.span.start)).copied().unwrap_or_default())
            .unwrap_or_default();
        let purity = purities.get(i).cloned().unwrap_or_default();
        match c.decide(&subject, &purity, options) {
            Ok((replacement, lane)) => {
                let span = ByteSpan::new(c.edit_start(), c.site.span.end);
                let edit = Edit { path: c.path.clone(), span, replacement };
                // Overlap is an invariant break, surfaced as a refusal (never a panic).
                if plan.add_edit(edit).is_ok() {
                    oracle.transformed += 1;
                    if lane == SubjectLane::Asserted {
                        oracle.transformed_asserted += 1;
                        let subj = c.site.subject.as_deref().unwrap_or_default();
                        asserted_admissions
                            .push(AssertedAdmission::new(site, asserted_label(subj)));
                    }
                } else {
                    oracle.refused += 1;
                    refusals.push(Refusal::new(
                        site,
                        REASON_ACCUMULATOR_INIT_NOT_ADJACENT,
                        "internal: the rewrite's span overlapped another edit; skipped",
                    ));
                }
            }
            Err((reason, detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, reason, detail));
            }
        }
    }

    TransformReport {
        plan,
        refusals,
        oracle,
        obstacles: Vec::new(),
        vouched_exemptions: Vec::new(),
        asserted_admissions,
    }
}

/// One enumerated `foreach`, with the file context its gates need.
struct Candidate<'a> {
    path: String,
    tree: &'a SourceTree,
    source: &'a str,
    site: &'a ForeachSite,
}

impl Candidate<'_> {
    /// The refusal / audit position: the `foreach` keyword.
    fn site_ref(&self) -> SiteRef {
        let p = self.tree.position(self.site.span.start);
        SiteRef::new(self.path.clone(), p.line, p.column, "foreach".to_owned())
    }

    /// Where the rewrite's replacement span begins: the accumulator
    /// initializer, consumed together with the loop. Only called once the
    /// adjacency gate has passed, so the initializer is present.
    fn edit_start(&self) -> u32 {
        self.site.prev_stmt.as_ref().map_or(self.site.span.start, |p| p.span.start)
    }

    /// The gate sequence, fixed order: shape gates first, then parity, then
    /// purity. Returns the replacement text plus the admitting trust lane, or
    /// the one named reason this loop is refused for.
    fn decide(
        &self,
        subject_fact: &SubjectFact,
        purity: &RegionPurity,
        options: LoopToArrayMapOptions,
    ) -> Result<(String, SubjectLane), (&'static str, String)> {
        let s = self.site;

        // ---- Shape (ADR-0076 §1) ------------------------------------------
        if s.key_binding {
            return Err((
                REASON_KEY_BINDING,
                "the loop binds a key (`as $k => $v`); array_map over one array passes only the value"
                    .to_owned(),
            ));
        }
        if s.by_ref_binding {
            return Err((
                REASON_REFERENCE_BINDING,
                "the loop binds by reference (`as &$v`), which writes back through the subject"
                    .to_owned(),
            ));
        }
        let Some(iter_var) = s.value_var.as_deref() else {
            return Err((
                REASON_VALUE_BINDING_NOT_VARIABLE,
                "the value target is not a plain variable (a destructuring binding has no arrow-function parameter spelling)"
                    .to_owned(),
            ));
        };
        let Some(subject) = s.subject.as_deref() else {
            return Err((
                REASON_SUBJECT_NOT_VARIABLE,
                "the iterated expression is not a plain variable".to_owned(),
            ));
        };
        if s.body.early_exit {
            return Err((
                REASON_EARLY_EXIT,
                "the body can end the loop early (break / continue / return / goto)".to_owned(),
            ));
        }
        let Some(append) = s.body.append.as_ref() else {
            return Err((
                REASON_BODY_NOT_SINGLE_APPEND,
                format!(
                    "the body is not exactly one `$acc[] = <expr>;` statement ({} statement(s))",
                    s.body.stmt_count
                ),
            ));
        };
        if append.value_writes {
            return Err((
                REASON_BODY_NOT_SINGLE_APPEND,
                "the appended expression writes a variable (assignment / `++` / `--`); `fn` captures by value, so the write would not carry"
                    .to_owned(),
            ));
        }

        // ---- Accumulator and its initializer (ADR-0076 §1/§3) --------------
        let acc = append.acc.as_str();
        let Some(prev) = s.prev_stmt.as_ref() else {
            return Err((
                REASON_ACCUMULATOR_INIT_NOT_ADJACENT,
                format!("`${acc} = [];` does not immediately precede the loop (the loop opens its block)"),
            ));
        };
        if prev.assign_target.as_deref() != Some(acc) {
            return Err((
                REASON_ACCUMULATOR_INIT_NOT_ADJACENT,
                format!("the statement before the loop does not initialize `${acc}`"),
            ));
        }
        if !prev.assigns_empty_array {
            return Err((
                REASON_ACCUMULATOR_NOT_EMPTY,
                format!("`${acc}` is initialized to something other than an empty array literal"),
            ));
        }
        if !gap_is_whitespace(self.source, prev.span.end, s.span.start) {
            return Err((
                REASON_ACCUMULATOR_INIT_NOT_ADJACENT,
                format!(
                    "the gap between `${acc} = [];` and the loop is not whitespace-only (the rewrite would consume it)"
                ),
            ));
        }
        // Accumulator may occur ONLY as the append target, else each element
        // clobbers it (read in the expression, or bound as iteration var).
        if append.value_vars.iter().any(|v| v == acc) || iter_var == acc || subject == acc {
            return Err((
                REASON_ACCUMULATOR_READ_IN_BODY,
                format!("`${acc}` occurs in the loop somewhere other than as the append target"),
            ));
        }

        // ---- Iteration-variable liveness (ADR-0076 §3) ---------------------
        if let Some(tail) = self.source.get(s.span.end as usize..s.scope_end as usize)
            && mentions_var(tail, iter_var)
        {
            return Err((
                REASON_ITERATION_VAR_LIVE_AFTER,
                format!(
                    "`${iter_var}` occurs after the loop; `foreach` leaves it bound to the last element, an arrow-function parameter does not"
                ),
            ));
        }

        // ---- Subject value facts (ADR-0076 §3, amended by issue #175) ------
        // `verified` is the lane wall: true = proven, false + array/list set =
        // Asserted (declared). Proven path is byte-identical opt-in on or off.
        let lane = if subject_fact.array && subject_fact.verified {
            if !subject_fact.list {
                return Err((
                    REASON_SUBJECT_NOT_PROVEN_LIST,
                    format!(
                        "`${subject}` is not proven `is_list = Yes`; array_map preserves keys while the append renumbers 0..n-1"
                    ),
                ));
            }
            SubjectLane::Proven
        } else if !options.asserted_subjects {
            // v1 reading unchanged: short of Verified array refuses at the
            // array gate, list unexamined (#145 check-order artifact).
            return Err((
                REASON_SUBJECT_NOT_PROVEN_ARRAY,
                format!(
                    "`${subject}` is not proven to be a plain array at the loop head (array_map TypeErrors on a Traversable)"
                ),
            ));
        } else if subject_fact.array {
            // Asserted evidence still needs the list half at the same stratum
            // (condition 2 — `array`/`array<K, V>` proves the array half only).
            if !subject_fact.list {
                return Err((
                    REASON_SUBJECT_NOT_PROVEN_LIST,
                    format!(
                        "`${subject}`'s declared type establishes `array` at the Asserted stratum but not list-ness (`array` alone leaves the keys unknown); array_map preserves keys while the append renumbers 0..n-1"
                    ),
                ));
            }
            SubjectLane::Asserted
        } else {
            // No array evidence at either stratum — a bare `array $xs` lands
            // here too (native lowering has no `array` member, ADR-0002).
            return Err((
                REASON_SUBJECT_NOT_PROVEN_ARRAY,
                format!(
                    "`${subject}` is not proven to be a plain array at the loop head, and no declared type establishes one either — the opt-in admits declared evidence, not its absence (array_map TypeErrors on a Traversable)"
                ),
            ));
        };

        // ---- The purity bar (ADR-0076 §2) ----------------------------------
        if !purity.labels.is_empty() {
            return Err((
                REASON_BODY_EFFECTS,
                format!("the body's proven effect lane is non-empty: {{{}}}", purity.labels.join(", ")),
            ));
        }
        if !purity.throws.is_empty() {
            return Err((
                REASON_BODY_THROWS,
                format!(
                    "the body's proven throw set is non-empty: {{{}}} — a throw on element k leaves the accumulator holding the first k results",
                    purity.throws.join(", ")
                ),
            ));
        }
        if append.value_unmodelled {
            return Err((
                REASON_BODY_CALL_UNRESOLVED,
                "the appended expression carries a construct the effect scan does not model (`new` / `clone` / `yield` / backticks / a poison construct / a frame-sensitive builtin)"
                    .to_owned(),
            ));
        }
        if !purity.declared.is_empty() {
            // ADR-0067's lane wall: a `≤` bound is a cap, not an occurrence
            // proof, so it cannot witness the equivalence of two orders.
            return Err((
                REASON_BODY_CALL_UNRESOLVED,
                format!(
                    "a call in the body is answered only by a declared bound {{{}}} — a cap, not an occurrence proof (ADR-0067)",
                    purity.declared.join(", ")
                ),
            ));
        }
        if !purity.exhaustive || !purity.throws_exhaustive {
            return Err((
                REASON_BODY_CALL_UNRESOLVED,
                "a call in the body did not resolve, so its effects/throws are unknown".to_owned(),
            ));
        }

        Ok((self.rewrite(acc, iter_var, subject, append.value_span), lane))
    }

    /// The replacement text for `[initializer, loop]` — one statement. No
    /// parameter type is written on the arrow function: inventing one could
    /// fail at runtime on inputs the engine never saw (ADR-0076 §3).
    fn rewrite(&self, acc: &str, iter_var: &str, subject: &str, value: Span) -> String {
        let expr = &self.source[value.start as usize..value.end as usize];
        format!("${acc} = array_map(fn (${iter_var}) => {expr}, ${subject});")
    }
}

/// Whether the bytes between two statements are whitespace only — a comment
/// there makes the statements non-adjacent, since consuming both spans would
/// delete it.
fn gap_is_whitespace(source: &str, from: u32, to: u32) -> bool {
    source
        .get(from as usize..to as usize)
        .is_some_and(|gap| gap.chars().all(char::is_whitespace))
}

/// Whether `text` mentions `$name` as a whole variable name (ADR-0076 §3
/// liveness check) — textual, and deliberately so: it can only over-report,
/// and over-reporting refuses. Whole-name matching keeps `$xs` from answering
/// for `$x`; interpolated forms (`"$x"`, `"{$x}"`) match, but indirect ones
/// (`$$v`, `${'x'}`, `compact('x')`) are each an ADR-0001 poison construct,
/// refusing at the subject gate instead.
fn mentions_var(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = text[from..].find('$') {
        let at = from + rel + 1;
        from = at;
        if !text[at..].starts_with(name) {
            continue;
        }
        let after = at + name.len();
        let boundary = bytes.get(after).is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
        if boundary {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_name_matching_does_not_confuse_prefixes() {
        assert!(mentions_var("echo $x;", "x"));
        assert!(!mentions_var("echo $xs;", "x"));
        assert!(mentions_var("echo \"{$x}\";", "x"));
        assert!(mentions_var("f($x, $y);", "y"));
        assert!(!mentions_var("f($xy);", "x"));
        assert!(!mentions_var("// nothing here", "x"));
    }

    #[test]
    fn a_comment_in_the_gap_is_not_whitespace() {
        assert!(gap_is_whitespace("a;\n    b;", 2, 7));
        assert!(!gap_is_whitespace("a;\n// c\nb;", 2, 8));
    }
}
