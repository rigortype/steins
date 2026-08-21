//! String context (ADR-0078 / issue #193): `string.non-stringable` and
//! `string.array-conversion` — a value the lanes prove cannot be coerced at a site
//! PHP reads as a string.

use std::collections::HashMap;

use steins_domain::{Certainty, Fact};
use steins_syntax::{ArgValue, Stmt, StringContextSite};

use crate::contract::IsA;
use crate::cx::Cx;
use crate::env::{Known, Store, Stratum, singleton_fact};
use crate::predicates::{TypePred, pred_holds_on_fact};
use crate::project::Diagnostic;
use crate::walk::WalkCx;
use crate::{Folder, STRING_ARRAY_CONVERSION_ID, STRING_NON_STRINGABLE_ID};
use crate::absence::{ChainWalk, UndefKind, enumerate_method_chain};

// ---------------------------------------------------------------------------
// string context (ADR-0078, issue #193): `string.non-stringable` +
// `string.array-conversion`.
// ---------------------------------------------------------------------------

/// Judge every string conversion this statement performs (ADR-0078, issue #193).
///
/// The sites come from the lowering ([`Stmt::string_contexts`]), which owns *where*
/// the conversions are — an `echo`/`print` operand, an interpolated string's
/// embedded expressions, a `(string)` cast, and both operands of `.` / `.=`. This
/// owns *whether each one is legal*, and the default answer is that it is: an
/// `int`, a `float`, a `bool`, a `null` and a `string` all convert silently and
/// totally, so they are never a finding however precisely they are proven. Two
/// values are not:
///
/// * an **array** — a warning plus the literal string `"Array"`
///   ([`STRING_ARRAY_CONVERSION_ID`]);
/// * an **object with no reachable `__toString`** — a fatal `Error`
///   ([`STRING_NON_STRINGABLE_ID`]).
///
/// A site whose value is neither proven array nor proven exact-class object is
/// silence, which covers every `Maybe` by construction.
pub(crate) fn check_string_contexts(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    for site in &stmt.string_contexts {
        check_string_context_site(w, folder, site, env, store, out);
    }
}

/// One conversion site: the array leg, then the object leg. They are disjoint by
/// construction (a value-domain [`Fact`] never denotes an object), so at most one
/// finding is produced per site.
fn check_string_context_site(
    w: &WalkCx,
    folder: &mut dyn Folder,
    site: &StringContextSite,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    // The array leg. Warning-grade, so it rides the ADR-0049 §7 lever: under a
    // declared `warning-handler = "null"` the application tolerates the warning and
    // the finding leaves the proof surface, exactly as `offset.missing` does.
    if cx.warning_handler_abort && string_context_is_array(w, folder, &site.value, env) {
        let pos = cx.tree().position(site.span.start);
        out.push(Diagnostic {
            id: STRING_ARRAY_CONVERSION_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "array in {} — PHP warns \"Array to string conversion\" and converts it to \
                 the literal string \"Array\"",
                site.kind.render(),
            ),
            facet: None,
            fix: None,
        });
        return;
    }
    check_non_stringable(w, folder, site, store, out);
}

/// The fatal leg: an object whose class provably declares no `__toString` anywhere
/// a runtime lookup could find one. Reuses `call.undefined-method`'s ladder for one
/// method name: an exactly-known class ([`undefined_method_receiver`]'s discipline —
/// a lower bound is not enough since a subclass may declare `__toString`; `$this` is
/// membership, not exactness, unless `final`); a live monkey-patch-free boot surface
/// (`absence_family_available`, ADR-0049 A9); `Stringable` refuted by the is-a oracle
/// (which already knows PHP 8.0's implicit implementation — any `__toString` in the
/// hierarchy answers `Yes`, a trait-using class `Unknown`); the chain fully enumerated
/// with `__toString` absent from every node ([`enumerate_method_chain`] — unresolvable/
/// ambiguous ancestor, builtin ancestor, trait, enum, or an A14 magic-tag stop the
/// walk); and the dam plus the A2ii homonym leg, as the method id applies them.
///
/// [`undefined_method_receiver`]: crate::absence::undefined_method_receiver
fn check_non_stringable(
    w: &WalkCx,
    folder: &mut dyn Folder,
    site: &StringContextSite,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let Some(class_fqn) = string_context_object_class(cx, store, &site.value, w.scope.poisoned)
    else {
        return;
    };
    if !folder.absence_family_available() {
        return;
    }
    if cx.is_a(&class_fqn, "Stringable") != IsA::No {
        return;
    }
    let ChainWalk::Absent { simple_chain, fqns, any_conditional } =
        enumerate_method_chain(cx, &class_fqn, "__toString", UndefKind::Instance)
    else {
        return;
    };
    if any_conditional && !cx.dam.is_clear() {
        return;
    }
    for fqn in &fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }

    // Every leg holds — a proven `Error: Object of class C could not be converted
    // to string`, witnessed on PHP 8.5.9 in all five contexts.
    let pos = cx.tree().position(site.span.start);
    let display = cx.class_display_fqn(&class_fqn);
    let chain_render = simple_chain.join(" → ");
    out.push(Diagnostic {
        id: STRING_NON_STRINGABLE_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "object of class {display} in {} — hierarchy fully enumerated ({chain_render}), \
             no __toString, not Stringable: PHP raises \
             \"Error: Object of class {display} could not be converted to string\"",
            site.kind.render(),
        ),
        facet: None,
        fix: None,
    });
}

/// Whether a string-context operand is **provably an array** at the `Verified`
/// stratum a proof-layer finding requires (ADR-0052 §5): a proven array value, in
/// the env or resolved by the fold. `Certainty::Maybe` (`array|string`) is silence
/// by construction.
///
/// Deliberately not evidence: a **declared** array's `Fact::Shape` is always
/// `Asserted` (ADR-0062 A-G9's corollary — a declared shape's contents are a claim
/// the runtime never checks), so the docblock case (`@param array<int, string> $a`)
/// is covered by the stratum check above. A bare native `array $x` parameter — which
/// PHP *does* enforce with a `TypeError`, and is the commonest shape of this finding
/// elsewhere — reports nothing today: `TypeMember` has no array member, so an
/// `array` hint lowers to `None` with no `Verified` arm to read. Widening that is an
/// IR change (array vocabulary joining the native envelope), recorded silence until then.
fn string_context_is_array(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> bool {
    matches!(
        string_context_fact(w, folder, value, env),
        Some((fact, Stratum::Verified)) if pred_holds_on_fact(TypePred::Array, &fact) == Certainty::Yes
    )
}

/// The value-domain fact behind a string-context operand, with its trust stratum:
/// a bare variable's env binding, else whatever the operand resolves to as a
/// literal. Anything else — a call, an offset read, an object — yields `None`,
/// which is silence.
fn string_context_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<(Fact, Stratum)> {
    if let ArgValue::Var(name) = value {
        if w.scope.poisoned {
            return None;
        }
        let known = env.get(name)?;
        return Some((known.fact.clone()?, known.stratum));
    }
    let (lit, stratum) = w.cx.resolve_literal_strat(value, env, w.scope.poisoned, folder)?;
    Some((singleton_fact(&lit, w.cx.php_minor)?, stratum))
}

/// The **exact** class of a string-context operand that is an object, or `None`.
/// A `new C(...)` and a resolved enum case are exact by construction; a variable
/// must be bound to a heap object the store knows exactly (`class_exact`), which is
/// the same bound [`undefined_method_receiver`] requires and for the same reason.
///
/// [`undefined_method_receiver`]: crate::absence::undefined_method_receiver
fn string_context_object_class(
    cx: &Cx,
    store: &Store,
    value: &ArgValue,
    poisoned: bool,
) -> Option<String> {
    if let ArgValue::Var(name) = value {
        if poisoned {
            return None;
        }
        let obj = store.obj_of(name)?;
        return obj.class_exact.then(|| obj.class.clone());
    }
    cx.proven_object_class(value)
}
