//! Undefined variables (ADR-0078 / issue #194): `variable.undefined` and
//! `variable.maybe-undefined` over the scope's binding lanes, dam-gated, plus the
//! `phpdoc.maybe-undefined` leg of the unset pseudo-type (ADR-0087) and the by-ref /
//! out-parameter spans that exempt a read.

use std::collections::{HashMap, HashSet};

use steins_contract::ContractTy;
use steins_phpdoc::{TagKind, scan_docblock};
use steins_syntax::{ArgValue, NameRef};

use crate::contract::parse_tag_type;
use crate::{
    Cx, Diagnostic, FnResolution, PHPDOC_MAYBE_UNDEFINED_ID, VARIABLE_MAYBE_UNDEFINED_ID,
    VARIABLE_UNDEFINED_ID, arg_is_by_value,
};
use crate::docblock_hygiene::hygiene_diag;

// ---------------------------------------------------------------------------
// undefined variables (ADR-0078, issue #194)
// ---------------------------------------------------------------------------

/// The `variable.*` pair: `variable.undefined` on a read of a name its scope never
/// binds, and `variable.maybe-undefined` on a read only *some* paths reach bound
/// (ADR-0081, issue #267).
///
/// **One predicate routes between them, and it lives at lowering**: a name the
/// scope binds nowhere lands in `Scope::undefined_reads`, a name it binds
/// somewhere lands in `Scope::maybe_undefined_reads`, and no read can be in
/// both — `check_return_missing`'s shape, a Default-floor definite id and a
/// Strict-floor possibly id, disjoint by construction.
///
/// The firing set is `Scope::undefined_reads`, computed at lowering — every
/// binding form, the `isset`/`empty`/`??`/`unset`/`@` guard exclusions, the
/// superglobal/`$this` exclusions, the top-level and arrow-function silences and
/// the `extract`/`compact`/`$$x`/`eval`/`include` scope dam are all settled
/// there. This function adds the two premises lowering cannot reach:
///
/// 1. **The warning-handler posture** (ADR-0049 §7): warning-plus-`null`, so
///    under a declared `warning-handler = "null"` the finding leaves the proof
///    surface, exactly as `offset.missing` does.
/// 2. **The out-parameter subtraction** (ADR-0077): `preg_match('/a/', $s, $m)`
///    *binds* `$m`, and whether an argument position is by-reference is a
///    property of the **callee's** declaration — the cross-file index for a
///    user function, the catalog's `out_params` rows for a builtin.
///    `arg_is_by_value` is that oracle, and it refuses for every uncertainty
///    (an unresolved/ambiguous name, an argument past the declared arity),
///    keeping this id silent rather than wrong.
///
/// `Scope::poisoned` is deliberately **not** a gate here: its members that
/// matter to a binding question (`global $x`, `static $x`, `$a = &$b`,
/// `use (&$x)`) are binding forms this id reads directly, and the rest
/// (`extract`, `$$v`, `eval`, `include`) already dam the read list at lowering.
/// Gating on the flag would silence every scope that merely declares a
/// `global`.
pub(crate) fn check_undefined_variables(cx: &Cx, out: &mut Vec<Diagnostic>) {
    if !cx.warning_handler_abort {
        return;
    }
    // Nothing to judge in most files: skip the call-site sweep entirely then.
    if cx
        .tree()
        .scopes()
        .iter()
        .all(|s| s.undefined_reads.is_empty() && s.maybe_undefined_reads.is_empty())
    {
        return;
    }
    let bound_by_call = out_param_argument_spans(cx);
    for scope in cx.tree().scopes() {
        // An out-parameter position is a **binding form**, not merely a read that
        // does not count: `preg_match($p, $s, $m); return $m;` must be silent at the
        // `return` too, so a surviving candidate binds its name for the whole scope
        // exactly as `global $x` does.
        //
        // The candidates come from `Scope::ref_arg_candidates`, which is collected
        // independently of the read list. Deriving them from the reads instead made
        // the binding depend on the read being *recorded*, and a guarded occurrence
        // is not: `@proc_open($cmd, $spec, $pipes)` binds `$pipes` in PHP while the
        // `@` withholds the read (symfony/console `Terminal.php`).
        let bound: HashSet<&str> = scope
            .ref_arg_candidates
            .iter()
            .filter(|c| bound_by_call.contains(&c.span.start))
            .map(|c| c.name.as_str())
            .collect();
        for read in &scope.undefined_reads {
            if bound.contains(read.name.as_str()) {
                continue;
            }
            let name = &read.name;
            out.push(hygiene_diag(
                cx,
                VARIABLE_UNDEFINED_ID,
                read.span.start,
                format!(
                    "${name} is never bound in this scope — PHP warns \
                     \"Undefined variable ${name}\" and the read evaluates to null"
                ),
            ));
        }
        // The some-paths leg. Same scope, same warning-handler gate, same
        // out-parameter oracle — with one refinement the definite leg does not
        // need: an out-parameter binds from its **call site forward**, so a
        // confirmed candidate subtracts only the reads that follow it. Subtracting
        // scope-wide (the definite leg's rule) would be wrong in the other
        // direction here — `echo $x; preg_match($p, $s, $x);` reaches its read
        // before the binding — and subtracting nothing would report the shape
        // ADR-0077 exists to keep silent.
        for read in &scope.maybe_undefined_reads {
            let bound_before = scope.ref_arg_candidates.iter().any(|c| {
                c.name == read.name
                    && c.span.start <= read.span.start
                    && bound_by_call.contains(&c.span.start)
            });
            if bound_before {
                continue;
            }
            let name = &read.name;
            out.push(hygiene_diag(
                cx,
                VARIABLE_MAYBE_UNDEFINED_ID,
                read.span.start,
                format!(
                    "${name} is bound on only some of the paths that reach this read \
                     — on the others PHP warns \"Undefined variable ${name}\" and the \
                     read evaluates to null"
                ),
            ));
        }
    }
}

// unset pseudo-type (ADR-0087 §4, issue #396)

/// `phpdoc.maybe-undefined`: a read of a top-level variable declared
/// `/** @var T|unset $x */` while the possibly-undefined state that declaration
/// states is still live.
///
/// The presence half is `SourceTree::unset_seed_facts`, ADR-0081's pass run over the
/// top-level statement list with the declarations as seeds — so every guard in that
/// engine's vocabulary discharges the state here identically. This half adds the two
/// premises lowering cannot reach:
///
/// 1. **The declaration itself.** `steins-syntax` has no edge to the phpdoc lowering,
///    so its seeds are a syntactic superset: every `$name` in a docblock that spells
///    `unset` anywhere. Here the named tag is actually lowered, and a candidate
///    survives only if the lowered contract carries a top-level `ContractTy::Unset`
///    member — a nested one (`array<int, unset>`) is a different claim and seeds
///    nothing.
/// 2. **The out-parameter subtraction** (ADR-0077), on the maybe leg's rule — an
///    out-parameter binds from its **call site forward**, so a confirmed candidate
///    subtracts only the reads that follow it — but over a **confirmed by-reference**
///    argument rather than a not-confirmed-by-value one ([`by_ref_argument_spans`]).
///
/// The ADR-0049 §7 warning-handler gate is deliberately absent — see
/// [`PHPDOC_MAYBE_UNDEFINED_ID`].
pub(crate) fn check_phpdoc_maybe_undefined(cx: &Cx, out: &mut Vec<Diagnostic>) {
    let facts = cx.tree().unset_seed_facts();
    if facts.reads.is_empty() {
        return;
    }
    let mut declared: HashMap<u32, HashMap<String, String>> = HashMap::new();
    let mut bound_by_call: Option<HashSet<u32>> = None;
    for read in &facts.reads {
        let tags = declared
            .entry(read.seed_stmt)
            .or_insert_with(|| unset_declared_names(cx, read.seed_stmt));
        let Some(spelling) = tags.get(&read.name).cloned() else { continue };
        let calls = bound_by_call.get_or_insert_with(|| by_ref_argument_spans(cx));
        let bound_before = facts.ref_arg_candidates.iter().any(|c| {
            c.name == read.name && c.span.start <= read.span.start && calls.contains(&c.span.start)
        });
        if bound_before {
            continue;
        }
        let name = &read.name;
        out.push(hygiene_diag(
            cx,
            PHPDOC_MAYBE_UNDEFINED_ID,
            read.span.start,
            format!(
                "${name} is declared {spelling} and may be undefined at this read — \
                 guard it with isset(${name}) or give it a default"
            ),
        ));
    }
}

/// The names a statement's adopted docblock declares possibly-unbound, each with the
/// spelling the author wrote — the confirmation half of
/// [`check_phpdoc_maybe_undefined`].
///
/// The tag-selection rules are [`apply_inline_var_casts`]': a property target
/// (`@var T $this->p`) speaks about a property rather than a local, `$this` is never
/// a local, and a prefixed `@phpstan-var`/`@psalm-var` displaces the plain `@var` for
/// the same variable (ADR-0029 precedence). Class resolution and `@template`
/// shadowing are not: neither can turn an `unset` member into something else, and
/// `unset` is non-shadowable vocabulary (ADR-0087 §2.2).
///
/// [`apply_inline_var_casts`]: crate::apply_inline_var_casts
fn unset_declared_names(cx: &Cx, stmt_start: u32) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(doc) = cx.tree().stmt_docblock(stmt_start) else { return out };
    let tags = scan_docblock(&doc.text);
    for tag in &tags {
        if !matches!(tag.kind, TagKind::Var) || tag.property_target {
            continue;
        }
        let Some(var) = &tag.var_name else { continue };
        let name = var.trim_start_matches('$');
        if name.is_empty() || name == "this" {
            continue;
        }
        if !tag.prefixed
            && tags.iter().any(|t| {
                matches!(t.kind, TagKind::Var) && t.prefixed && t.var_name == tag.var_name
            })
        {
            continue;
        }
        let Some(pt) = parse_tag_type(&tag.type_text) else { continue };
        if !declares_unset(&steins_contract::lower(&pt)) {
            continue;
        }
        out.insert(name.to_owned(), tag.type_text.trim().to_owned());
    }
    out
}

/// The byte offsets of every bare-variable argument in this file a call **provably
/// writes**: the declaration says `&$p`, or the builtin catalog rows the position as
/// an out-parameter.
///
/// The mirror image of [`out_param_argument_spans`], and the difference is the whole
/// point. That one asks "could this be an out-parameter?" and answers yes for every
/// uncertainty, which is right for a *proof*-layer id: it trades recall for a bar
/// that admits no false positive. Here the premise is a declaration the author wrote
/// down, and the same conservatism would delete the claim wholesale — `date_format`
/// carries no catalog row, so `date_format($x, 'c')` is "maybe an out-parameter" to
/// that oracle, and the conformance fixture's own second probe would go silent.
/// A builtin whose reference parameters PHP actually declares is rowed; an
/// unresolvable callee proves nothing about the binding, and this id reports it.
fn by_ref_argument_spans(cx: &Cx) -> HashSet<u32> {
    let mut spans = HashSet::new();
    for call in cx.tree().calls() {
        for (position, arg) in call.args.iter().enumerate() {
            if !matches!(arg.value, ArgValue::Var(_)) {
                continue;
            }
            let by_ref = call
                .callee_ref
                .as_ref()
                .is_some_and(|callee| arg_is_by_ref(cx, callee, position as u32));
            if by_ref {
                spans.insert(arg.span.start);
            }
        }
    }
    spans
}

/// Whether argument `position` of `callee` is **certainly** by-reference — the
/// positive half of [`arg_is_by_value`], refusing for every uncertainty in the other
/// direction: an unresolved name, a rowless builtin, an argument past the declared
/// arity, a variadic position.
fn arg_is_by_ref(cx: &Cx<'_>, callee: &NameRef, position: u32) -> bool {
    let position = position as usize;
    match cx.resolve_arg_function(callee) {
        FnResolution::Builtin(builtin_name) => {
            steins_catalog::by_value_arg(&builtin_name, position) == Some(false)
        }
        FnResolution::User(fn_site) => {
            matches!(cx.fn_decl(fn_site).params.get(position), Some(p) if p.by_ref && !p.variadic)
        }
        FnResolution::Unknown => false,
    }
}

/// Whether a lowered contract carries the `unset` pseudo-type as a **top-level**
/// member: the whole type, or an arm of the union it flattens to.
///
/// Nested positions are deliberately not reached. `array<int, unset>` says something
/// about an array's values, not about whether `$x` is bound, and reading it as the
/// latter would manufacture a claim out of a spelling ADR-0087 §5 has not decided.
fn declares_unset(ty: &ContractTy) -> bool {
    match ty {
        ContractTy::Union(members) => members.iter().any(declares_unset),
        other => other.is_unset(),
    }
}

// end unset pseudo-type (ADR-0087 §4, issue #396)

/// The byte offsets of every bare-variable argument in this file that a call could
/// be **writing** rather than reading — the out-parameter subtraction of
/// [`check_undefined_variables`].
///
/// Only statically-named function calls reach here: `SourceTree::calls()` is the
/// comprehensive file-wide function-call surface, and every other call shape
/// (method, static, dynamic, `new`, every named argument) already binds its
/// bare-variable arguments at lowering, where no callee name existed to ask about.
/// The span keys the join because `lower_argument_list` records a positional
/// argument's own expression span, which for a bare `$x` is the same token span the
/// read carries.
fn out_param_argument_spans(cx: &Cx) -> HashSet<u32> {
    let mut spans = HashSet::new();
    for call in cx.tree().calls() {
        for (position, arg) in call.args.iter().enumerate() {
            if !matches!(arg.value, ArgValue::Var(_)) {
                continue;
            }
            let by_value = call
                .callee_ref
                .as_ref()
                .is_some_and(|callee| arg_is_by_value(cx, callee, position as u32));
            if !by_value {
                spans.insert(arg.span.start);
            }
        }
    }
    spans
}

// end undefined variables (ADR-0078, issue #194)
