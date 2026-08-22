//! The offset family (ADR-0049 §7 / S3, ADR-0062): `offset.missing` /
//! `offset.on-unsupported` on the value lane, the shape-aware read row, and the
//! STRICT leg `offset.undeclared` / `offset.maybe-missing` (ADR-0062 S6 / A-G10,
//! issue #51).

use std::collections::HashMap;

use steins_domain::{Fact, PhpStr, ShapeFact, Key as VKey, Val};
use steins_syntax::{ArgValue, CallExpr, Span, php_canonical_int_string};

use crate::assign::{coalesce_projection, cover_discharges, flatten_coalesce};
use crate::cx::Cx;
use crate::dump::render_shape_fact;
use crate::env::{Known, Store, Stratum, render_val, singleton_fact};
use crate::project::Diagnostic;
use crate::refine::seed_shape_fact;
use crate::return_arms::call_return_arms;
use crate::shapes::{array_has_key, base_fact_val, emit_offset};
use crate::walk::WalkCx;
use crate::fold::Folder;
use crate::{
    OFFSET_MAYBE_MISSING_ID, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, OFFSET_UNDECLARED_ID,
};

// ---------------------------------------------------------------------------
// The offset family: `offset.missing` / `offset.on-unsupported` (ADR-0049 §7 / S3).
//
// A value-domain absence proof: a read `$base[$key]` provably emits an
// `E_WARNING` because the whole container value is known (a Verified
// `Singleton`/all-array `OneOf`) and the key is provably absent, or the base
// is a proven non-offsetable scalar/null. Value-domain evidence only (§7):
// `General`/`Refined`, objects, string bases, and any non-`Verified` fact
// (N2) are silent.
//
// **Read-context whitelist (A7).** Called ONLY from a plain assignment-RHS,
// a return operand, and the SOURCE of a destructuring assignment (issue
// #288). Every silence context (`isset`/`??`/`array_key_exists`/`unset`,
// write lvalues, by-ref/unresolved-callee argument positions, array
// elements) never reaches here by construction of the lowering. Destructure
// source: `[$a, $b] = $m;` reads `$m[0]`/`$m[1]` (witnessed PHP 8.5.9,
// `Undefined array key`); keys are PHP's own — positional (a hole `[, $b]`
// skips its index), explicit for `['a' => $x] = $m`, nested patterns
// recurse; TARGETS stay silent (write positions, ADR-0049/0052 audit note
// G7(e)). See [`StmtKind::Destructure`] / [`check_destructure_source`].
//
// v1 scope (deferred, all safe silence): Error-grade object case (needs
// ArrayAccess is-a), TypeError string-key-on-string, string-base offset
// reads, call-argument read position (autovivification, A7), compound-
// assignment read half, destructure source below the first pattern level.
// ---------------------------------------------------------------------------

/// Severity grade of an offset finding (ADR-0049 §7 verified table). The
/// `warning-handler` posture gates only [`Self::Warning`]; [`Self::Fatal`]
/// (object `Error` / string-key `TypeError`) is unimplemented, so every
/// finding here is currently `Warning`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetGrade {
    Warning,
    #[allow(dead_code)]
    Fatal,
}

/// Canonicalize a proven key [`Val`] to a domain array key (ADR-0049 A10),
/// reusing the SAME [`php_canonical_int_string`] primitive as the
/// write/lowering side — `$a = [5 => 'x']; $a["5"]` resolves to key `5`,
/// while `"05"`/`"+5"` stay strings. `None` for an array key (a distinct
/// TypeError, out of scope here) or a non-finite float.
pub(crate) fn offset_key_of(v: &Val) -> Option<VKey> {
    match v {
        Val::Int(i) => Some(VKey::Int(*i)),
        Val::Bool(b) => Some(VKey::Int(i64::from(*b))),
        Val::Null => Some(VKey::Str(PhpStr::new())),
        #[allow(clippy::cast_possible_truncation)]
        Val::Float(f) if f.is_finite() => Some(VKey::Int(f.trunc() as i64)),
        Val::Str(s) => Some(match php_canonical_int_string(s) {
            Some(i) => VKey::Int(i),
            None => VKey::Str(s.clone()),
        }),
        Val::Float(_) | Val::Array(_) => None,
    }
}

/// The proven `Verified` value-domain fact for an offset-read operand (base
/// or key), or `None` when unproven, poisoned, or below the proof stratum
/// (N2). A bare `Var` reads the env (requiring `Verified`); a literal/fully-
/// literal array resolves directly. Every other form is unproven.
pub(crate) fn offset_operand_fact(
    arg: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<Fact> {
    match arg {
        ArgValue::Var(name) => {
            if poisoned {
                return None;
            }
            let k = env.get(name)?;
            (k.stratum == Stratum::Verified).then(|| k.fact.clone()).flatten()
        }
        _ => singleton_fact(arg, php_minor),
    }
}

/// The PHP type word for an offset read on a proven non-offsetable scalar/null base
/// (verified PHP 8.5.8: `Trying to access array offset on null|int|float|true|false`).
/// `None` for a string base (offsetable — deferred) or any array.
fn unsupported_base_word(v: &Val) -> Option<&'static str> {
    match v {
        Val::Null => Some("null"),
        Val::Int(_) => Some("int"),
        Val::Float(_) => Some("float"),
        Val::Bool(true) => Some("true"),
        Val::Bool(false) => Some("false"),
        Val::Str(_) | Val::Array(_) => None,
    }
}

/// Render a canonical key in Steins' own phrasing (`0`, `'foo'`) for the evidence
/// clause, and in PHP's verbatim phrasing (`0`, `"foo"`) for the quoted consequence.
fn render_offset_key(k: &VKey) -> (String, String) {
    match k {
        VKey::Int(i) => (i.to_string(), i.to_string()),
        VKey::Str(s) => (s.render_with('\''), s.render_with('"')),
    }
}

/// Judge a single whitelisted offset read `base[key]` and emit at most one finding
/// (ADR-0049 §7 / S3). `span` locates the diagnostic (the enclosing statement).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_offset_read(
    cx: &Cx,
    folder: &mut dyn Folder,
    base: &ArgValue,
    key: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    // A9 (global): silent without a live, monkey-patch-free sidecar.
    if !folder.absence_family_available() {
        return;
    }
    // Legs (b)/(e): base must be a proven `Verified` whole value (N2). An
    // object base (fact `None`, state lives in the heap) is silent — the
    // ArrayAccess/non-ArrayAccess object split is the deferred case.
    let Some(base_fact) = offset_operand_fact(base, env, poisoned, cx.php_minor) else {
        return;
    };

    // Case 1 — proven non-offsetable scalar/null base (`offset.on-unsupported`,
    // warning-grade): warns regardless of key. Only `Singleton` fires.
    if let Fact::Singleton(v) = &base_fact
        && let Some(word) = unsupported_base_word(v)
    {
        emit_offset(
            cx,
            span,
            OFFSET_ON_UNSUPPORTED_ID,
            OffsetGrade::Warning,
            format!(
                "offset read on {} — provably {word}; reads null with \"Trying to access array offset on {word}\"",
                base.render(),
            ),
            out,
        );
        return;
    }

    // Case 2 — container base (`offset.missing`, warning-grade): key must be
    // a proven single value (leg (c)), canonicalized via the shared helper (A10).
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned, cx.php_minor)
    else {
        return;
    };
    let Some(canon) = offset_key_of(&key_val) else {
        return;
    };

    let (our_key, php_key) = render_offset_key(&canon);
    match &base_fact {
        // A single proven array (including `Singleton([])` from an `=== []` guard):
        // key absence is definite (leg (b)).
        Fact::Singleton(Val::Array(entries)) => {
            if !array_has_key(entries, &canon) {
                emit_offset(
                    cx,
                    span,
                    OFFSET_MISSING_ID,
                    OffsetGrade::Warning,
                    format!(
                        "offset {our_key} provably missing — {} is {} on this path; reads null with \"Undefined array key {php_key}\"",
                        base.render(),
                        render_val(&base_fact_val(&base_fact)),
                    ),
                    out,
                );
            }
        }
        // A `OneOf` fires only when EVERY member is an array and none carries the key
        // (leg (b), the join rule): any member with the key — or any non-array member
        // — is silence.
        Fact::OneOf(members) => {
            let all_arrays_missing = members.iter().all(|m| {
                matches!(m, Val::Array(entries) if !array_has_key(entries, &canon))
            });
            if all_arrays_missing {
                emit_offset(
                    cx,
                    span,
                    OFFSET_MISSING_ID,
                    OffsetGrade::Warning,
                    format!(
                        "offset {our_key} provably missing — {} is one of {} proven arrays, none carrying the key; reads null with \"Undefined array key {php_key}\"",
                        base.render(),
                        members.len(),
                    ),
                    out,
                );
            }
        }
        // `Refined`/`General` (no proven whole value), a string base, or anything
        // else: silent (§7 value-domain-only provability).
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Shape-aware reads (ADR-0062 §4's read row, S3).
//
// The value-lane sibling of `check_offset_read`: where that function judges a
// PROVEN whole container for an absence FINDING, this one answers "what does
// the declaration say this key holds" against the abstract stratum, and
// emits nothing. The two never overlap — a `Fact::Shape` base falls into
// `check_offset_read`'s silent `_` arm, and an `Asserted` shape seed is
// invisible to its `Verified`-only operand gate (A-G9's corollary: shape-
// derived facts never feed proof-layer findings).
// ---------------------------------------------------------------------------

/// What a constant-key read against an abstract shape found (ADR-0062 §4).
/// The three no-fact outcomes are deliberately distinct even though S3
/// renders them identically (unknown, silent): they are the finding ladder
/// S6 wires (A-G10) — [`Self::MaybeMissing`] is `offset.maybe-missing`'s
/// site, [`Self::DeclaredAbsent`] is `offset.undeclared`'s.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShapeRead {
    /// A `Required` field: its value slot (`None` is the honest floor, A-G1a).
    Present(Option<Fact>),
    /// An `Optional` field, undischarged: **no fact**, never the slot's value
    /// ∪ null (A-G9 — reads are never null-poisoned). The declared slot rides
    /// along but [`Self::into_fact`] doesn't yield it; its ONE consumer is
    /// [`ShapeRead::taken_fact`] (the `??` left-arm reading, where missing-
    /// ness is fall-through, not a value, A-G11).
    MaybeMissing(Option<Fact>),
    /// The declaration proves the key is not there: an `Absent` field, a key
    /// outside a `Sealed` shape's fields, or a key the tail's key class rejects.
    DeclaredAbsent,
    /// An undeclared key admitted by an `Unsealed` tail: the tail's value bound.
    Tail(Option<Fact>),
}

impl ShapeRead {
    /// The fact this read yields, if any. Every no-fact outcome collapses
    /// here — the distinction above is for S6's emitters, not the value lane.
    pub(crate) fn into_fact(self) -> Option<Fact> {
        match self {
            ShapeRead::Present(f) | ShapeRead::Tail(f) => f,
            ShapeRead::MaybeMissing(_) | ShapeRead::DeclaredAbsent => None,
        }
    }

    /// The value this read yields **when a `??` takes the arm** (S5, A-G11).
    /// A non-final `??` arm is used only when `isset` holds of it, so
    /// missing-ness is fall-through: an undischarged optional field still
    /// yields its declared slot; a declared absence yields nothing. Not a
    /// weakening of A-G9 — the fact never reaches a read surface, only the
    /// `??` join, where PHP says the arm's value is what the expression has.
    pub(crate) fn taken_fact(self) -> Option<Fact> {
        match self {
            ShapeRead::Present(f) | ShapeRead::Tail(f) | ShapeRead::MaybeMissing(f) => f,
            ShapeRead::DeclaredAbsent => None,
        }
    }
}

/// Read one canonical key out of an abstract shape (ADR-0062 §4).
pub(crate) fn shape_read(shape: &ShapeFact, key: &VKey) -> ShapeRead {
    use steins_domain::{Presence, Tail};
    match shape.field(key) {
        // The witness bit is provenance, not extension: a declared-Required
        // and a guard-Verified key read the same value.
        Some((_, Presence::Required { .. }, slot)) => {
            ShapeRead::Present(slot.as_deref().cloned())
        }
        Some((_, Presence::Optional, slot)) => {
            ShapeRead::MaybeMissing(slot.as_deref().cloned())
        }
        Some((_, Presence::Absent, _)) => ShapeRead::DeclaredAbsent,
        None => match &shape.tail {
            Tail::Sealed => ShapeRead::DeclaredAbsent,
            Tail::Unsealed { key: class, value } if class.admits_key(key) => {
                ShapeRead::Tail(value.as_deref().cloned())
            }
            // The tail's key class excludes this key, so no admitted value has it.
            Tail::Unsealed { .. } => ShapeRead::DeclaredAbsent,
        },
    }
}

/// Resolve a read site `base[key]` to the **shape and canonical key** it
/// names, or decline. The one resolver both the value lane
/// ([`shape_read_at`]) and the strict leg's emitters (S6) go through, so a
/// read and a finding can never disagree about which field they mean.
///
/// Declines (`None`) when the base carries no shape fact, is **nullable**
/// (no field is guaranteed then; narrowing that is S4's job), or the key is
/// not a proven single value.
fn shape_site_at<'a>(
    base: &ArgValue,
    key: &ArgValue,
    env: &'a HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<(&'a ShapeFact, VKey, Stratum)> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = base else { return None };
    let known = env.get(name)?;
    let Some(Fact::Shape { shape, nullable: false }) = &known.fact else { return None };
    // The key resolution is the offset family's, unchanged: a literal or a
    // `Verified` proven value, canonicalized by PHP's own key rule (A10).
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned, php_minor) else {
        return None;
    };
    let canon = offset_key_of(&key_val)?;
    Some((shape, canon, known.stratum))
}

/// Resolve a read site `base[key]` against the abstract stratum, plus the
/// stratum the result inherits.
pub(crate) fn shape_read_at(
    base: &ArgValue,
    key: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<(ShapeRead, Stratum)> {
    let (shape, canon, stratum) = shape_site_at(base, key, env, poisoned, php_minor)?;
    // Derivation clause (ADR-0052 §5): the read consumes the base's fact, so the
    // result is no stronger than it — which is always `Asserted` for a shape.
    Some((shape_read(shape, &canon), stratum))
}

// ---------------------------------------------------------------------------
// The offset family's STRICT leg (ADR-0062 S6 / A-G10, issue #51).
//
// Two contract-layer ids, both at `Floor::Strict`, emitted from the SAME
// whitelisted read positions the proof leg uses (plain assignment-RHS,
// return operand) plus one the proof leg does not judge: the right-most arm
// of a `??` chain. Every finding reads a `Fact::Shape` — `Asserted` — so
// A-G9's corollary holds by construction: nothing below consults or
// produces a proof-layer fact/id.
//
// Silent, and why: `ShapeRead::Present` (proved clean, never by skip);
// `ShapeRead::Tail` (unsealed tail's value bound, OUT of v1 scope, A-G10, a
// future id mirroring PHPStan's two-flag split); every non-whitelisted read
// position (`isset`/`array_key_exists`/`unset` argument, write lvalue,
// array element, non-final `??` arm) never reaches these emitters (A7).
//
// **The `??` split** (issue #51 §2): PHP protects only the arms it may fall
// *through*; the right-most arm is a plain read under the premise `¬isset`
// of every arm to its left. Left arms stay silent everywhere; the final arm
// is judged under S5's accumulated premise ladder — clean, not a wolf cry.
// ---------------------------------------------------------------------------

/// Render the evidence + consequence clauses shared by both strict-leg ids:
/// what the declaration says, then what PHP does at runtime, quoted verbatim.
fn strict_leg_message(kind: &str, base: &str, shape: &ShapeFact, canon: &VKey) -> String {
    let (our_key, php_key) = render_offset_key(canon);
    let declared = render_shape_fact(shape, false);
    // `{base} is {declared}` mirrors `offset.missing`'s evidence clause. The
    // spelling is the shape fact's, which may differ from source text (a
    // sealed shape's head is canonicalized, issue #163 takes it from `is_list`)
    // — the same rendering the dump surface shows.
    match kind {
        "undeclared" => format!(
            "offset {our_key} is outside the declared shape — {base} is {declared}, which cannot carry the key; reads null with \"Undefined array key {php_key}\""
        ),
        "coalesce" => format!(
            "offset {our_key} may be missing on the final `??` arm — {base} is {declared}, which declares the key optional, and nothing to the left of this arm discharges it; reads null with \"Undefined array key {php_key}\""
        ),
        _ => format!(
            "offset {our_key} may be missing — {base} is {declared}, which declares the key optional, and no guard on this path discharges it; reads null with \"Undefined array key {php_key}\""
        ),
    }
}

/// Judge one whitelisted **plain** read `base[key]` against the declared
/// shape and emit at most one strict-leg finding (ADR-0062 S6).
///
/// Deliberately NOT gated on [`Folder::absence_family_available`], unlike
/// the proof leg: A9's gate exists because a monkey-patched runtime can
/// invalidate a *value-domain* absence proof. This leg's evidence is the
/// docblock, which no sidecar posture can move.
pub(crate) fn check_shape_read(
    cx: &Cx,
    base: &ArgValue,
    key: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let Some((shape, canon, _)) = shape_site_at(base, key, env, poisoned, cx.php_minor) else {
        return;
    };
    judge_shape_read(cx, shape, &canon, &base.render(), span, out);
}

/// The emitter half of [`check_shape_read`], taking the resolved site rather
/// than deriving it — so a read whose base is not a bare variable (the
/// destructure source of issue #288, whose base may be the call expression
/// itself) judges through the same ladder and message discipline.
fn judge_shape_read(
    cx: &Cx,
    shape: &ShapeFact,
    canon: &VKey,
    rendered: &str,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    match shape_read(shape, canon) {
        // Proven present (declared Required, or promoted by an S4 guard / S5 cover),
        // or an unsealed tail's value (out of v1 scope): silent.
        ShapeRead::Present(_) | ShapeRead::Tail(_) => {}
        ShapeRead::DeclaredAbsent => emit_offset(
            cx,
            span,
            OFFSET_UNDECLARED_ID,
            OffsetGrade::Warning,
            strict_leg_message("undeclared", rendered, shape, canon),
            out,
        ),
        // A plain read carries no `¬isset` premises — a discharged optional key is
        // already `Present` here (S4 promotion / S5 collapse), so reaching this arm
        // IS the "no proof on this path" statement.
        ShapeRead::MaybeMissing(_) => emit_offset(
            cx,
            span,
            OFFSET_MAYBE_MISSING_ID,
            OffsetGrade::Warning,
            strict_leg_message("maybe-missing", rendered, shape, canon),
            out,
        ),
    }
}

/// Judge the source of a destructuring assignment `[$a, $b] = <source>;` as
/// the read position it is (issue #288, ADR-0049 §7 A7 whitelist extended).
///
/// Both legs run, as at the assignment-RHS position: the proof leg
/// ([`check_offset_read`]) on a `Verified` whole container, the strict leg
/// ([`judge_shape_read`]) on the declared shape — disjoint via the usual
/// `Verified` vs. `Asserted` operand gates.
///
/// Two source spellings carry a shape, both judged: a bare variable (its env
/// fact) and a statically-named call (its declared-return arms, ADR-0069
/// floor). The call spelling is the one plain assignment never needs — there
/// the value is already bound to a name.
///
/// **Depth 1 only.** `reads` records the full key path of a nested pattern
/// (`[[$a], $b]` reads `$m[0]`, `$m[0][0]`, `$m[1]`, what PHP itself reads);
/// a path below the first level names an intermediate base neither leg can
/// resolve — same silence as a chained `$x = $m[0][1];` read.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_destructure_source(
    w: &WalkCx,
    folder: &mut dyn Folder,
    source: &ArgValue,
    call: Option<&CallExpr>,
    reads: &[Vec<ArgValue>],
    env: &HashMap<String, Known>,
    store: &Store,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let poisoned = w.scope.poisoned;
    // The call spelling's shape, resolved once for the whole pattern: a bare
    // variable is `check_shape_read`'s own site resolution and is left to it.
    let call_shape = match source {
        ArgValue::Var(_) => None,
        _ => call
            .and_then(|c| {
                call_return_arms(cx, c, store, w.this_exact, w.enclosing_class, poisoned)
            })
            .and_then(|arms| seed_shape_fact(&arms))
            // A nullable declared return may be null, so no field is guaranteed —
            // the same decline `shape_site_at` makes for a nullable base.
            .and_then(|fact| match fact {
                Fact::Shape { shape, nullable: false } => Some(*shape),
                _ => None,
            }),
    };
    for path in reads {
        let [key] = path.as_slice() else { continue };
        check_offset_read(cx, folder, source, key, env, poisoned, span, out);
        match (&call_shape, source) {
            (_, ArgValue::Var(_)) => check_shape_read(cx, source, key, env, poisoned, span, out),
            (Some(shape), _) => {
                // The key resolution is the offset family's own (A10), unchanged.
                let Some(Fact::Singleton(key_val)) =
                    offset_operand_fact(key, env, poisoned, cx.php_minor)
                else {
                    continue;
                };
                let Some(canon) = offset_key_of(&key_val) else { continue };
                judge_shape_read(cx, shape, &canon, &source.render(), span, out);
            }
            (None, _) => {}
        }
    }
}

/// Judge the **right-most arm** of a `??` chain in a whitelisted value
/// position (ADR-0062 S6, issue #51 §2). The arm walk mirrors
/// [`eval_coalesce_fact`]'s exactly — same projection test, premise
/// accumulation, invalidation, settle-and-stop — so the two agree on which
/// arm PHP evaluates. Only the final arm can produce a finding.
///
/// [`eval_coalesce_fact`]: crate::eval_coalesce_fact
pub(crate) fn check_coalesce_final_arm(
    cx: &Cx,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let mut arms: Vec<&ArgValue> = Vec::new();
    flatten_coalesce(value, &mut arms);
    let Some(last) = arms.len().checked_sub(1) else { return };

    let mut premises: Vec<(String, VKey)> = Vec::new();
    for (i, arm) in arms.iter().enumerate() {
        let projection = coalesce_projection(arm, env, poisoned, cx.php_minor);
        if i == last {
            let Some((var, canon)) = projection else { return };
            judge_coalesce_final(cx, &var, &canon, env, &premises, span, out);
            return;
        }
        match projection {
            Some((var, canon)) => {
                // A projection arm the shape proves present AND non-null IS the
                // value: `??` never evaluates anything to its right, so the final
                // arm is dead code and judging it would be a wolf cry.
                if coalesce_arm_settles(&var, &canon, env) {
                    return;
                }
                premises.push((var, canon));
            }
            // A non-projection arm may write through a reference or a global, so
            // every accumulated `¬isset` goes stale (A-G11's conservatism).
            None => premises.clear(),
        }
    }
}

/// Whether a non-final projection arm settles the chain — the emitter's reading of
/// [`eval_coalesce_fact`]'s `settled`, kept spelling-identical so the two lanes
/// cannot drift about which arms are reachable.
///
/// [`eval_coalesce_fact`]: crate::eval_coalesce_fact
fn coalesce_arm_settles(var: &str, key: &VKey, env: &HashMap<String, Known>) -> bool {
    let Some(known) = env.get(var) else { return false };
    let Some(Fact::Shape { shape, nullable: false }) = &known.fact else { return false };
    matches!(shape_read(shape, key), ShapeRead::Present(Some(f)) if f.is_null().is_no())
}

/// Emit for the final `??` arm, consuming S5's premise ladder as its discharge.
fn judge_coalesce_final(
    cx: &Cx,
    var: &str,
    canon: &VKey,
    env: &HashMap<String, Known>,
    premises: &[(String, VKey)],
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let Some(known) = env.get(var) else { return };
    let Some(Fact::Shape { shape, nullable: false }) = &known.fact else { return };
    let rendered = format!("${var}");
    match shape_read(shape, canon) {
        ShapeRead::Present(_) | ShapeRead::Tail(_) => {}
        ShapeRead::DeclaredAbsent => emit_offset(
            cx,
            span,
            OFFSET_UNDECLARED_ID,
            OffsetGrade::Warning,
            strict_leg_message("undeclared", &rendered, shape, canon),
            out,
        ),
        ShapeRead::MaybeMissing(_) => {
            // The `¬isset` ladder over THIS base, exactly as `coalesce_arm_fact`
            // builds it — the S5 discharge, asked as a presence question.
            let absent: Vec<VKey> =
                premises.iter().filter(|(v, _)| v == var).map(|(_, k)| k.clone()).collect();
            if cover_discharges(shape, canon, &absent).is_none() {
                emit_offset(
                    cx,
                    span,
                    OFFSET_MAYBE_MISSING_ID,
                    OffsetGrade::Warning,
                    strict_leg_message("coalesce", &rendered, shape, canon),
                    out,
                );
            }
        }
    }
}
