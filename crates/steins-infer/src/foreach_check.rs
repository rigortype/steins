//! `foreach.non-iterable` (ADR-0078 / issue #192): a `foreach` subject the value and
//! fact lanes prove is not iterable.

use std::collections::HashMap;

use steins_domain::{Base, Fact, Val};
use steins_syntax::ForeachSite;

use crate::{Cx, Diagnostic, FOREACH_NON_ITERABLE_ID, Known, Stratum, describe_fact, render_val};

// ---------------------------------------------------------------------------
// `foreach.non-iterable` (ADR-0078, issue #192).
// ---------------------------------------------------------------------------

/// The PHP word `foreach()`'s own warning would print for a proven-concrete
/// non-array `Val` (`php -r`-witnessed, 8.5.9: `foreach() argument must be of
/// type array|object, {word} given`), or `None` for an array (iterable — never
/// reaches this check).
fn foreach_subject_word(v: &Val) -> Option<&'static str> {
    match v {
        Val::Null => Some("null"),
        Val::Int(_) => Some("int"),
        Val::Float(_) => Some("float"),
        Val::Bool(true) => Some("true"),
        Val::Bool(false) => Some("false"),
        Val::Str(_) => Some("string"),
        Val::Array(_) => None,
    }
}

/// The definite PHP word `foreach()`'s own warning would print for a `Refined`/
/// `General` fact, when the base/nullability pin exactly one — `int`/`float`/
/// `string` at `nullable: false` (the same words a concrete `Val` of that base
/// renders as). `None` when the word is not pinned: `nullable: true` (the
/// runtime value could be the base OR `null` — two different words) or a `Bool`
/// base (PHP's warning names the concrete `true`/`false`, never the bare word
/// `bool`, and the abstract layers never carry a bool's truth value).
fn foreach_subject_abstract_word(fact: &Fact) -> Option<&'static str> {
    match fact {
        Fact::Refined { base, nullable: false, .. } | Fact::General { base, nullable: false } => {
            match base {
                Base::Int => Some("int"),
                Base::Float => Some("float"),
                Base::String => Some("string"),
                Base::Bool => None,
            }
        }
        _ => None,
    }
}

/// Judge one [`ForeachSite`]'s subject against the entry env and emit
/// `foreach.non-iterable` when it is proven a non-array scalar/`null` (ADR-0078,
/// issue #192).
///
/// Reuses `SourceTree::foreach_sites()` (ADR-0076) rather than re-lowering the
/// construct — the checker's own trace erases `foreach` into an undifferentiated
/// `StmtKind::Opaque` (ADR-0027), so the site list is where "this IS a foreach"
/// survives.
///
/// The subject's fact comes from the SAME lane `offset.missing` reads (a bare
/// `Var` in `env`, required `Verified` — an `Asserted`/docblock claim never
/// premises this proof, ADR-0052 §5). Only a `Singleton` over a non-array `Val`
/// and a scalar-base `Refined`/`General` fact fire; a `Fact::Shape` (an array,
/// always iterable) and a `Fact::OneOf` (no single warning word to attribute)
/// stay silent.
///
/// Every other silence leg falls out of the same gate: a plain object carries no
/// `Fact` at all (the domain `Val` has no object variant — object state lives in
/// the heap/store, ADR-0036), so `Traversable`/`Generator`/a plain object/an
/// unenumerable hierarchy are simply unproven. An `iterable`-declared native
/// parameter contributes no `Fact` either (ADR-0002 silence). A `None` fact, a
/// poisoned scope, or an `Asserted`-stratum fact all stay silent by the same
/// gates `offset.missing` uses.
pub(crate) fn check_foreach_subject(
    cx: &Cx,
    site: &ForeachSite,
    env: &HashMap<String, Known>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    // The warning-handler posture (ADR-0049 §7): a refusal is warning-plus-a-
    // skipped-body, silenced under a declared `warning-handler = "null"` posture
    // exactly as `offset.missing` is.
    if !cx.warning_handler_abort {
        return;
    }
    if poisoned {
        return;
    }
    let Some(name) = &site.subject else { return };
    let Some(known) = env.get(name) else { return };
    if known.stratum != Stratum::Verified {
        return;
    }
    let Some(fact) = &known.fact else { return };

    let message = match fact {
        Fact::Singleton(v) => {
            let Some(word) = foreach_subject_word(v) else { return };
            format!(
                "foreach subject ${name} is provably {} — the loop body never runs; \
                 warns with \"foreach() argument must be of type array|object, {word} given\"",
                render_val(v),
            )
        }
        // A union has no single type word for the engine's message, and this
        // finding quotes that word verbatim.
        Fact::Union { .. } => return,
        Fact::Refined { .. } | Fact::General { .. } => {
            let desc = describe_fact(fact);
            match foreach_subject_abstract_word(fact) {
                Some(word) => format!(
                    "foreach subject ${name} is provably {desc} — the loop body never runs; \
                     warns with \"foreach() argument must be of type array|object, {word} given\"",
                ),
                None => format!(
                    "foreach subject ${name} is provably {desc} — never array or object, so \
                     PHP's foreach() warns and the loop body never runs",
                ),
            }
        }
        Fact::OneOf(_) | Fact::Shape { .. } => return,
    };
    let pos = cx.tree().position(site.span.start);
    out.push(Diagnostic {
        id: FOREACH_NON_ITERABLE_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}
