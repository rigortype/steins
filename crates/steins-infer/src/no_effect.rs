//! `statement.no-effect` (issue #320, ADR-0096): a statement-position call whose
//! result is unused and whose callee — **at this call site** — is proven to do
//! nothing the rest of the program can observe.
//!
//! PHPStan spells this family as `CallTo*StatementWithoutSideEffectsRule` over a
//! curated `hasSideEffects` boolean. Steins already proves a finer thing — the
//! effect labels of ADR-0018 — so the boolean is replaced by a set membership:
//! the proven labels must all lie in [`DISCARDABLE`]. The set is a *policy*
//! (ADR-0096 §2), not a derivation, which is why it is written out here in one
//! place with the argument for each member and each exclusion beside it.
//!
//! The catalog's colour is a row **per name**, and for most of the names it
//! colours pure the row is the fold allowlist's: `strlen` is `Some(&[])` because
//! it folds, and a name folds because it is pure *given literal arguments*. That
//! calibration is the whole of the evidence, so the predicate holds a call to it
//! (ADR-0096 §3): the arguments must be the literals the fold gate would admit,
//! typed exactly as the parameter declares, within the arity the engine reports,
//! at no position that takes a callback. Under those terms nothing an argument
//! could do — a `__toString`, a `Countable::count`, a callable named by a string,
//! a coercion the engine deprecates — is left for the call to do, and what the
//! catalog says about the name is what happens at the site.
//!
//! The judgment is made **inside the walk**, at the statement, rather than in a
//! reporting pass of its own. That is what keeps ADR-0092's warm property: a
//! reporting pass would have to scan every file's trace for statement-position
//! calls, and a run that changed nothing is required to decode no tree at all.
//! Riding the walk instead puts the finding in the per-file block a warm run
//! replays.
//!
//! **This slice answers catalogued builtins only** (ADR-0096 §5). The predicate
//! over a *project* callee is the same shape, but its premise is a whole-project
//! fixpoint result, and the walk forces neither fixpoint today; §5 records what
//! pricing it per file would take.

use steins_syntax::{ArgValue, CallExpr, Span};

use crate::cx::Cx;
use crate::project::{Diagnostic, FnResolution};
use crate::{
    CALL_PRINTF_TOO_FEW_ARGUMENTS_ID, CALL_TOO_FEW_ARGUMENTS_ID, CALL_TOO_MANY_ARGUMENTS_ID,
    CALL_UNDEFINED_FUNCTION_ID, ID as TYPE_ARGUMENT_MISMATCH_ID, PREG_INVALID_PATTERN_ID,
    STATEMENT_NO_EFFECT_ID,
};

/// The effect labels a **discarded** call may carry and still be a dead
/// statement (ADR-0096 §2). Read as a set of subsumption roots: a proven label
/// qualifies when one of these covers it (ADR-0018's dot-path order), so
/// `io.fs.read` admits itself and nothing wider.
///
/// Each member is a judgment call, and so is each exclusion:
///
/// * `global.read` / `nondet.random` / `nondet.time` — reading a value and then
///   throwing it away changes nothing. `rand();` is the case the curated boolean
///   structurally could not express: `rand` is impure, never foldable, and
///   discarding its result is still dead code.
/// * `io.fs.read` — declines to count the atime a read updates. A filesystem
///   read performed for its timestamp is a program nobody writes. The label is
///   **policy-reserved and inert in this slice**: it is only ever minted by
///   [`steins_catalog::narrowed_stream_labels`], which the effects pass calls and
///   this judgment does not, so no statement reaches this set through it today.
/// * `io.input` is **excluded** though it is also a read: consuming a stream
///   advances its position, so `fgets($h);` is how a program skips a line.
/// * `mutate.local` is **excluded**: `sort($rows);` is *called* for the
///   caller-visible mutation, and its return value is the thing nobody wants.
const DISCARDABLE: &[&str] = &["global.read", "nondet.random", "nondet.time", "io.fs.read"];

/// Names the catalog colours pure and the argument bar would admit, refused
/// here because a literal call to them can still **raise an engine diagnostic
/// or write engine state** — neither of which a catalog row records, and
/// neither of which the fold seam can see (its runner routes every warning,
/// notice and deprecation to a stream the parent discards, and answers a value
/// as if nothing had been said). A diagnostic reaches `set_error_handler`, so a
/// call that can raise one may run user code; that is a refusal under the bar,
/// not a "non-effect" (ADR-0096 §3).
///
/// Each row is one name and one witness, reproduced on PHP 8.5.10. The list is
/// held to the names it would otherwise judge: a name here that the rest of the
/// predicate would already decline is a row nobody needs, and a test says so.
const REFUSED_ON_LITERALS: &[(&str, &str)] = &[
    // `json_last_error()` reads what this last wrote — a global write the
    // catalog does not colour — and a literal `4194304` (`JSON_THROW_ON_ERROR`)
    // in the flags makes `json_encode("\xff", 4194304)` a `JsonException`, the
    // arm the catalog carries only under a synthetic key. `json_decode` writes
    // the same slot and declines earlier, on its `ValueError` row.
    ("json_encode", "writes the JSON error slot; throws under a literal flag"),
    // `preg_split('/(/', 'a')` warns on the pattern and writes
    // `preg_last_error()`'s slot; the other `preg_*` names decline earlier on
    // their out-parameter or throw rows.
    ("preg_split", "compile warning on the pattern; writes the PCRE error slot"),
    // A stray character in the digit string is a deprecation, not a throw.
    ("bindec", "deprecation on a stray character"),
    ("hexdec", "deprecation on a stray character"),
    // A mask with a decreasing `..` range warns and continues.
    ("trim", "warning on a decreasing `..` mask range"),
    ("ltrim", "warning on a decreasing `..` mask range"),
    ("rtrim", "warning on a decreasing `..` mask range"),
    ("chop", "warning on a decreasing `..` mask range"),
    // An unrecognized format token warns and answers `false`.
    ("idate", "warning on an unrecognized format token"),
    // `strtr('abc', 'b')` is a `TypeError` on the two-argument shape with a
    // string `$from` — a throw the declared `array|string` cannot show.
    ("strtr", "shape-dependent TypeError the parameter types cannot show"),
    // The same class as `strtr`: the legacy signature `array|string
    // $separator, ?array $array` admits `implode('a')`, `implode('x', null)`
    // and `implode([1], [2])` by type, and the engine throws `TypeError` on
    // each because the shapes it accepts are coupled across the two positions.
    ("implode", "shape-dependent TypeError the parameter types cannot show"),
    ("join", "shape-dependent TypeError the parameter types cannot show"),
    // `substr_replace('abc', 'x', [1])`: an array `$offset` or `$length` is
    // admitted by the declared `array|int`, and a `TypeError` on a string
    // subject — the array arms are for the array-subject shape.
    ("substr_replace", "array offset or length on a string subject is a TypeError"),
];

/// Names whose admitted argument counts are not an interval, so the engine's
/// `params_required`..`params.len()` reading admits a count the call refuses.
/// `rand()` takes exactly zero or exactly two arguments — `rand(1)` is an
/// `ArgumentCountError` raised from inside the call — and reflection, which
/// `param_facts` is mined from, can only say "0 required of 2" for it.
/// `mt_rand` has the same shape and needs no row: its `ValueError` throw row
/// declines the name earlier. The arity checker is silent on `rand(1)` too,
/// so the co-fire rule does not catch it.
const ARITY_SETS: &[(&str, &[usize])] = &[("rand", &[0, 2])];

/// The ids a checker that ran on this same call emits when it has **proven the
/// call throws** or otherwise cannot be the no-op line: a `TypeError` on an
/// argument, an arity error at either end, a format string demanding arguments
/// it was not given, a pattern the project's own PCRE refuses, and a callee
/// that does not exist. A statement one of these has judged is a validity check
/// or a bug, and never dead code as well (ADR-0096 §3, the co-fire rule).
const PROVEN_TO_THROW: &[&str] = &[
    TYPE_ARGUMENT_MISMATCH_ID,
    CALL_TOO_FEW_ARGUMENTS_ID,
    CALL_TOO_MANY_ARGUMENTS_ID,
    CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
    PREG_INVALID_PATTERN_ID,
    CALL_UNDEFINED_FUNCTION_ID,
];

/// Whether one proven label is discardable — covered by some [`DISCARDABLE`]
/// root under ADR-0018 subsumption.
fn discardable(label: &str) -> bool {
    DISCARDABLE.iter().any(|root| steins_catalog::subsumes(root, label))
}

/// Report `statement.no-effect` for one statement-position call, or say nothing.
/// `stmt` is the whole statement's span — the thing a reader would delete — and
/// `call` the call written as that statement. `before` is `out`'s length when
/// the walk began this statement's checks: everything appended since is what
/// the earlier checkers concluded about this same call.
///
/// Called from the walk under `descent.is_none()`, so a binding descent never
/// re-judges a site the plain per-scope pass already judged.
pub(crate) fn check_no_effect(
    cx: &Cx,
    stmt: Span,
    call: &CallExpr,
    before: usize,
    out: &mut Vec<Diagnostic>,
) {
    // Named functions only, and `callee_ref` is that gate on its own: the
    // lowering fills it only for a statically-named **function** call, so a
    // method, static, dynamic or constructor receiver answers here (ADR-0096
    // §5). A statement-position method call needs the effect lane's own receiver
    // resolution, and the receiver forms that lane can resolve (`$this->`,
    // `self::`, `Foo::`) are not where a discarded call is written: the everyday
    // `$config->getTimeout();` dispatches on a variable, which draws no edge in
    // the effect graph and would be silent whatever this function did.
    let Some(name) = call.callee_ref.as_ref() else { return };
    let does_nothing = match cx.resolve_effect_function(name) {
        FnResolution::Builtin(builtin) => {
            builtin_does_nothing(&builtin) && literal_call_shape(&builtin, call)
        }
        // The project half, deferred with the module doc's reason: its premise
        // is a whole-project fixpoint the walk does not force.
        FnResolution::User(_) => false,
        // An unresolved or ambiguous name is the `…?` of the function world:
        // nothing is known about what the call does.
        FnResolution::Unknown => false,
    };
    if !does_nothing {
        return;
    }
    // The co-fire rule: a checker that already proved this call throws has
    // said what the statement is for. `out[before..]` is exactly what the walk
    // appended about this statement, so no line arithmetic is involved.
    if out[before..].iter().any(|d| PROVEN_TO_THROW.contains(&d.id)) {
        return;
    }
    // The call's own spelling, not the catalog name it resolved to — a reader
    // looking at `t()` in their source wants to be told about `t()`.
    let display = call.callee.as_deref().unwrap_or_else(|| name.simple());
    let pos = cx.tree().position(stmt.start);
    out.push(Diagnostic {
        id: STATEMENT_NO_EFFECT_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "`{display}()` has no effect anything can observe and its result is unused — the statement does nothing"
        ),
        facet: None,
        fix: None,
    });
}

/// The catalog's verdict on a builtin **name** — ADR-0096 §3's callee-side
/// legs, each answered by its own catalog row, and each missing row a different
/// kind of silence. Argument-blind by construction; [`literal_call_shape`] is
/// the half that reads the call.
fn builtin_does_nothing(name: &str) -> bool {
    // The colour row. A name resolves to `FnResolution::Builtin` when it has a
    // colour OR an out-parameter row, so a name that reaches here without a
    // colour is one the wider half admitted — and that name is declined on its
    // out-parameter row just below either way. The `None` arm is therefore not
    // a leg of its own; it is written so the match is total.
    let Some(labels) = steins_catalog::effect_labels(name) else { return false };
    // A by-ref out-parameter row means the call writes through an argument, the
    // very thing `mutate.local`'s exclusion is about — and whether it wrote is
    // conditional on what this call site passed, which is a question the catalog
    // row alone does not answer. Decline the whole name: `shuffle($rows);` has a
    // discardable colour (`nondet.random`) and is written for the write.
    if steins_catalog::out_params(name).is_some() {
        return false;
    }
    // The throw row (ADR-0007/0040): a mined throw row is a non-empty throw set,
    // so the call can be a validity check and the statement is not dead — the
    // reason PHPStan exempts a throwing call, and the reason `random_int(1, 10);`
    // and `str_repeat('a', 3);` alike stay silent here. The row is per name and
    // says nothing about the values this site passed; the seam that could
    // (the fold) cannot tell a value from a value-with-a-deprecation, so the
    // row is read as written rather than re-derived per call.
    if steins_catalog::builtin_throws(name).is_some() {
        return false;
    }
    // The diagnostic and engine-state refusals, for the same reason in the
    // other direction: what the row cannot say, this list says.
    if REFUSED_ON_LITERALS.iter().any(|(n, _)| *n == name) {
        return false;
    }
    // Every proven label discardable. A `void` builtin cannot reach here: a name
    // is either catalogued pure — which for a `void` builtin would make it a
    // no-op nobody shipped — or carries one of the [`DISCARDABLE`] read colours,
    // and every one of those rows returns the value it read.
    labels.iter().all(|l| discardable(l))
}

/// The argument bar (ADR-0096 §3): does this call pass the builtin exactly the
/// arguments its pure-given-literals calibration was made for?
///
/// Every argument must be a **flat literal** — a scalar, or an array literal of
/// scalars — spelled positionally, with no spread and no name; the count must
/// lie inside the arity the engine reports, and inside [`ARITY_SETS`] where the
/// engine's interval is wider than the call's; no position passed may be one
/// that takes a callback; and each literal must be **exactly** of a type the
/// parameter declares, `null` only where the parameter is nullable. Exactness is
/// what rules out both halves of a coercion: a `TypeError` under
/// `strict_types=1` and the deprecation the coercive mode raises instead
/// (`strlen(null)`, a float with a fraction into `int`). An object cannot be
/// written as a literal, so `__toString`, `Countable`, `JsonSerializable` and
/// `ArrayAccess` are out by construction — and a string literal at a `callable`
/// position names user code, which is why `callable` is not a type any literal
/// satisfies here.
///
/// A name the engine's parameter table does not carry has no arity or types to
/// hold the call to, and declines.
fn literal_call_shape(name: &str, call: &CallExpr) -> bool {
    if !call.positional_only || call.has_spread || !call.named_args.is_empty() {
        return false;
    }
    let Some(facts) = steins_catalog::param_facts(name) else { return false };
    let n = call.args.len();
    let variadic = facts.variadic.first().copied();
    if n < facts.params_required || (variadic.is_none() && n > facts.params.len()) {
        return false;
    }
    // A count inside the interval can still be one the call refuses.
    if let Some((_, counts)) = ARITY_SETS.iter().find(|(row, _)| *row == name)
        && !counts.contains(&n)
    {
        return false;
    }
    call.args.iter().enumerate().all(|(i, arg)| {
        // Deliberately redundant with the exact typing below, which admits no
        // literal at a `callable` member: this is the rule stated in the terms
        // the engine's own table uses, so it survives a rendering the type
        // reader does not (`Closure|string`, say) — and its deletion test
        // fails nothing today, which is the redundancy on record.
        if facts.callable.contains(&i) {
            return false;
        }
        // Past the declared list only a variadic binds, and it binds by its own
        // declared type.
        let slot = match variadic {
            Some(v) if i >= v => v,
            _ => i,
        };
        let Some(declared) = facts.params.get(slot) else { return false };
        flat_literal(&arg.value) && literal_exactly_of(declared, &arg.value)
    })
}

/// A scalar literal, or an array literal whose every element is one. A nested
/// array is refused whole: the string family renders an element to a string
/// (`implode`, `str_replace`), and an array element there is an "Array to
/// string conversion" warning the catalog row does not record.
fn flat_literal(v: &ArgValue) -> bool {
    match v {
        ArgValue::Array(items) => items.iter().all(|(_, e)| e.is_literal()),
        v => v.is_literal(),
    }
}

/// Whether the literal `v` is **exactly** of a type the engine's rendered
/// parameter type `declared` admits — PHP's strict-mode acceptance, with the
/// one widening strict mode itself performs (`int` into `float`). `mixed` admits
/// every **scalar** literal, `null` only a nullable slot, and a class, `callable`
/// or `iterable` member admits nothing a literal can be, except that an array
/// literal is an `iterable`.
///
/// An array literal is admitted only where the declaration names `array` or
/// `iterable`, and **not** at `mixed`: a `mixed` slot is one the engine does
/// not type-check, so what the builtin does with an array there is its own
/// business, and for the string family that business is an "Array to string
/// conversion" warning (`strval([1])`). The rule is uniform rather than a row
/// per name, so `intval([1])`, `boolval([1])` and `gettype([1])` — which take
/// an array without a word — are silent with it (ADR-0096 §3).
fn literal_exactly_of(declared: &str, v: &ArgValue) -> bool {
    let (nullable, members) = match declared.strip_prefix('?') {
        Some(rest) => (true, rest),
        None => (false, declared),
    };
    let admits = |wanted: &[&str]| {
        members.split('|').any(|m| wanted.iter().any(|w| m.eq_ignore_ascii_case(w)))
    };
    match v {
        ArgValue::Null => nullable || admits(&["null", "mixed"]),
        ArgValue::Int(_) => admits(&["int", "float", "mixed"]),
        ArgValue::Float(_) => admits(&["float", "mixed"]),
        ArgValue::Str(_) => admits(&["string", "mixed"]),
        ArgValue::Bool(true) => admits(&["bool", "true", "mixed"]),
        ArgValue::Bool(false) => admits(&["bool", "false", "mixed"]),
        ArgValue::Array(_) => admits(&["array", "iterable"]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ARITY_SETS, REFUSED_ON_LITERALS, builtin_does_nothing, discardable, literal_exactly_of,
    };
    use steins_domain::PhpStr;
    use steins_syntax::ArgValue;

    /// A refusal row is held to a name the rest of the predicate would judge:
    /// one the catalog colours discardable with no out-parameter and no throw
    /// row. A row for any other name is dead weight, and a row that stops
    /// earning its place — because a throw row arrives for the name, say — is
    /// caught here rather than left to mislead a reader.
    #[test]
    fn every_refusal_row_names_a_call_the_rest_would_judge() {
        for (name, why) in REFUSED_ON_LITERALS {
            let labels = steins_catalog::effect_labels(name)
                .unwrap_or_else(|| panic!("{name} ({why}) is not catalogued at all"));
            assert!(labels.iter().all(|l| discardable(l)), "{name} declines on its colour: {labels:?}");
            assert!(steins_catalog::out_params(name).is_none(), "{name} declines on its out-param row");
            assert!(steins_catalog::builtin_throws(name).is_none(), "{name} declines on its throw row");
            assert!(!builtin_does_nothing(name), "{name} must be refused");
        }
    }

    /// An arity-set row earns its place the same way: the name must be one the
    /// rest of the predicate would judge, and the set must lie inside the
    /// interval the engine's table reports — a count outside it is declined
    /// already, and a set equal to the whole interval says nothing.
    #[test]
    fn every_arity_set_row_refines_an_interval_the_rest_would_admit() {
        for (name, counts) in ARITY_SETS {
            assert!(builtin_does_nothing(name), "{name} declines before arity is read");
            let facts = steins_catalog::param_facts(name)
                .unwrap_or_else(|| panic!("{name} has no param facts"));
            assert!(facts.variadic.is_empty(), "{name} is variadic; an interval reads it right");
            let interval = facts.params_required..=facts.params.len();
            assert!(counts.iter().all(|c| interval.contains(c)), "{name}: {counts:?} outside {interval:?}");
            assert!(counts.len() < interval.clone().count(), "{name}: the set is the whole interval");
        }
    }

    #[test]
    fn exact_type_admission_is_strict_mode_acceptance() {
        let s = ArgValue::Str(PhpStr::from("x"));
        assert!(literal_exactly_of("string", &s));
        assert!(literal_exactly_of("array|string", &s));
        assert!(literal_exactly_of("mixed", &s));
        assert!(!literal_exactly_of("int", &s), "a numeric-looking string still coerces");
        assert!(!literal_exactly_of("callable", &s), "a string names user code at a callable");
        assert!(literal_exactly_of("float", &ArgValue::Int(1)), "the one strict widening");
        assert!(!literal_exactly_of("int", &ArgValue::Float(1.5)), "a fraction into int deprecates");
        assert!(!literal_exactly_of("string", &ArgValue::Null), "null into non-nullable deprecates");
        assert!(literal_exactly_of("?string", &ArgValue::Null));
        assert!(literal_exactly_of("string|null", &ArgValue::Null));
        assert!(literal_exactly_of("Countable|array", &ArgValue::Array(Vec::new())));
        assert!(!literal_exactly_of("Countable", &ArgValue::Array(Vec::new())));
        assert!(!literal_exactly_of("mixed", &ArgValue::Array(Vec::new())), "an array at mixed may render");
        assert!(literal_exactly_of("RoundingMode|int", &ArgValue::Int(1)));
        assert!(!literal_exactly_of("string", &ArgValue::Bool(true)), "bool into string coerces");
        assert!(literal_exactly_of("false", &ArgValue::Bool(false)));
        assert!(!literal_exactly_of("false", &ArgValue::Bool(true)));
    }
}
