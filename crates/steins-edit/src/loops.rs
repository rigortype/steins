//! Transform #3 — loop→`array_map` under a **proven-purity** precondition
//! (ADR-0076, issue #116).
//!
//! The flagship transform of ADR-0010: the rewrite
//!
//! ```php
//! $out = [];
//! foreach ($xs as $x) {
//!     $out[] = f($x);
//! }
//! // becomes
//! $out = array_map(fn ($x) => f($x), $xs);
//! ```
//!
//! is trivial to *spell* and impossible for a rule-driven codemod to *justify*:
//! its precondition is a whole-project effect judgment. This module asks the
//! engine for that judgment and refuses, by name, everywhere it is not answered.
//!
//! ## The bar: proven, and strictly stronger than ADR-0006 `Pure`
//!
//! 1. The body's **proven** effect lane must be empty on every label, and the
//!    exhaustiveness bit intact — an unresolved call refuses
//!    [`REASON_BODY_CALL_UNRESOLVED`] rather than being assumed harmless.
//! 2. **Declared bounds never qualify** (ADR-0067). The probe
//!    ([`steins_infer::region_purity_project`]) keeps the two lanes apart, and
//!    this transform reads a non-empty **declared** lane as *unproven*: a `≤`
//!    envelope imported at an interface-typed receiver, a plugin coloring, or a
//!    conditional-purity contract refuses [`REASON_BODY_CALL_UNRESOLVED`]. This
//!    is not decoration — the effect pass deliberately *discharges* the
//!    exhaustiveness taint at a call a declared receiver answered, so a gate
//!    reading exhaustiveness alone would admit exactly the bounds ADR-0067 built
//!    the lane wall to keep out.
//! 3. The **proven throw set must be empty**, which `Pure` does not require. A
//!    body that throws on element `k` leaves `$out` holding the first `k`
//!    results — observable in every enclosing `catch` — while the rewrite's
//!    all-or-nothing assignment leaves `$out` unassigned. The probe therefore
//!    evaluates the region's throw origins with the **enclosing** guards
//!    stripped: an enclosing `catch` is precisely the observer that tells the
//!    two spellings apart.
//!
//! ## Parity gates beyond purity
//!
//! - The subject must prove `array` **and** `is_list = Yes` at the loop head, at
//!   the `Verified` stratum: `array_map` over a single array *preserves keys*
//!   while `$out[] = …` renumbers `0..n-1`. A docblock-asserted shape is a claim,
//!   not a proof, and refuses.
//! - The iteration variable must not occur after the loop (`foreach` leaks it,
//!   the arrow function does not). v1 scans the remainder of the enclosing scope
//!   textually — sound in the refusing direction.
//! - The accumulator may occur **only** as the append target, anywhere in the
//!   whole `foreach` statement, and its `$out = [];` initializer must be the
//!   immediately preceding statement with a whitespace-only gap. The rewrite
//!   consumes both statements, so a comment in that gap refuses rather than
//!   being eaten.
//!
//! ## Enumeration domain
//!
//! **Every** `foreach` statement in the analyzed set is a candidate
//! ([`steins_syntax::SourceTree::foreach_sites`]). Enumerating only
//! append-shaped loops would hide exactly the narrowness the completeness oracle
//! exists to expose; the refusal distribution over the whole construct family is
//! the roadmap for v2 (ADR-0076 §6).

use steins_db::{Db, Project, SourceFile, parse};
use steins_infer::{RegionPurity, SubjectFact, probe_subjects, region_purity_project};
use steins_syntax::{ForeachSite, SourceTree, Span};

use crate::obstacles::VouchSet;
use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};

// ---- Stable refusal reason names (ADR-0034 point 2 / ADR-0076 §4) ----------

/// `foreach ($xs as $k => $v)` — `array_map` over a single array passes only the
/// value to the callback, so the key form has no equivalent v1 spelling.
pub const REASON_KEY_BINDING: &str = "key-binding";
/// `foreach ($xs as &$v)` — a by-reference binding writes back through the
/// subject; `array_map` builds a new array and writes nothing.
pub const REASON_REFERENCE_BINDING: &str = "reference-binding";
/// `foreach ($xs as [$a, $b])` / `as list($a, $b)` — a destructuring binding.
/// A real PHP shape the ADR-0076 §4 list does not name, added rather than
/// mislabeled: arrow functions cannot spell a destructuring parameter, and since
/// every `foreach` is a candidate the shape needs exactly one honest reason.
pub const REASON_VALUE_BINDING_NOT_VARIABLE: &str = "value-binding-not-variable";
/// The iterated expression is not a plain variable (a call, a property read, an
/// offset read). v1 moves the subject verbatim into the `array_map` argument, so
/// only a bare variable is guaranteed to evaluate the same once as it did once.
pub const REASON_SUBJECT_NOT_VARIABLE: &str = "subject-not-variable";
/// The subject's value fact does not prove a plain `array` at the loop head, at
/// the `Verified` stratum. `foreach` iterates any `Traversable`; `array_map`
/// `TypeError`s on one.
pub const REASON_SUBJECT_NOT_PROVEN_ARRAY: &str = "subject-not-proven-array";
/// The subject proves `array` but not `is_list = Yes`. `array_map` with a single
/// array preserves keys while the append renumbers `0..n-1`, so a non-list
/// subject changes the result's keys. Wrapping in `array_values(...)` is
/// rejected by ADR-0076 §6: a non-list subject is a refusal v1 reports, not a
/// shape it quietly launders.
pub const REASON_SUBJECT_NOT_PROVEN_LIST: &str = "subject-not-proven-list";
/// The accumulator's `$out = [];` initializer is not the statement immediately
/// preceding the loop (or the gap between them holds more than whitespace).
pub const REASON_ACCUMULATOR_INIT_NOT_ADJACENT: &str = "accumulator-init-not-adjacent";
/// The adjacent initializer assigns something other than an empty array literal.
/// `array_merge($out, array_map(...))` is a compound rewrite with a compound
/// proof obligation — a later slice, not a v1 shape (ADR-0076 §6).
pub const REASON_ACCUMULATOR_NOT_EMPTY: &str = "accumulator-not-empty";
/// The accumulator occurs somewhere in the loop other than as the append target
/// — read inside the appended expression, or bound as the iteration variable.
/// Either way the loop observes its own partial state, which an all-at-once
/// assignment cannot reproduce.
pub const REASON_ACCUMULATOR_READ_IN_BODY: &str = "accumulator-read-in-body";
/// The iteration variable occurs after the loop. `foreach` leaves it bound to
/// the last element; an arrow function's parameter does not escape.
pub const REASON_ITERATION_VAR_LIVE_AFTER: &str = "iteration-var-live-after";
/// The body carries a `break` / `continue` / `return` / `goto`: the loop can end
/// early, and a whole-array map cannot.
pub const REASON_EARLY_EXIT: &str = "early-exit";
/// The body is not exactly one `$out[] = <expr>;` statement — it is empty, holds
/// several statements, or its single statement is not a plain append.
///
/// It also covers an append expression that **writes** a variable (an embedded
/// assignment, `++`, `--`). Function-local mutation carries no effect-lane label,
/// but `fn` captures by value, so `$out[] = $i++ . $x` is not equivalence-
/// preserving: ADR-0076 §1's shape lets the expression *read* freely, and a write
/// is outside it.
pub const REASON_BODY_NOT_SINGLE_APPEND: &str = "body-not-single-append";
/// The body's **proven** effect lane is non-empty. The detail names the labels.
pub const REASON_BODY_EFFECTS: &str = "body-effects";
/// The body's **proven** throw set is non-empty. The detail names the classes.
/// Stricter than ADR-0006 `Pure`, which admits `throw` — see the module docs.
pub const REASON_BODY_THROWS: &str = "body-throws";
/// A call in the body did not resolve, so its effects and throws are unknown: a
/// dynamic callee, an opaque receiver, an unanalyzable target — or a construct
/// the effect scan does not model at all (`new`, `clone`, `yield`, a backtick
/// shell execute, an ADR-0001 poison construct, or a frame-sensitive builtin such
/// as `compact` / `get_defined_vars` / `func_get_args` / `func_num_args`, whose
/// meaning changes inside the arrow function's scope). A call answered only by a
/// **declared** bound lands here too (ADR-0067: a cap is not an occurrence
/// proof).
pub const REASON_BODY_CALL_UNRESOLVED: &str = "body-call-unresolved";

/// The loop→`array_map` transform (ADR-0076).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopToArrayMap;

impl Transform for LoopToArrayMap {
    fn id(&self) -> &'static str {
        "loop-to-array-map"
    }
}

/// Plan the loop→`array_map` rewrite over `project`. Pure planning: no files are
/// written and no diagnostics are re-checked here — the caller (CLI) drives the
/// dry-run diff, ADR-0034's dual-verification post-check, and any `--apply`
/// write.
///
/// `vouches` and `partitions` are accepted for signature parity with the other
/// transforms and are **not consumed**: this transform enumerates no callers, so
/// ADR-0046 §2's caller-enumeration obstacles (`eval`, dynamic `include`) decide
/// nothing here, and ADR-0047's region map decides nothing in any planner yet.
/// The plan is identical whatever either argument holds.
#[must_use]
pub fn plan_loop_to_array_map(
    db: &dyn Db,
    project: Project,
    vouches: &VouchSet,
    partitions: Option<&crate::regions::PartitionMap>,
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

    // 2. Batch the two engine queries so their whole-project passes run once for
    //    the whole run rather than once per loop.
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
        match c.decide(&subject, &purity) {
            Ok(replacement) => {
                let span = ByteSpan::new(c.edit_start(), c.site.span.end);
                let edit = Edit { path: c.path.clone(), span, replacement };
                // Overlap rejection is the plan's job; a rejection here is an
                // internal invariant break, surfaced as a refusal (never a panic).
                if plan.add_edit(edit).is_ok() {
                    oracle.transformed += 1;
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

    /// Where the rewrite's replacement span begins: the accumulator initializer,
    /// which the rewrite consumes together with the loop. Only ever called on a
    /// candidate whose adjacency gate passed, so the initializer is present.
    fn edit_start(&self) -> u32 {
        self.site.prev_stmt.as_ref().map_or(self.site.span.start, |p| p.span.start)
    }

    /// The whole gate sequence, in a fixed order: the shape gates first (a
    /// judgment about what is *written*), then the parity gates, then the purity
    /// bar (a judgment about what the code *does*). Returns the replacement text,
    /// or the one named reason this loop is refused for.
    fn decide(
        &self,
        subject_fact: &SubjectFact,
        purity: &RegionPurity,
    ) -> Result<String, (&'static str, String)> {
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
        // The accumulator may occur ONLY as the append target — not read in the
        // appended expression, and not bound as the loop's own iteration
        // variable (which would make each element clobber the accumulator).
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

        // ---- Subject value facts (ADR-0076 §3) -----------------------------
        if !subject_fact.array || !subject_fact.verified {
            return Err((
                REASON_SUBJECT_NOT_PROVEN_ARRAY,
                format!(
                    "`${subject}` is not proven to be a plain array at the loop head (array_map TypeErrors on a Traversable)"
                ),
            ));
        }
        if !subject_fact.list {
            return Err((
                REASON_SUBJECT_NOT_PROVEN_LIST,
                format!(
                    "`${subject}` is not proven `is_list = Yes`; array_map preserves keys while the append renumbers 0..n-1"
                ),
            ));
        }

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
            // ADR-0067's lane wall, enforced at its first transform consumer. The
            // effect pass *discharges* the exhaustiveness taint at a call a
            // declared receiver answered, so reading `exhaustive` alone would let
            // a `≤` bound through as if it were proof. A cap cannot witness the
            // equivalence of two evaluation orders, so it refuses.
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

        Ok(self.rewrite(acc, iter_var, subject, append.value_span))
    }

    /// The replacement text for `[initializer, loop]` — one statement.
    ///
    /// No parameter type is written on the arrow function: inventing one could
    /// fail at runtime on inputs the engine never saw (ADR-0076 §3).
    fn rewrite(&self, acc: &str, iter_var: &str, subject: &str, value: Span) -> String {
        let expr = &self.source[value.start as usize..value.end as usize];
        format!("${acc} = array_map(fn (${iter_var}) => {expr}, ${subject});")
    }
}

/// Whether the bytes between two statements are whitespace only. A comment there
/// makes the two statements non-adjacent for the rewrite's purposes: consuming
/// both spans would delete it.
fn gap_is_whitespace(source: &str, from: u32, to: u32) -> bool {
    source
        .get(from as usize..to as usize)
        .is_some_and(|gap| gap.chars().all(char::is_whitespace))
}

/// Whether `text` mentions `$name` as a whole variable name. The v1 iteration-
/// variable liveness check (ADR-0076 §3) — textual, and deliberately so: it can
/// only over-report, and over-reporting refuses.
///
/// Whole-name matching keeps `$xs` from answering for `$x`. The interpolated
/// forms (`"$x"`, `"{$x}"`) match; the indirect ones (`$$v`, `${'x'}`,
/// `compact('x')`) do not — but every one of those is an ADR-0001 poison
/// construct, which leaves the scope's values unknown and so refuses at the
/// subject gate instead.
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
