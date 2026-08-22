//! The dump surface (ADR-0053): `debug.type` / `debug.phpdoc-type` / `debug.var-dump`
//! / `debug.trace` — requested introspection rendered from the env at the site, the
//! honest-incompleteness vocabulary, and the assertType emission the harness reads.

use std::collections::{HashMap, HashSet};

use steins_contract::ContractTy;
use steins_domain::{Base, Certainty, Fact, IntRange, Refinement, ShapeFact, Key as VKey, Val};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_syntax::{
    ArgValue, CallExpr, Callee, Comment, NameRef, RefKind, SourceTree, Span, Stmt, StmtKind,
};

use crate::fold::Folder;
use crate::{
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, DUMP_PHPDOC_TYPE_FQN,
    DUMP_TYPE_FQN,
};
use crate::assert_harness::{ASSERT_SINK, AssertObservation};
use crate::assign::eval_coalesce_fact;
use crate::builtin_returns::{
    builtin_call_return_fact, builtin_return_floor, shape_builtin_return_fact,
};
use crate::cond::eval_binary_fact;
use crate::cx::Cx;
use crate::descent::{project_call_summary, project_method_summary, summary_binds};
use crate::env::{
    ContractArm, Known, ReturnSummary, Store, Stratum, array_literal_fact, class_const_class_fact,
    singleton_fact,
};
use crate::offsets::shape_read_at;
use crate::project::{Diagnostic, Fix, FixEdit, Res};
use crate::return_arms::{call_return_arms_by_name, method_return_arms_by_callee};
use crate::walk::WalkCx;

// ---------------------------------------------------------------------------
// The dump surface (ADR-0053): requested introspection — an "answered question".
// Emitted mid-walk at the call position, reading (never binding) the walk's facts
// (§7 / §10); the plain per-scope pass only (`descent.is_none()`), so a site is
// dumped once. The explicit pair (D3) is recognized by resolved FQN; `var_dump`
// (D4) by the PHP fallback rule. Rendering shares the ONE speller (`spell_arms`).
// ---------------------------------------------------------------------------

/// The honest-incompleteness rendering (ADR-0053 §7): the dump knows nothing faithful
/// to spell about the expression. Never a guess, never a `mixed` pretense.
const DUMP_UNKNOWN: &str = "unknown";

/// The rendering of a domain guards have subtracted to **nothing** (issue #429),
/// spelled as PHPStan's own dump spells the empty type. The opposite of
/// [`DUMP_UNKNOWN`] in every way: that one says the analysis has no faithful
/// answer, this one says the answer is that no value reaches here.
const DUMP_NEVER: &str = "*NEVER*";

/// The `debug.phpdoc-type` rendering when the contract carrier is empty (ADR-0053
/// §2): no declared `@param`/native envelope narrows the expression — never a
/// synthesized type.
const DUMP_NO_CONTRACT: &str = "no declared contract";

/// Which explicit dump the reserved FQN names (ADR-0053 §2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DumpFamily {
    /// `PHPStan\dumpType($e)` → `debug.type`: the trust-ordered best value fact.
    Type,
    /// `PHPStan\dumpPhpDocType($e)` → `debug.phpdoc-type`: the declared arm list.
    PhpDocType,
}

/// A rendered dump fact plus whether it rode an `Asserted`-stratum premise (ADR-0053
/// §2 / ADR-0052 §5): an asserted fact carries an explicit `(asserted)` marker so the
/// introspection surface never launders a docblock claim into a proven value.
struct DumpRendering {
    text: String,
    asserted: bool,
}

/// The resolved function FQN a call names (ADR-0001), lowercase-normalized —
/// definition-insensitive (no index lookup, ADR-0053 §5: the reserved dump pair
/// is recognized regardless of whether a userland definition exists). Mirrors
/// [`Cx::resolve_function`]'s name computation but yields the FQN string.
pub(crate) fn resolved_fn_fqn(cx: &Cx, r: &NameRef) -> String {
    match r.kind {
        RefKind::FullyQualified => r.raw.to_ascii_lowercase(),
        RefKind::Qualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            fqn.to_ascii_lowercase()
        }
        RefKind::Unqualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            // A `use function` import resolves the name outright.
            if let Some(t) = ctx.fn_imports.get(&name) {
                return t.to_ascii_lowercase();
            }
            // Otherwise PHP tries the current-namespace candidate first; the global
            // fallback (bare `name`, no separator) never matches a reserved `PHPStan\`
            // FQN, so the namespace candidate is the only one recognition needs.
            if ctx.namespace.is_empty() {
                name
            } else {
                format!("{}\\{}", ctx.namespace.to_ascii_lowercase(), name)
            }
        }
        // ADR-0049 A8: `namespace\name` resolves against the enclosing namespace only
        // (no imports, no global fallback). In the global namespace it is `name` itself.
        RefKind::Relative => {
            let ctx = cx.tree().ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            if ctx.namespace.is_empty() {
                name
            } else {
                format!("{}\\{}", ctx.namespace.to_ascii_lowercase(), name)
            }
        }
    }
}

/// Which explicit dump a `Callee::Function` call is, recognized by resolved FQN
/// (ADR-0053 §5): the reserved `PHPStan\` pair, definition-insensitive and
/// case-insensitive. `None` for every other call.
fn dump_family(cx: &Cx, call: &CallExpr) -> Option<DumpFamily> {
    let Callee::Function(_) = &call.receiver else { return None };
    let r = call.callee_ref.as_ref()?;
    match resolved_fn_fqn(cx, r).as_str() {
        DUMP_TYPE_FQN => Some(DumpFamily::Type),
        DUMP_PHPDOC_TYPE_FQN => Some(DumpFamily::PhpDocType),
        _ => None,
    }
}

/// Whether a call resolves to the **global** `var_dump()` under PHP's own name
/// resolution and fallback rule (ADR-0053 §5 / D4) — the `debug.var-dump` trigger.
/// The six enumerated legs:
///
/// - (a) `\var_dump($e)` — always;
/// - (b) unqualified in the root namespace — always;
/// - (c) unqualified in `namespace Foo;` — only if `Foo\var_dump` is provably
///   undefined (ambiguous or Unknown ⇒ no dump — silence is the safe side);
/// - (d) `Foo\var_dump($e)` qualified, or `use function Foo\var_dump;` — never;
///   `use function var_dump;` importing the global still dumps;
/// - (e) a method `$o->var_dump()` — never (different symbol space);
/// - (f) first-class/string callables — never (no argument expression to dump).
fn recognizes_var_dump(cx: &Cx, call: &CallExpr) -> bool {
    let Callee::Function(_) = &call.receiver else { return false };
    let Some(r) = call.callee_ref.as_ref() else { return false };
    name_reaches_global_var_dump(cx, r)
}

/// The name-resolution core of [`recognizes_var_dump`] (legs a–d; e/f are call-
/// shape questions its caller answers). Split out so the ADR-0070 survival
/// gate's dump-read recognition ([`is_dump_read_site`]) shares it verbatim.
///
/// [`is_dump_read_site`]: crate::walk::is_dump_read_site
pub(crate) fn name_reaches_global_var_dump(cx: &Cx, r: &NameRef) -> bool {
    match r.kind {
        // (a) `\var_dump` — the global builtin (a single segment, no namespace).
        RefKind::FullyQualified => r.raw.eq_ignore_ascii_case("var_dump"),
        // (d) `Foo\var_dump` — a qualified name resolves elsewhere.
        // A8: `namespace\var_dump` is relative to the current namespace with NO global
        // fallback, so it never denotes the global builtin — never a dump.
        RefKind::Qualified | RefKind::Relative => false,
        RefKind::Unqualified => {
            if !r.raw.eq_ignore_ascii_case("var_dump") {
                return false;
            }
            let ctx = cx.tree().ctx_at(r.offset);
            // (d) `use function ...\var_dump;` — resolves to the import target; only a
            // `use function var_dump;` naming the global is still the trigger.
            if let Some(t) = ctx.fn_imports.get("var_dump") {
                return t.eq_ignore_ascii_case("var_dump");
            }
            // (b) the root namespace: always the global.
            if ctx.namespace.is_empty() {
                return true;
            }
            // (c) in a namespace: only if `Ns\var_dump` is provably undefined (index
            // Absent) AND the dam is clear (dynamic code could otherwise mint it,
            // leaving existence Unknown — silence, the free safe side).
            let ns_fqn = format!("{}\\var_dump", ctx.namespace).to_ascii_lowercase();
            matches!(cx.index.resolve_function(&ns_fqn), Res::Absent) && cx.dam.is_clear()
        }
    }
}

/// A first-class callable `f(...)` (ADR-0049 §6 shape): a non-positional call with
/// all of `args`/`named_args` empty and no spread. It creates a `Closure`, not a
/// call — there is no argument expression at the site to dump (ADR-0053 §5 leg f),
/// and a reserved-name first-class callable is not a dumping call either.
pub(crate) fn is_first_class_callable(call: &CallExpr) -> bool {
    !call.positional_only && call.args.is_empty() && call.named_args.is_empty() && !call.has_spread
}

/// Render a value-domain [`Fact`] for the dump surface (ADR-0053 §7). Finite layers
/// (`Singleton`/`OneOf`) render value-precisely — the literal itself, not its base
/// type (what PHPStan's constant types render too). This reverses ADR-0053 §9's
/// collapse-to-base pin for the dump path only (the annotate/docblock renderer
/// keeps base-collapsed spelling — see [`render_finite_precise`]). Abstract layers
/// (`Refined`/`General`) render as the phpdoc keyword ladder, reusing the speller's
/// `preds_keyword` for refined strings. No faithful spelling renders [`DUMP_UNKNOWN`].
pub(crate) fn render_dump_fact(fact: &Fact) -> String {
    if let Some(members) = fact.finite_members() {
        return render_finite_precise(members).unwrap_or_else(|| DUMP_UNKNOWN.to_owned());
    }
    match fact {
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            with_null(int_range_keyword(*r), *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            with_null(steins_contract::spell::preds_keyword(*p), *nullable)
        }
        Fact::Refined { base, nullable, .. } => with_null(base_keyword(*base).to_owned(), *nullable),
        Fact::General { base, nullable } => with_null(base_keyword(*base).to_owned(), *nullable),
        // A union spells arm by arm through this same speller, joined by `|`
        // (issue #339) — `int|non-decimal-int-string` is the abstract form of
        // what the finite layers have always printed as `1|'x'`. The arms carry
        // no `null`; the union's own flag adds it once, at the end.
        Fact::Union { arms, nullable } => {
            let spelled: Vec<String> = arms
                .iter()
                .map(|(base, refinement)| {
                    let arm = match refinement {
                        Some(r) => Fact::refined(*base, *r, false),
                        None => Fact::General { base: *base, nullable: false },
                    };
                    render_dump_fact(&arm)
                })
                .collect();
            with_null(spelled.join("|"), *nullable)
        }
        // The abstract array stratum (ADR-0062 `Fact::Shape`, S2): routed through
        // the same ONE speller as the contract-arm and concrete-value paths.
        Fact::Shape { shape, nullable } => render_shape_fact(shape, *nullable),
        // Finite layers are handled above.
        Fact::Singleton(_) | Fact::OneOf(_) => unreachable!("finite_members handled above"),
    }
}

/// Spell an abstract [`ShapeFact`] for the dump surface (ADR-0062 §6, D4) — the
/// same array-vocabulary decision [`render_contract_arms`]'s `Shape` arm and
/// [`render_finite_precise`]'s concrete-array path make, run over the flow-side
/// fact form. Every field/tail slot recurses through [`render_dump_fact`] (the
/// domain's `Fact` is recursive, ADR-0062 §3). [`describe_fact`] inherits it for
/// `phpdoc.*-mismatch` messages, so the diagnostic and dump surfaces agree.
///
/// No declared-arm fallback for a `None` field slot ([`render_shape_fact_flow`]
/// is the variant that has one) — every caller here has no arm list in hand
/// (a nested field, or a caller with none to offer).
///
/// [`describe_fact`]: crate::describe_fact
pub(crate) fn render_shape_fact(shape: &ShapeFact, nullable: bool) -> String {
    render_shape_fact_flow(shape, nullable, &[])
}

/// [`render_shape_fact`] with a **declared-arm fallback** for a field whose
/// value-lane slot is `None` (issue #424): `steins-contract::to_fact`'s
/// float/int floor deliberately never lowers a `float`-typed field, so that
/// field's slot is `None` from the seed — sound while the shape is freshly
/// seeded (the caller spells straight from the arm lane then, S3), but once
/// S4 flow-refines ANY key of the same array the render switches to this
/// fact-lane speller, and without a fallback the float sibling would degrade
/// to `mixed` for no reason of its own. `fallback_arms` is searched
/// (`steins_contract::spell::spell_shape_field`) only for fields whose slot
/// is empty — a populated slot (the narrowing's own finding) always wins.
fn render_shape_fact_flow(
    shape: &ShapeFact,
    nullable: bool,
    fallback_arms: &[ContractArm],
) -> String {
    use steins_domain::{KeyClass, Presence, Tail};

    let is_list = shape.is_list == Certainty::Yes;
    // The degenerate forms (A-G1) spell as the generic vocabulary, not a brace
    // shape with only a tail: `array`, `array<K, V>`, `list<T>` — both what they
    // lowered from and what they round-trip back to. `covers` does not gate this:
    // a fieldless covers-bearing fact still spells the generic keyword (PHPStan's
    // own `non-empty-array` for an `array_key_exists` join).
    if shape.fields.is_empty()
        && let Tail::Unsealed { key, value } = &shape.tail
    {
        let val = value.as_ref().map(|f| render_dump_fact(f));
        // Printed only where narrower than `array-key`; dropped for a list.
        let key_text = match key {
            KeyClass::ArrayKey => None,
            KeyClass::Int => Some("int"),
            KeyClass::Str => Some("string"),
        };
        // A key-agnostic, value-agnostic tail IS plain `array`. A denotational
        // `No` (never `Yes`, handled above) is Phan's `associative-array`.
        let not_list = shape.is_list == Certainty::No;
        let body = steins_contract::spell::spell_generic_array(
            is_list,
            not_list,
            shape.non_empty,
            key_text,
            val.as_deref(),
        );
        return with_null(body, nullable);
    }
    // Field order follows PROVENANCE (issue #327): a shape that witnessed its
    // construction prints the order it saw (`['b'=>1,'a'=>$x]` is
    // `array{b: 1, a: int}`, matching the reference implementation). A shape with
    // a merely *declared* order keeps canonical key order — Steins saying
    // `array{b: int, a: string}` is an order-agnostic key set (ADR-0062 §2, RFC
    // #14939), the registered divergence between the two provenances.
    let ordered: Vec<&(VKey, Presence, Option<Box<Fact>>)> = match &shape.order {
        Some(order) => order
            .iter()
            .filter_map(|k| shape.fields.iter().find(|(fk, _, _)| fk == k))
            .collect(),
        None => shape.fields.iter().collect(),
    };
    let fields: Vec<(VKey, bool, String)> = ordered
        .into_iter()
        .filter(|(_, p, _)| !matches!(p, Presence::Absent))
        .map(|(k, p, slot)| {
            let value = slot.as_ref().map_or_else(
                || shape_field_fallback(fallback_arms, k).unwrap_or_else(|| "mixed".to_owned()),
                |f| render_dump_fact(f),
            );
            (k.clone(), p.is_required(), value)
        })
        .collect();
    let tail = match &shape.tail {
        Tail::Sealed => steins_contract::spell::ShapeTail::Sealed,
        Tail::Unsealed { key: KeyClass::ArrayKey, value: None } => {
            steins_contract::spell::ShapeTail::Untyped
        }
        Tail::Unsealed { key, value } => {
            let val_spelling = value.as_ref().map_or_else(|| "mixed".to_owned(), |f| render_dump_fact(f));
            let key_spelling = match key {
                KeyClass::ArrayKey => None,
                KeyClass::Int => Some("int".to_owned()),
                KeyClass::Str => Some("string".to_owned()),
            };
            steins_contract::spell::ShapeTail::Typed { key: key_spelling, value: val_spelling }
        }
    };
    let body = steins_contract::spell::spell_shape(is_list, shape.non_empty, &fields, &tail);
    with_null(body, nullable)
}

/// The declared spelling of field `k`, read off `arms` — the first arm whose
/// `Shape` declares `k` wins ([`steins_contract::spell::spell_shape_field`]).
/// `None` when no arm declares it (an unsealed-tail addition the guard
/// admitted, or `arms` itself carries nothing shaped). Issue #424's fallback:
/// [`render_shape_fact_flow`]'s only caller of this.
fn shape_field_fallback(arms: &[ContractArm], k: &VKey) -> Option<String> {
    arms.iter().find_map(|a| steins_contract::spell::spell_shape_field(&a.ty, k))
}

/// Value-precise spelling of a finite value set (`Singleton`/`OneOf` members) for
/// the dump surface: int/float/bool literals verbatim, string literals through the
/// shared speller's escaping ([`steins_contract::spell::string_literal`]), `null`
/// as `null`, and (ADR-0062 §6) an array member through the shared D4 spelling
/// ([`steins_contract::spell::spell_val`]: `list{…}` or `array{…}`, recursing).
/// Members sorted+deduped, joined with `|`. `None` only for an empty slice
/// (unreachable from `Fact::finite_members`).
fn render_finite_precise(members: &[Val]) -> Option<String> {
    let mut vals = members.to_vec();
    vals.sort();
    vals.dedup();
    // Emit in the canonical spelling order `summarize_vals` fixes (int, float,
    // string, bool, null) — kept stable and PHPStan-shaped for readable output.
    let mut parts: Vec<String> = Vec::with_capacity(vals.len());
    parts.extend(vals.iter().filter_map(|v| match v {
        Val::Int(n) => Some(n.to_string()),
        _ => None,
    }));
    parts.extend(vals.iter().filter_map(|v| match v {
        Val::Float(f) => Some(render_dump_float(*f)),
        _ => None,
    }));
    parts.extend(vals.iter().filter_map(|v| match v {
        // A byte string has no phpdoc literal spelling, so the dump surface shows
        // PHP's own escape form instead of a lossy character.
        Val::Str(s) => Some(
            s.as_str().map_or_else(|| s.to_php_literal(), steins_contract::spell::string_literal),
        ),
        _ => None,
    }));
    // Both bool literals present == the whole `bool` type — PHPStan renders `bool`,
    // not `false|true`. A single bool literal stays precise.
    match (vals.contains(&Val::Bool(true)), vals.contains(&Val::Bool(false))) {
        (true, true) => parts.push("bool".to_owned()),
        (true, false) => parts.push("true".to_owned()),
        (false, true) => parts.push("false".to_owned()),
        (false, false) => {}
    }
    if vals.contains(&Val::Null) {
        parts.push("null".to_owned());
    }
    // Array members: D4 spelling, appended last (scalars first, arrays after).
    parts.extend(vals.iter().filter_map(|v| match v {
        Val::Array(_) => Some(steins_contract::spell::spell_val(v)),
        _ => None,
    }));
    (!parts.is_empty()).then(|| parts.join("|"))
}

/// Render a float constant as PHPStan does: an integral value keeps a visible
/// fractional part (`123.0`, not `123`); every other value uses its shortest
/// round-tripping decimal (`3.14`, `1.5`).
fn render_dump_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 { format!("{f:.1}") } else { f.to_string() }
}

/// Append `|null` when the fact admits null (the honest nullable spelling).
fn with_null(s: String, nullable: bool) -> String {
    if nullable { format!("{s}|null") } else { s }
}

/// The bare phpdoc keyword for a scalar base.
fn base_keyword(b: Base) -> &'static str {
    match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    }
}

/// Fold each enum whose **whole** declared case set is present back into one
/// [`ContractTy::Class`] arm (issue #429) — the inverse of
/// [`expand_enum_case_arms`], applied at the rendering boundary only.
///
/// Purely a spelling rule, and it belongs on this side of the ADR-0052 §4 cut for
/// the reason the ADR states: the expansion is what makes the domain finite and
/// subtractable, while `Suit` and `Suit::Hearts|Suit::Spades|Suit::Clubs` denote
/// one set, so which of the two a reader sees is rendering policy. The collapsed
/// arm keeps the first case arm's position, so declaration order survives.
///
/// [`expand_enum_case_arms`]: crate::refine::expand_enum_case_arms
fn collapse_whole_enums(cx: &Cx, tys: impl Iterator<Item = ContractTy>) -> Vec<ContractTy> {
    let tys: Vec<ContractTy> = tys.collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for ty in &tys {
        if let ContractTy::EnumCase { enum_fqn, .. } = ty {
            *counts.entry(enum_fqn.as_str()).or_default() += 1;
        }
    }
    let whole: HashSet<&str> = counts
        .into_iter()
        .filter(|(fqn, n)| cx.enum_case_names(fqn).is_some_and(|all| all.len() == *n))
        .map(|(fqn, _)| fqn)
        .collect();
    if whole.is_empty() {
        return tys;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<ContractTy> = Vec::with_capacity(tys.len());
    for ty in &tys {
        match ty {
            ContractTy::EnumCase { enum_fqn, .. } if whole.contains(enum_fqn.as_str()) => {
                if seen.insert(enum_fqn.as_str()) {
                    out.push(ContractTy::Class(enum_fqn.clone()));
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// How an int interval spells on the dump surface — shares
/// [`steins_contract::spell::int_range_keyword`] with the contract-arm path so
/// the two can't disagree about a range (issue #90). [`describe_fact`] keeps its
/// own keyword prose since a finding message reads better with phpdoc sugar.
///
/// [`describe_fact`]: crate::describe_fact
fn int_range_keyword(r: IntRange) -> String {
    steins_contract::spell::int_range_keyword(r)
}

/// Render a narrowed contract-fact arm list (ADR-0052 §1 carrier) for the dump
/// surface. Scalar arms spell through [`steins_contract::spell::spell_arms`]; a
/// pure class/`null` arm list renders each class's source-cased FQN (via
/// [`Cx::class_display_fqn`], matching PHPStan); anything else has no faithful
/// spelling (`None` — caller falls to honest unknown).
///
/// An enum whose cases are ALL still present collapses back to the enum's own
/// name first (issue #429): the expanded case set and the declaration denote the
/// same thing, and a reader who narrowed nothing must be shown what they wrote.
pub(crate) fn render_contract_arms(cx: &Cx, arms: &[ContractArm]) -> Option<String> {
    let tys: Vec<ContractTy> = collapse_whole_enums(cx, arms.iter().map(|a| a.ty.clone()));
    if let Some(scalar) = steins_contract::spell::spell_arms(&tys) {
        return Some(scalar);
    }
    let mut parts = Vec::new();
    for ty in &tys {
        match ty {
            ContractTy::Class(n) => parts.push(cx.class_display_fqn(n)),
            // `Suit::Hearts`, PHPStan's own spelling, with the enum's declared
            // casing recovered the way a class arm's is (issue #429).
            ContractTy::EnumCase { enum_fqn, case } => {
                parts.push(format!("{}::{case}", cx.class_display_fqn(enum_fqn)));
            }
            ContractTy::Null => parts.push("null".to_owned()),
            // An array/generic/shape/callable/intersection arm has no faithful plain
            // spelling here — honest unknown rather than a guess (§7).
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("|"))
}

/// The dump spelling of a declared lane the branch's guards NARROWED (issue #429),
/// or `None` when the lane says no more than the declaration and the rungs below
/// should answer.
///
/// Two things qualify, and only two:
///
/// * a lane **subtracted to nothing** ([`Store::contract_emptied`]) —
///   [`DUMP_NEVER`]. Every arm was `Verified` and every guard that deleted one was
///   a native runtime test, so this is the statement that no value reaches the
///   position, not a guess;
/// * a lane that is entirely enum cases of ONE enum and holds **fewer than all**
///   of them. The unnarrowed case set denotes exactly what `Suit` denotes, and
///   spelling out every case there would make an untouched parameter dump as a
///   union nobody wrote.
///
/// Mixed lanes, class lanes, scalar lanes and the array vocabulary all decline:
/// their narrowing has other carriers with their own dump rungs, and this one
/// exists for the domain that has no other home.
fn narrowed_lane_dump(cx: &Cx, store: &Store, var: &str) -> Option<String> {
    if store.contract_emptied(var) {
        return Some(DUMP_NEVER.to_owned());
    }
    let arms = store.contract_arms(var)?;
    let mut of: Option<&str> = None;
    for a in arms {
        let ContractTy::EnumCase { enum_fqn, .. } = &a.ty else { return None };
        if *of.get_or_insert(enum_fqn.as_str()) != enum_fqn.as_str() {
            return None;
        }
    }
    if cx.enum_case_names(of?).is_some_and(|all| all.len() == arms.len()) {
        return None;
    }
    render_contract_arms(cx, arms)
}

/// The `Known::bound` provenance every S4 flow refinement stamps on a shape
/// fact. Two things read it: `annotate` (as prose) and [`shape_is_flow_refined`]
/// (as the dump-preference signal), which is why it is a constant rather than a
/// literal at each site.
pub(crate) const SHAPE_REFINED: &str = "narrowed array shape";

/// Has this shape fact been refined by the flow, rather than merely seeded from a
/// declaration? Two independent signals, since S4 operators leave different
/// traces: a witnessed field is presence promotion's own record (ADR-0062 §3),
/// and [`SHAPE_REFINED`] covers operators with no structural mark
/// (`set_non_empty`, `set_is_list`, `mark_absent`, the collapse mint).
fn shape_is_flow_refined(fact: &Fact, known: &Known) -> bool {
    if known.bound.as_deref() == Some(SHAPE_REFINED) {
        return true;
    }
    let Fact::Shape { shape, .. } = fact else { return false };
    shape
        .fields
        .iter()
        .any(|(_, p, _)| matches!(p, steins_domain::Presence::Required { witnessed: true }))
}

/// The best value fact of a dump argument, in the trust order (ADR-0052 §1 /
/// ADR-0037): a proven value fact, else the object holder's exact class / membership,
/// else the narrowed declared-arm list, else honest unknown. Drives `debug.type` and
/// `debug.var-dump` (identical rendering, identical fact source, ADR-0053 §2).
fn best_dump_type(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    // The dump argument's span start — the provenance anchor for the issue-#60
    // value-lane descent below (its findings are suppressed, so this surfaces only
    // in the bound params' internal provenance strings).
    span_start: u32,
) -> DumpRendering {
    let cx = w.cx;
    let poisoned = w.scope.poisoned;
    if let ArgValue::Var(name) = value {
        // 1. A proven value fact (the four-layer value domain), carrying its stratum.
        if let Some(known) = env.get(name)
            && let Some(fact) = &known.fact
        {
            // A-G1a, applied to spelling: declared fidelity the fact domain cannot
            // express (class-typed slots, exotic key contracts) lives in the ALIGNED
            // arm, and an UNREFINED shape fact is by construction a lossy lowering
            // of that one arm — so a freshly seeded binding spells from the arm
            // lane while both describe the same thing.
            //
            // **S4 flips it once flow refinement exists** (the S3 note at this site
            // said it would have to): a fact carrying a witnessed field, or one
            // minted by arm subtraction, states something the declared arm does
            // not, and spelling the arm would report the declaration back at a
            // caller who just narrowed it. [`shape_is_flow_refined`] is the test.
            if matches!(fact, Fact::Shape { .. })
                && !shape_is_flow_refined(fact, known)
                && let Some(arms) = store.contract_arms(name)
                && let Some(text) = render_contract_arms(cx, arms)
            {
                return DumpRendering {
                    text,
                    asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
                };
            }
            // Flow-refined (or arm-less): the fact-lane spelling, same as the
            // fallthrough below — except a `Shape` still keeps a fallback to
            // the declared arms for any field the S4 narrowing didn't touch
            // (issue #424). `to_fact`'s float/int floor leaves such a field's
            // value-lane slot `None` from the seed; without this, the ONE
            // switch above (full arm text vs. full fact text) would make that
            // field read `mixed` the moment a sibling key got narrowed.
            if let Fact::Shape { shape, nullable } = fact {
                let fallback = store.contract_arms(name).unwrap_or(&[]);
                return DumpRendering {
                    text: render_shape_fact_flow(shape, *nullable, fallback),
                    asserted: known.stratum == Stratum::Asserted,
                };
            }
            return DumpRendering {
                text: render_dump_fact(fact),
                asserted: known.stratum == Stratum::Asserted,
            };
        }
        // 1b. A declared lane the guards on this path have NARROWED (issue #429),
        //     above every object rung for rung 2b's reason: a guard the walk just
        //     executed is strictly stronger than the declaration it narrowed, and
        //     printing `Suit` inside `if ($s === Suit::Hearts)` would report the
        //     declaration back at a reader who had already refined it. An
        //     un-narrowed lane declines here and the declaration wins, exactly as
        //     before.
        if let Some(text) = narrowed_lane_dump(cx, store, name) {
            return DumpRendering { text, asserted: false };
        }
        // 2. An object holder whose class the heap proved EXACT — the allocation's
        //    own class, rendered source-cased and namespace-qualified (matching
        //    PHPStan).
        if let Some(obj) = store.obj_of(name)
            && obj.class_exact
        {
            return DumpRendering { text: cx.class_display_fqn(&obj.class), asserted: false };
        }
        // 2b. The N4 `Member{yes:[…]}` carrier (ADR-0052 §1): a var an `instanceof`
        //     guard bound to a class. A single-yes-member set renders that class; a
        //     multi-member set falls through. Bound at `Verified` (a live-branch
        //     `instanceof`) => never `(asserted)`.
        //
        //     Above the lower-bound heap class, not below it: since a declared
        //     parameter is a heap object (issue #388) the two co-occur, and a guard
        //     the walk just executed is strictly stronger than the declaration it
        //     narrowed — rendering `Box` inside `if ($b instanceof Sub)` would report
        //     the declaration back at a reader who had already refuted it.
        if let Some(m) = store.member_of(name)
            && let [only] = m.yes.as_slice()
        {
            return DumpRendering { text: cx.class_display_fqn(only), asserted: false };
        }
        // 2c. An object holder whose class is only a lower bound — a `$this` seed, a
        //     declared parameter, a returned non-exact object. Still the object's
        //     own fact and still above the declared arms, which for such a variable
        //     say the same thing one rung less directly.
        if let Some(obj) = store.obj_of(name) {
            return DumpRendering { text: cx.class_display_fqn(&obj.class), asserted: false };
        }
        // 3. The narrowed declared-arm list (contract carrier).
        if let Some(arms) = store.contract_arms(name)
            && let Some(text) = render_contract_arms(cx, arms)
        {
            return DumpRendering {
                text,
                asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
            };
        }
        // 4. Honest unknown.
        return DumpRendering { text: DUMP_UNKNOWN.to_owned(), asserted: false };
    }
    // A constant-key read against an abstract shape (ADR-0062 §4, S3): the declared
    // field's value slot. Every no-fact outcome (optional field, unknown slot,
    // declared absence) falls through to honest unknown.
    if let ArgValue::OffsetRead { base, key } = value
        && let Some((read, stratum)) = shape_read_at(base, key, env, poisoned, cx.php_minor)
        && let Some(fact) = read.into_fact()
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }

    // A `??` chain (ADR-0052 §6 + ADR-0062 A-G11, S5): the spine's join under the
    // left-to-right `¬isset` premise ladder, where a KeyCover discharges. Placed
    // above the fold since a `??` is never a literal the folder can reach.
    if let ArgValue::Coalesce(a, b, _) = value
        && let Some((fact, stratum)) = eval_coalesce_fact(w, folder, a, b, env, Some(store))
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }

    // A value-position binary operator (issue #260): the operator's own fact, same
    // placement reasoning as `??`. Total for a comparison, so lower rungs never see
    // one — `true`/`false` when `eval_cmp` decides, `bool` when it doesn't.
    if let ArgValue::Binary { op, lhs, rhs } = value {
        let (fact, stratum) =
            eval_binary_fact(cx, folder, *op, lhs, rhs, env, Some(store), poisoned);
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }

    // A depth-1 property fetch `$var->prop` (ADR-0052 §7, Gap B): the allocation-keyed
    // heap property fact (alias-correct, ADR-0036). Escaped-then-swept props carry no
    // fact and fall through to unknown; a readonly prop survives. Deeper chains
    // (`$a->b->c`) lower to `Other`, never here.
    if let ArgValue::PropFetch { var, prop } = value
        && !poisoned
        && let Some(fact) = store.prop_fact(var, prop)
    {
        return DumpRendering {
            text: render_dump_fact(fact),
            asserted: store.prop_stratum(var, prop) == Stratum::Asserted,
        };
    }
    // A non-variable argument: a resolved literal/foldable value fact wins first (a
    // fully-literal call folds to a Singleton, ADR-0056 §4). Stratum comes from the
    // resolution itself so a fold over an Asserted project-call summary stays
    // Asserted (issue #127).
    if let Some((lit, strat)) = cx.resolve_literal_strat(value, env, poisoned, folder)
        && let Some(fact) = singleton_fact(&lit, cx.php_minor)
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: strat == Stratum::Asserted,
        };
    }
    // An array literal the rung above could not prove whole (issue #327): the
    // shape its observed keys denote, with an unknown slot per unresolved element.
    if let ArgValue::Array(items) = value
        && let Some((fact, stratum)) =
            array_literal_fact(cx, folder, items, env, poisoned, Some(store))
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }
    // The `::class` magic constant (issue #236): FQN literal when written, the
    // `class-string` refinement when relative. Verified — PHP's own claim.
    if let ArgValue::ClassConst(sc, name) = value
        && let Some(fact) = class_const_class_fact(cx, w.scope, sc, name)
    {
        return DumpRendering { text: render_dump_fact(&fact), asserted: false };
    }
    // The member-wise union fold (issue #74): a bounded union-of-constants argument
    // is enumerated, each combination folded through the same seam a literal call
    // takes, and the answers composed. Sits above every type rung below — this is a
    // value the real engine answered, member by member.
    if let ArgValue::Call(name, cargs) = value
        && let Some((fact, stratum, _prov)) = cx.try_union_fold(name, cargs, env, poisoned, folder)
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }
    // A project-function call in argument position (issue #60): the T0 return-fact
    // summary, where the assignment ladder puts it (fold > summary > builtin
    // envelope > arms). `summary_binds` keeps the two forms identical. Findings go
    // to a scratch since the dump surface never emits for the callee, and
    // `descent: None` is sound because `emit_dumps` runs only in the plain
    // per-scope pass.
    if let ArgValue::Call(name, cargs) = value
        && !cargs.is_empty()
    {
        let mut scratch: Vec<Diagnostic> = Vec::new();
        if let Some(ReturnSummary { value: Some(sv), .. }) = project_call_summary(
            cx, folder, name, cargs, env, store, poisoned, span_start, None, &mut scratch,
        ) && summary_binds(&sv.fact)
        {
            return DumpRendering {
                text: render_dump_fact(&sv.fact),
                asserted: sv.stratum == Stratum::Asserted,
            };
        }
    }
    // A method / static call in argument position (issue #386): the same rung, one
    // resolver over, and the same `summary_binds` gate — so `dumpType($b->get())`
    // and `$v = $b->get(); dumpType($v)` cannot disagree. `w` carries the frame, so
    // `$this->m()` and `self::m()` resolve here where the frame-less seams decline.
    // The **value** component only: an object result has no rendering in value
    // position (ADR-0057 B5), which is why `dumpType($b->makeFoo())` stays unknown.
    if let ArgValue::MethodCall { callee, args, named } = value {
        let mut scratch: Vec<Diagnostic> = Vec::new();
        if let Some(ReturnSummary { value: Some(sv), .. }) = project_method_summary(
            cx,
            folder,
            callee,
            args,
            named,
            env,
            store,
            w.this_exact,
            w.enclosing_class,
            poisoned,
            span_start,
            None,
            &mut scratch,
        ) && summary_binds(&sv.fact)
        {
            return DumpRendering {
                text: render_dump_fact(&sv.fact),
                asserted: sv.stratum == Stratum::Asserted,
            };
        }
    }
    // Argument-dependent type rung (ADR-0061 §1) — `count`/`array_is_list` over an
    // abstract shape (ADR-0062 §4) — sits above the envelope, as at the assignment
    // seam, carrying the argument's stratum.
    if let ArgValue::Call(name, args) = value
        && let Some((fact, stratum)) =
            shape_builtin_return_fact(cx, folder, name, args, env, Some(store), poisoned)
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: stratum == Stratum::Asserted,
        };
    }

    // A uniquely-resolved builtin call the fold could not reach: its reflected
    // return envelope / admitted refinement (ADR-0056 R1). Always Verified — read
    // off the engine's own arginfo.
    if let ArgValue::Call(name, _) = value
        && let Some(fact) = builtin_call_return_fact(cx, folder, name)
    {
        return DumpRendering { text: render_dump_fact(&fact), asserted: false };
    }
    // The declared-return floor (ADR-0069): reached only where the engine said
    // nothing about this name. Always `(asserted)` — the row is a catalog
    // declaration, not a runtime answer. Rendered through the same arm speller the
    // project-call floor below uses.
    if let ArgValue::Call(name, _) = value
        && let Some(arms) = builtin_return_floor(cx, name)
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering { text, asserted: true };
    }
    // The declared-return floor of an unresolved project call (issue #60): the
    // callee's `: string` is a fact the caller should see even with no summary
    // crossed. Exactly the arm list the assignment form seeds into the contract
    // store; no declared return type still falls to honest unknown.
    if let ArgValue::Call(name, cargs) = value
        && let Some(arms) = call_return_arms_by_name(cx, folder, name, cargs, env, store, poisoned)
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    // The same floor for an unsummarized method/static call (issue #386): the
    // declared `: string` of the resolved target, which the assignment form seeds
    // into the contract store.
    if let ArgValue::MethodCall { callee, args, .. } = value
        && let Some(arms) = method_return_arms_by_callee(
            cx,
            folder,
            callee,
            args,
            env,
            store,
            w.this_exact,
            w.enclosing_class,
            poisoned,
        )
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    DumpRendering { text: DUMP_UNKNOWN.to_owned(), asserted: false }
}

/// The declared-side view of a dump argument (ADR-0053 §2, `debug.phpdoc-type`): the
/// contract-fact arm list (the declared envelope as narrowed by guards), or
/// `no declared contract` when the carrier is empty — never a synthesized type.
fn best_dump_phpdoc_type(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> DumpRendering {
    if let ArgValue::Var(name) = value
        && let Some(arms) = store.contract_arms(name)
        && let Some(text) = render_contract_arms(cx, arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    // A project call in argument position (issue #60): its declared envelope is the
    // same arm list the assignment form seeds into the contract store — parity
    // between `dumpPhpDocType(f(…))` and `$x = f(…); dumpPhpDocType($x)`.
    if let ArgValue::Call(name, cargs) = value
        && let Some(arms) = call_return_arms_by_name(cx, folder, name, cargs, env, store, poisoned)
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    // A method / static call in argument position (issue #386): the same parity, the
    // same speller. This surface has no `WalkCx`, so it holds no enclosing class —
    // `$this->m()`, `self::m()` and `parent::m()` decline here (through
    // `resolve_call_target`'s own arms) while `dumpType` next door resolves them.
    // Widening the signature to close that would thread a frame through the
    // declared-side dump for one receiver spelling; the declining direction is
    // silence, and the value-side dump is where a receiver is observed.
    if let ArgValue::MethodCall { callee, args, .. } = value
        && let Some(arms) =
            method_return_arms_by_callee(cx, folder, callee, args, env, store, None, None, poisoned)
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    // A builtin call, through the ADR-0069 floor (issue #79). Same parity claim, one
    // rung lower — the row IS a declared contract.
    if let ArgValue::Call(name, _) = value
        && let Some(arms) = builtin_return_floor(cx, name)
        && let Some(text) = render_contract_arms(cx, &arms)
    {
        return DumpRendering { text, asserted: true };
    }
    DumpRendering { text: DUMP_NO_CONTRACT.to_owned(), asserted: false }
}

/// The message frame around a rendered dump fact (ADR-0053 §7: wording is not a
/// contract, the rendered fact is). Carries the `(asserted)` marker when the fact
/// rode a docblock/assert premise.
fn dump_message(label: &str, r: &DumpRendering) -> String {
    let marker = if r.asserted { " (asserted)" } else { "" };
    format!("{label}: {}{marker}", r.text)
}

/// Emit the dump reports a recognized call site produces (ADR-0053 §7): the explicit
/// pair (D3) by resolved FQN, `var_dump` (D4) by the PHP fallback rule. One report
/// per positional argument; a zero-argument `dumpType()` still reports (fail-level,
/// "nothing to dump" — the committed call is a runtime fatal either way). Reads the
/// walk's facts at the call position; binds nothing (§10 §3).
///
/// `removal` is the enclosing statement's span when the dump call IS the whole
/// expression-statement, driving the fix payload (ADR-0010, issue #114): the
/// explicit pair's remedy is deleting that statement, so each finding there carries
/// the deletion as a first-class [`Fix`]. A dump embedded in a larger statement
/// (`$y = dumpType($x);`) gets `None` — deleting the statement would delete the
/// enclosing binding too. `debug.var-dump` never carries a fix: `var_dump()` is
/// legal working PHP, so deleting it is a judgment call.
pub(crate) fn emit_dumps(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    removal: Option<Span>,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if let Some(family) = dump_family(cx, call) {
        // A first-class callable `dumpType(...)` is a Closure, not a dumping call.
        if is_first_class_callable(call) {
            return;
        }
        let (id, label, call_name) = match family {
            DumpFamily::Type => (DEBUG_TYPE_ID, "dumped type", "PHPStan\\dumpType()"),
            DumpFamily::PhpDocType => {
                (DEBUG_PHPDOC_TYPE_ID, "dumped phpdoc type", "PHPStan\\dumpPhpDocType()")
            }
        };
        // The statement deletion, widened to swallow its whole line when the
        // statement stands alone (no blank gutter line left behind). A
        // multi-argument dump emits one finding per argument, each carrying this
        // same edit — the CLI's plan builder dedupes identical edits.
        let fix = removal.map(|span| {
            let span = cx.tree().whole_line_span(span);
            Fix {
                title: "remove the dump statement",
                edits: vec![FixEdit {
                    path: cx.path().to_owned(),
                    start: span.start,
                    end: span.end,
                    replacement: String::new(),
                }],
            }
        });
        if call.args.is_empty() {
            // Zero-argument explicit dump: still fail-level (§7) — the runtime fatal
            // stands regardless of what (nothing) it would dump.
            let pos = cx.tree().position(call.span.start);
            out.push(Diagnostic {
                id,
                facet: None,
                fix,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: format!("{call_name} called with no argument — nothing to dump"),
            });
            return;
        }
        for arg in &call.args {
            let rendering = match family {
                DumpFamily::Type => best_dump_type(w, folder, &arg.value, env, store, arg.span.start),
                DumpFamily::PhpDocType => {
                    best_dump_phpdoc_type(cx, folder, &arg.value, env, store, w.scope.poisoned)
                }
            };
            let pos = cx.tree().position(arg.span.start);
            out.push(Diagnostic {
                id,
                facet: None,
                fix: fix.clone(),
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: dump_message(label, &rendering),
            });
        }
        return;
    }

    // var_dump (ADR-0053 D4): default-on, one `debug.type`-shaped report per argument,
    // same rendering and fact source as the explicit `debug.type`. A first-class
    // callable and a zero-argument `var_dump()` dump nothing (§2/§5 leg f).
    if recognizes_var_dump(cx, call) {
        if is_first_class_callable(call) || call.args.is_empty() {
            return;
        }
        for arg in &call.args {
            let rendering = best_dump_type(w, folder, &arg.value, env, store, arg.span.start);
            let pos = cx.tree().position(arg.span.start);
            out.push(Diagnostic {
                id: DEBUG_VAR_DUMP_ID,
                facet: None,
                fix: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: dump_message("dumped type", &rendering),
            });
        }
    }
}

/// Whether the statement starting at `stmt_start` is itself a **declaration**
/// (function/class/interface/enum/trait). Declarations lower to
/// [`StmtKind::Barrier`], so the kind alone cannot say; the tree's declaration
/// indexes can — a named function's or class-like's name span falls inside the
/// declaration statement's own span and inside no other `Barrier`'s.
///
/// Matters because (ADR-0074 §6) a docblock a declaration owns is a contract
/// surface, never a statement trigger. The shared adoption query (`stmt_docblock`)
/// does not exclude declaration-owned docblocks, so this guard is load-bearing;
/// the trace-specific exclusion lives here, keeping the query itself shared.
fn stmt_is_declaration(tree: &SourceTree, span: Span) -> bool {
    let within = |s: Span| span.start <= s.start && s.start < span.end;
    tree.functions().iter().any(|f| within(f.span))
        || tree.classes().iter().any(|c| within(c.span))
}

/// The docblock the statement adopts as a trace-annotation trigger (ADR-0074
/// §6), or `None` for every deliberate silence: a docblock not adopted under the
/// shared statement-adoption rule (`stmt_docblock`, same query the inline-`@var`
/// cast reads), and a declaration statement's docblock (a contract surface,
/// inert at the emitter — see [`stmt_is_declaration`]). Resolved at the top of
/// the walk's per-statement step; flushed by [`emit_trace_annotations`] at exit.
pub(crate) fn adopted_trace_docblock<'a>(w: &'a WalkCx, stmt: &Stmt) -> Option<&'a Comment> {
    let tree = w.cx.tree();
    let comment = tree.stmt_docblock(stmt.span.start)?;
    // Only a `Barrier` can be a declaration statement (see
    // `stmt_is_declaration`), so every other kind skips the index scan outright.
    if matches!(stmt.kind, StmtKind::Barrier) && stmt_is_declaration(tree, stmt.span) {
        return None;
    }
    Some(comment)
}

/// Emit the trace-annotation reports a statement's adopted docblock asks for
/// (ADR-0074 §5/§6): a `/** @psalm-trace $x */` directly above the statement is
/// the docblock spelling of `PHPStan\dumpType($x)` — the same question, answered
/// through the same renderer ([`best_dump_type`]), against the statement's exit
/// facts (§5, Psalm semantics: "applied to the next statement", reporting what
/// it leaves behind), reported at the tag's own position. A comma list
/// (`@psalm-trace $a, $b`, §7) arrives as one tag per variable, emitting one
/// diagnostic each, independently rendered. Reads facts, binds nothing (§9).
///
/// `pending` is the adoption [`adopted_trace_docblock`] resolved at the step's
/// top — `None` flushes nothing. Called exactly once per statement, on
/// whichever exit it takes (divergent `return`s answer too, §5, or the common
/// bottom). A named variable with no fact renders honest `unknown`.
///
/// Plain per-scope pass only (gates on `descent.is_none()` like [`emit_dumps`]),
/// so an annotated site emits once.
pub(crate) fn emit_trace_annotations(
    w: &WalkCx,
    folder: &mut dyn Folder,
    pending: Option<&Comment>,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let Some(comment) = pending else { return };
    for tag in scan_docblock(&comment.text) {
        if tag.kind != TagKind::TraceTag {
            continue;
        }
        let Some(var) = tag.var_name.as_deref() else { continue };
        let name = var.trim_start_matches('$');
        let rendering = best_dump_type(
            w,
            folder,
            &ArgValue::Var(name.to_owned()),
            env,
            store,
            stmt.span.start,
        );
        // The diagnostic sits at the tag's own line/column: the tag span is
        // docblock-relative (`comment.text` is the exact source substring at
        // `comment.span`), so the comment's file span start maps it back.
        let pos = cx.tree().position(comment.span.start + tag.tag_span.start);
        out.push(Diagnostic {
            id: DEBUG_TRACE_ID,
            facet: None,
            fix: None,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            // The label names the variable so the list form stays unambiguous;
            // the rendered fact after the final ": " is the parity-pinned
            // part, the frame wording is not a contract (§5).
            message: dump_message(&format!("traced type of {var}"), &rendering),
        });
    }
}

/// The resolved FQN of `PHPStan\Testing\assertType` (oracle idea B), lowercase-
/// normalized — the assertType harness recognizer key, matched exactly like the
/// dump family's [`DUMP_FQNS`], by resolved FQN (definition- and case-insensitive).
///
/// [`DUMP_FQNS`]: crate::DUMP_FQNS
pub(crate) const ASSERT_TYPE_FQN: &str = "phpstan\\testing\\asserttype";

/// Harness-only (oracle idea B): when the assertType sink is installed
/// ([`collect_assert_types`]), recognize a `PHPStan\Testing\assertType('T', $e)` call
/// by resolved FQN and record (expected string, Steins rendering of `$e`) — sharing
/// the D3 dump path ([`best_dump_type`]) verbatim. A no-op when the sink is absent,
/// so the check surface is byte-identical. Plain per-scope pass only, like
/// [`emit_dumps`].
///
/// [`collect_assert_types`]: crate::assert_harness::collect_assert_types
pub(crate) fn emit_asserts(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) {
    // Fast path: no sink ⇒ not the harness ⇒ assertType is an ordinary call.
    if ASSERT_SINK.with(|s| s.borrow().is_none()) {
        return;
    }
    let cx = w.cx;
    let Callee::Function(_) = &call.receiver else { return };
    let Some(r) = call.callee_ref.as_ref() else { return };
    if resolved_fn_fqn(cx, r) != ASSERT_TYPE_FQN {
        return;
    }
    // `assertType('Expected', $expr)` needs both positional arguments; a first-class
    // callable or a mis-arity call records nothing (there is no fact pair to observe).
    if call.args.len() < 2 {
        return;
    }
    let expected = assert_expected_string(cx, &call.args[0].value, env, w.scope.poisoned, folder);
    let rendering = best_dump_type(w, folder, &call.args[1].value, env, store, call.args[1].span.start);
    let pos = cx.tree().position(call.span.start);
    let obs = AssertObservation {
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        expected,
        got: rendering.text,
        asserted: rendering.asserted,
    };
    ASSERT_SINK.with(|s| {
        if let Some(buf) = s.borrow_mut().as_mut() {
            buf.push(obs);
        }
    });
}

/// The expected-type string an `assertType` first argument names: a plain string
/// literal, or a value Steins can fold to one; `None` for a `::class`/concatenation
/// the fold cannot reduce (the harness counts those as skipped, never a false match).
fn assert_expected_string(
    cx: &Cx,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    folder: &mut dyn Folder,
) -> Option<String> {
    // A text lane (the harness compares this against a phpdoc type string), so a
    // non-UTF-8 literal is not an expected-type spelling at all.
    if let ArgValue::Str(s) = value {
        return s.as_str().map(ToOwned::to_owned);
    }
    match cx.resolve_literal(value, env, poisoned, folder) {
        Some(ArgValue::Str(s)) => s.as_str().map(ToOwned::to_owned),
        _ => None,
    }
}
