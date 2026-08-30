//! Type-predicate guard vocabulary (ADR-0064 seam (v)): the `is_*` predicates
//! with both polarities pinned per predicate, `in_array` literals, and the fact
//! refinement each predicate implies.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Base, Certainty, Fact, Refinement, StrPreds, Val, php_is_numeric};
use steins_syntax::{ArgValue, CallExpr, CondExpr};

use crate::cx::Cx;
use crate::env::{Known, Store, Stratum, val_of};
use crate::existence::global_function_callee;
use crate::refine::{
    add_str_preds, arms_refute, clear_null, exclude_member, leave_empty_domain, refine_fact,
};
use crate::shapes::mint_collapsed_shape;

// ---------------------------------------------------------------------------
// Type-predicate guard vocabulary (ADR-0064 seam (v)).
//
// PHPStan ships this family as a `FunctionTypeSpecifyingExtension` set; Steins
// imports it into the existing narrowing machinery (ADR-0052) rather than a new
// extension mechanism — the arm lane subtracts, the value-fact lane refines, and
// `assert(is_string($x))` inherits both for free.
//
// Both polarities are pinned per predicate, each a different question. For arm
// `M` and predicate `P`, `pred_holds_on_arm` answers "does *every* value `M`
// admits satisfy `P`?": the TRUE branch deletes `M` iff `No`; the FALSE branch
// deletes `M` iff `Yes`. `Maybe` keeps the arm on both branches (ADR-0052 §2).
//
// `ctype_*` is deliberately NOT here: locale- and byte-sensitive (and, before
// PHP 8.1, silently reinterpreted int arguments as byte values), so it needs its
// own measured slice; DR2 declines rather than guessing.
// ---------------------------------------------------------------------------

/// One recognized `is_*` type predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypePred {
    /// `is_string`
    Str,
    /// `is_int` / `is_integer` / `is_long`
    Int,
    /// `is_float` / `is_double`
    Float,
    /// `is_bool`
    Bool,
    /// `is_array`
    Array,
    /// `is_null`
    Null,
    /// `is_object`
    Object,
    /// `is_scalar`
    Scalar,
    /// `is_numeric`
    Numeric,
    /// `is_callable`
    Callable,
    /// `is_iterable`
    Iterable,
}

/// A value's PHP runtime type class — what `gettype()` reports, the only thing
/// the `is_*` family actually tests. Distinct from the contract crate's
/// acceptance relation: `admits_val(Base(Float), Int(5))` is `Yes` (PHPStan's
/// "float accepts int" rule), but `is_float(5)` is `false` since PHP widens at
/// the boundary. Reads declared arms as runtime types, as PHPStan's own type
/// specifier does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtKind {
    Null,
    Bool,
    Int,
    Float,
    String,
    Array,
    Object,
    /// `gettype()` reports `resource` (or `resource (closed)` — one kind here,
    /// since no `is_*` predicate separates them). No [`Val`] maps here: exists
    /// for the [`ContractTy::Resource`] arm alone (ADR-0056 §8), which is why
    /// every predicate below rejects it.
    Resource,
}

/// The exhaustive set of runtime kinds an arm's values can have, or `None` when
/// the arm spans an unknown set (`mixed`, its cuts, `Opaque`, `never`). Unknown
/// keeps the arm on both polarities.
fn arm_rt_kinds(arm: &ContractTy) -> Option<&'static [RtKind]> {
    use ContractTy as C;
    use RtKind::{Array, Bool, Float, Int, Null, Object, Resource, String as Str};
    Some(match arm {
        C::Null => &[Null],
        C::Base(Base::Int) | C::IntIn(_) | C::LitInt(_) => &[Int],
        C::Base(Base::Float) | C::LitFloat(_) => &[Float],
        // `class-string`/`literal-string`/`callable-string` are strings at
        // runtime — their non-extensionality (ADR-0038) is about *which*
        // strings, never about being one.
        C::Base(Base::String) | C::StrWith(_) | C::LitStr(_) | C::StrOpaque => &[Str],
        C::Base(Base::Bool) | C::LitBool(_) => &[Bool],
        C::ArrayAny { .. } | C::ListOf { .. } | C::MapOf { .. } | C::Shape { .. } => &[Array],
        // An enum case is an object at runtime — `gettype(Suit::Hearts)` is
        // `"object"`, and `is_object` is the only predicate that separates it.
        C::Class(_) | C::EnumCase { .. } | C::ObjectAny => &[Object],
        C::Resource => &[Resource],
        // `iterable` is `array|Traversable`; `callable` is a callable-string, a
        // `[obj, 'm']`/`['C', 'm']` pair-array, a Closure or an `__invoke`able.
        C::IterableOf { .. } => &[Array, Object],
        C::CallableTy { .. } => &[Str, Array, Object],
        // `Unset` is unreachable here — [`flatten_arms`] drops it before any arm
        // list exists (ADR-0087) — and answers `None` for the same reason the
        // floors below do: an arm spanning no known runtime kind must survive
        // both polarities rather than be narrowed away.
        C::Mixed
        | C::MixedMinus(_)
        | C::Opaque
        | C::Unset
        | C::Never
        | C::Union(_)
        | C::Inter(_) => {
            return None;
        }
    })
}

/// `(kinds the predicate definitely accepts, kinds it definitely rejects)`. A kind
/// in neither set is undecidable for that predicate (`is_callable` on a string,
/// array, or object; `is_iterable` on an object).
///
/// Every predicate here rejects [`RtKind::Resource`]: PHP's `is_*` family answers
/// `false` for a resource across the board, `is_scalar`/`is_callable`/
/// `is_iterable` included (probed at 8.5.9). `is_resource` itself would answer
/// `true` and is deliberately not a [`TypePred`] yet (ADR-0056 §8 deferral) —
/// it needs the positive branch to bind a resource fact, a producer question.
fn pred_kind_sets(pred: TypePred) -> (&'static [RtKind], &'static [RtKind]) {
    use RtKind::{Array, Bool, Float, Int, Null, Object, Resource, String as Str};
    match pred {
        TypePred::Str => (&[Str], &[Null, Bool, Int, Float, Array, Object, Resource]),
        TypePred::Int => (&[Int], &[Null, Bool, Float, Str, Array, Object, Resource]),
        TypePred::Float => (&[Float], &[Null, Bool, Int, Str, Array, Object, Resource]),
        TypePred::Bool => (&[Bool], &[Null, Int, Float, Str, Array, Object, Resource]),
        TypePred::Array => (&[Array], &[Null, Bool, Int, Float, Str, Object, Resource]),
        TypePred::Null => (&[Null], &[Bool, Int, Float, Str, Array, Object, Resource]),
        TypePred::Object => (&[Object], &[Null, Bool, Int, Float, Str, Array, Resource]),
        // `is_scalar(null)` and `is_scalar([])` are both false — PHP's "scalar"
        // is exactly int|float|string|bool.
        TypePred::Scalar => (&[Bool, Int, Float, Str], &[Null, Array, Object, Resource]),
        // `is_iterable` is `is_array($x) || $x instanceof Traversable`; an object
        // arm is therefore undecided without the is-a oracle, and stays `Maybe`.
        TypePred::Iterable => (&[Array], &[Null, Bool, Int, Float, Str, Resource]),
        // `is_callable` accepts no *kind* outright (a string may name a function,
        // an array may be a `[obj, 'm']` pair, an object may be `__invoke`able),
        // and rejects the four kinds that can never be callable.
        TypePred::Callable => (&[], &[Null, Bool, Int, Float, Resource]),
        // `is_numeric(true)` is FALSE — bools are not numeric. The string kind is
        // decided by the arm's own predicate set, not by its kind, so it appears
        // in neither list here (see `pred_holds_on_arm`).
        TypePred::Numeric => (&[Int, Float], &[Null, Bool, Array, Object, Resource]),
    }
}

/// The [`Certainty`] that **every** value `arm` admits satisfies `pred`.
///
/// `Yes` licenses the FALSE branch to delete the arm, `No` licenses the TRUE
/// branch to; `Maybe` keeps it on both (ADR-0052 §2).
fn pred_holds_on_arm(pred: TypePred, arm: &ContractTy) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    // A union answers only where every member agrees; an intersection is a subset
    // of each member, so one deciding member decides it.
    match arm {
        ContractTy::Union(members) if !members.is_empty() => {
            let mut it = members.iter().map(|m| pred_holds_on_arm(pred, m));
            let first = it.next().expect("non-empty checked");
            return if it.all(|c| c == first) { first } else { Maybe };
        }
        ContractTy::Inter(members) => {
            if members.iter().any(|m| pred_holds_on_arm(pred, m).is_yes()) {
                return Yes;
            }
            if members.iter().any(|m| pred_holds_on_arm(pred, m) == No) {
                return No;
            }
            return Maybe;
        }
        // The two arms whose whole meaning IS a predicate's answer.
        ContractTy::CallableTy { .. } if pred == TypePred::Callable => return Yes,
        ContractTy::IterableOf { .. } if pred == TypePred::Iterable => return Yes,
        // `is_numeric` on a string arm is decided by the arm's predicate set, not
        // by its runtime kind: `numeric-string` proves it, a numeric literal
        // proves it, a non-numeric literal refutes it, and a bare `string` (or a
        // non-extensional `class-string`) answers nothing.
        ContractTy::LitStr(s) if pred == TypePred::Numeric => {
            return Certainty::from_bool(php_is_numeric(s));
        }
        ContractTy::StrWith(p) if pred == TypePred::Numeric => {
            return if p.contains_all(StrPreds::NUMERIC) { Yes } else { Maybe };
        }
        _ => {}
    }
    let Some(kinds) = arm_rt_kinds(arm) else { return Maybe };
    let (sat, unsat) = pred_kind_sets(pred);
    if !kinds.is_empty() && kinds.iter().all(|k| sat.contains(k)) {
        return Yes;
    }
    if !kinds.is_empty() && kinds.iter().all(|k| unsat.contains(k)) {
        return No;
    }
    Maybe
}

/// The [`Certainty`] that a concrete value satisfies `pred` — the finite-layer
/// twin of [`pred_holds_on_arm`], and exact for every predicate except the two
/// whose answer depends on data the domain does not carry (`is_callable` on a
/// string that may name a function or on a `[obj, 'm']` pair).
fn pred_holds_on_val(pred: TypePred, v: &Val) -> Certainty {
    use Certainty::{Maybe, No, Yes};
    if pred == TypePred::Numeric {
        return match v {
            Val::Int(_) | Val::Float(_) => Yes,
            Val::Str(s) => Certainty::from_bool(php_is_numeric(s)),
            Val::Bool(_) | Val::Null | Val::Array(_) => No,
        };
    }
    if pred == TypePred::Callable {
        return match v {
            // A literal string may name a function; a literal array may be a
            // `['C', 'm']` pair. Undecidable here, so it filters nothing.
            Val::Str(_) | Val::Array(_) => Maybe,
            Val::Int(_) | Val::Float(_) | Val::Bool(_) | Val::Null => No,
        };
    }
    let kind = match v {
        Val::Null => RtKind::Null,
        Val::Bool(_) => RtKind::Bool,
        Val::Int(_) => RtKind::Int,
        Val::Float(_) => RtKind::Float,
        Val::Str(_) => RtKind::String,
        Val::Array(_) => RtKind::Array,
    };
    let (sat, unsat) = pred_kind_sets(pred);
    if sat.contains(&kind) {
        Yes
    } else if unsat.contains(&kind) {
        No
    } else {
        Maybe
    }
}

/// The [`Certainty`] that **every** value the fact admits satisfies `pred` — the
/// value-lane twin of [`pred_holds_on_arm`]. A `nullable` abstract fact carries the
/// null kind alongside its base, since `is_string(null)` is false.
pub(crate) fn pred_holds_on_fact(pred: TypePred, f: &Fact) -> Certainty {
    use Certainty::{Maybe, Yes};
    if let Some(members) = f.finite_members() {
        return Certainty::all_of(members.iter().map(|v| pred_holds_on_val(pred, v)));
    }
    let (kind, nullable) = match f {
        Fact::Refined { base, nullable, .. } | Fact::General { base, nullable } => {
            let k = match base {
                Base::Int => RtKind::Int,
                Base::Float => RtKind::Float,
                Base::String => RtKind::String,
                Base::Bool => RtKind::Bool,
            };
            (k, *nullable)
        }
        Fact::Shape { nullable, .. } => (RtKind::Array, *nullable),
        // A union spans several runtime kinds at once, so no single-kind
        // predicate is decided by it — the honest floor, and the same one a
        // mixed `OneOf` takes.
        Fact::Union { .. } => return Maybe,
        // Finite layers are handled above.
        Fact::Singleton(_) | Fact::OneOf(_) => return Maybe,
    };
    // The one refinement that decides a predicate the base alone cannot: a string
    // fact already carrying `NUMERIC` is proven numeric.
    if pred == TypePred::Numeric
        && !nullable
        && let Fact::Refined { base: Base::String, refinement: Refinement::Str(p), .. } = f
        && p.contains_all(StrPreds::NUMERIC)
    {
        return Yes;
    }
    let (sat, unsat) = pred_kind_sets(pred);
    let kinds: &[RtKind] = if nullable { &[kind, RtKind::Null] } else { &[kind] };
    if kinds.iter().all(|k| sat.contains(k)) {
        return Yes;
    }
    if kinds.iter().all(|k| unsat.contains(k)) {
        return Certainty::No;
    }
    Maybe
}

/// The scalar [`Base`] a predicate *proves* on its true branch, for the four
/// predicates that name exactly one. The rest prove a base the four-layer domain
/// cannot spell alone (`is_scalar`/`is_numeric` name a union of bases, `is_array`
/// the array stratum, `is_object`/`is_callable`/`is_iterable` nothing the domain
/// represents at all).
fn pred_base(pred: TypePred) -> Option<Base> {
    match pred {
        TypePred::Str => Some(Base::String),
        TypePred::Int => Some(Base::Int),
        TypePred::Float => Some(Base::Float),
        TypePred::Bool => Some(Base::Bool),
        _ => None,
    }
}

/// The recognized type predicate a guard call names, or `None` for a call that
/// does not denote the global builtin — the SAME [`global_function_callee`] every
/// other recognizer opens with (a `Foo\is_string` or a same-named user function is
/// a different function). Every member of the family takes exactly one by-value
/// argument.
pub(crate) fn type_predicate(cx: &Cx, call: &CallExpr) -> Option<TypePred> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only || call.args.len() != 1 {
        return None;
    }
    const PREDS: &[(&str, TypePred)] = &[
        ("is_string", TypePred::Str),
        ("is_int", TypePred::Int),
        ("is_integer", TypePred::Int),
        ("is_long", TypePred::Int),
        ("is_float", TypePred::Float),
        ("is_double", TypePred::Float),
        ("is_bool", TypePred::Bool),
        ("is_array", TypePred::Array),
        ("is_null", TypePred::Null),
        ("is_object", TypePred::Object),
        ("is_scalar", TypePred::Scalar),
        ("is_numeric", TypePred::Numeric),
        ("is_callable", TypePred::Callable),
        ("is_iterable", TypePred::Iterable),
    ];
    PREDS.iter().find(|(n, _)| callee.eq_ignore_ascii_case(n)).map(|(_, p)| *p)
}

/// The recognized **pure-question** builtin a call names, or `None`.
///
/// These answer a question ABOUT their arguments and write none of them: every
/// parameter is by value in PHP's own signature, the option flags included
/// (`is_a`'s `bool $allow_string`, `is_subclass_of`'s the same,
/// `class_exists`'s `bool $autoload`). An unrecognized name here cost the
/// subject every fact it had, which is the defect #536, #414, #569 and #571
/// each fixed for one family; issue #575 measured the rest of the population
/// against PHPStan's own type-specifying set, and this is it.
///
/// Recognition only. What these guards PROVE — `is_a` is `instanceof`'s
/// function spelling, `str_starts_with` with a non-empty needle proves a
/// non-empty string, `class_exists` proves a class-string — is separate work
/// and none of it is claimed here.
///
/// **Not on this list, deliberately.** `settype` takes `mixed &$var` and
/// `preg_match` takes `array &$matches`: they really do write, and the tests
/// pin that they still forget. `array_key_exists` is on the OTHER exemption —
/// its branch-confined forgetting is a decision (#548), not this defect.
///
/// The arity is deliberately not pinned: an optional argument changes what a
/// call ANSWERS, never whether it writes, and this predicate is only asked the
/// second question. `call.positional_only` still gates, since a named-argument
/// call is not a shape the recognizers read.
pub(crate) fn pure_question_builtin(cx: &Cx, call: &CallExpr) -> Option<&'static str> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only {
        return None;
    }
    const NAMES: &[&str] = &[
        // Class reflection (issue #569).
        "get_class",
        "get_parent_class",
        "get_debug_type",
        "gettype",
        "is_a",
        "is_subclass_of",
        "spl_object_id",
        "spl_object_hash",
        // String questions (issue #575). Already by-value certified for the
        // statement position; guard position is what was missing.
        "str_contains",
        "str_starts_with",
        "str_ends_with",
        "strlen",
        "mb_strlen",
        // The `ctype_*` family: one `mixed $text` parameter, by value.
        "ctype_alnum",
        "ctype_alpha",
        "ctype_cntrl",
        "ctype_digit",
        "ctype_graph",
        "ctype_lower",
        "ctype_print",
        "ctype_punct",
        "ctype_space",
        "ctype_upper",
        "ctype_xdigit",
        // Existence questions: a name and an option flag, both by value.
        // `class_exists` autoloads, which is an EFFECT and not an argument
        // write — the effect lane owns that question and keeps it.
        "class_exists",
        "interface_exists",
        "enum_exists",
        "trait_exists",
        "function_exists",
        "method_exists",
        "property_exists",
        "defined",
        // Haystack searches: needle, haystack and the strict flag, all by
        // value. `in_array`'s literal-haystack form has its own recognizer for
        // what it PROVES; this is only about what it does not write.
        "in_array",
        "array_search",
    ];
    NAMES.iter().copied().find(|n| callee.eq_ignore_ascii_case(n))
}

/// One type-vocabulary guard, resolved to a variable and a branch polarity.
enum TypeGuard {
    /// `is_string($x)` and kin.
    Pred { var: String, pred: TypePred, positive: bool },
    /// `in_array($x, [<literals>], true)` — the strict-only literal-haystack form.
    InArray { var: String, lits: Vec<Val>, positive: bool },
    /// A guard that PROVES a string predicate of its subject on the branch where
    /// it holds (issue #575's second group). Positive-only by construction: what
    /// these prove is an existence, and the failure of an existence proves
    /// nothing about the subject.
    StrPred { var: String, preds: StrPreds },
}

/// The string predicate a guard PROVES of its subject on the branch where it
/// holds, or `None` (issue #575's second group).
///
/// Positive-only by construction for every member: what these prove is an
/// EXISTENCE, and the failure of an existence proves nothing about the subject.
///
/// # The substring three
///
/// `str_contains($s, 'x')`, `str_starts_with`, `str_ends_with`: a haystack that
/// contains a **non-empty** needle has at least that needle's length, so the
/// true branch proves `non-empty-string`.
///
/// The empty needle is the whole reason this reads the literal rather than
/// trusting the name. Measured on PHP 8.5.9 rather than assumed:
/// `str_contains("", "")` is **true**, and so are `str_starts_with("", "")` and
/// `str_ends_with("", "")` — an empty needle is found in the empty string, so
/// such a guard proves nothing. `str_contains("", "x")` is false, which is the
/// other half of the same measurement and the one the rule stands on. A needle
/// that is not a literal may be that empty string, and is declined for the same
/// reason.
///
/// # The existence questions
///
/// `class_exists($c)` proves `$c` is a **class-string**: measured at 8.5.9,
/// `class_exists('')` and `class_exists('0')` are both false, and a name the
/// engine resolves to a class-like is what [`StrPreds::CLASS_STRING`] denotes.
/// `interface_exists` / `enum_exists` / `trait_exists` prove the same predicate,
/// which covers class, interface, trait and enum together.
///
/// `function_exists($f)` and `defined($c)` prove only **non-empty**: both answer
/// false for `""` (measured), and a function or constant name is not a
/// class-string. Naming them here rather than omitting them is the point — the
/// weaker proof is a decision, not an oversight.
///
/// The option flag (`class_exists($c, false)`) changes what the call ANSWERS,
/// never what a true answer proves about the name, so the arity is not pinned.
fn string_proof_guard(cx: &Cx, call: &CallExpr) -> Option<(String, StrPreds)> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only || call.args.is_empty() {
        return None;
    }
    let ArgValue::Var(var) = &call.args[0].value else { return None };
    let named = |n: &str| callee.eq_ignore_ascii_case(n);

    if ["str_contains", "str_starts_with", "str_ends_with"].iter().any(|n| named(n)) {
        let [_, needle] = call.args.as_slice() else { return None };
        let ArgValue::Str(needle) = &needle.value else { return None };
        if needle.as_bytes().is_empty() {
            return None;
        }
        return Some((var.clone(), StrPreds::NON_EMPTY));
    }
    if ["class_exists", "interface_exists", "enum_exists", "trait_exists"].iter().any(|n| named(n))
    {
        return Some((var.clone(), StrPreds::CLASS_STRING));
    }
    if ["function_exists", "defined"].iter().any(|n| named(n)) {
        return Some((var.clone(), StrPreds::NON_EMPTY));
    }
    ctype_proof(callee).map(|preds| (var.clone(), preds))
}

/// What a `ctype_*` predicate proves of its subject when it answers true, or
/// `None` for a name outside the family.
///
/// **Every member proves non-empty**, and that is measured rather than read off
/// the family's name: at 8.5.9 not one of them answers true for `""`.
///
/// Three prove more, and the table below is the measurement they come from
/// (values the predicate accepted, out of `"" 0 0123 999 abc ABC ff " " "\t" 1.5 -1`):
///
/// ```text
/// ctype_digit    '0', '0123', '999'                             → numeric too
/// ctype_lower    'abc', 'ff'                                    → lowercase too
/// ctype_upper    'ABC'                                          → uppercase too
/// ctype_alpha    'abc', 'ABC', 'ff'
/// ctype_alnum    '0', '0123', '999', 'abc', 'ABC', 'ff'
/// ctype_xdigit   '0', '0123', '999', 'abc', 'ABC', 'ff'
/// ctype_space    ' ', '\t'
/// ctype_punct    (none of the sample)
/// ```
///
/// `ctype_digit` proving **numeric** is cross-checked rather than assumed: every
/// value it accepted is `is_numeric`, leading zeros included (`is_numeric('0123')`
/// is true).
///
/// The predicate it does **not** prove is worth naming: `ctype_digit('0')` is
/// true and `'0'` is falsy, so this is `NON_EMPTY` and never `NON_FALSY`. The
/// implication runs the other way (`NonFalsy ⇒ NonEmpty`), and claiming the
/// stronger one here would be a false claim about a real string.
///
/// `ctype_alpha`, `ctype_alnum`, `ctype_xdigit`, `ctype_space` and `ctype_punct`
/// each prove a character class the string vocabulary cannot spell, so
/// non-emptiness is all that survives the translation — the weaker answer is
/// what the vocabulary allows, not a gap in the measurement.
///
/// # The locale question, which is what the old decline was waiting on
///
/// The fixture that pinned this family as declined named the reason: these are
/// **locale-sensitive**. Measured across `C`, `en_US.UTF-8`, `de_DE.ISO-8859-1`
/// and `tr_TR.UTF-8`, and the answer splits:
///
/// * **`ctype_digit` does not move.** The Latin-1 superscript-two byte is
///   rejected under every locale tried, which is POSIX's rule rather than an
///   accident — the digit class contains 0-9 and nothing else, in every locale.
///   So the `numeric` claim is locale-stable.
/// * **`ctype_lower` and `ctype_upper` DO move**: under `en_US.UTF-8` the
///   Latin-1 e-acute byte counts as lowercase, and under `C` it does not.
///
/// The claims survive that movement anyway, and the reason is worth stating
/// because it is not the obvious one. [`StrPreds::LOWERCASE`] means "no ASCII
/// uppercase byte" (`strtolower` leaves it alone), and a locale can only ever
/// WIDEN which bytes count as lowercase. It cannot make an ASCII uppercase byte
/// count as lowercase, because POSIX requires the two classes to be disjoint —
/// so `ctype_lower` answering true still implies no `A`-`Z` is present, in every
/// locale. `ctype_upper` mirrors it.
///
/// The Turkish locale is the classic trap here and it is a trap for CONVERSION
/// (`strtolower('I')`), not for classification: `I` is uppercase and `i` is
/// lowercase under `tr_TR` as everywhere else, and these predicates classify.
fn ctype_proof(callee: &str) -> Option<StrPreds> {
    const FAMILY: &[(&str, Option<StrPreds>)] = &[
        ("ctype_digit", Some(StrPreds::NUMERIC)),
        ("ctype_lower", Some(StrPreds::LOWERCASE)),
        ("ctype_upper", Some(StrPreds::UPPERCASE)),
        ("ctype_alpha", None),
        ("ctype_alnum", None),
        ("ctype_xdigit", None),
        ("ctype_space", None),
        ("ctype_punct", None),
        ("ctype_cntrl", None),
        ("ctype_graph", None),
        ("ctype_print", None),
    ];
    FAMILY
        .iter()
        .find(|(n, _)| callee.eq_ignore_ascii_case(n))
        .map(|(_, extra)| match extra {
            Some(p) => StrPreds::NON_EMPTY.union(*p),
            None => StrPreds::NON_EMPTY,
        })
}

/// The `(needle var, haystack literals)` of a **strict** `in_array` over a literal
/// haystack, or `None`.
///
/// The non-strict form narrows NOTHING, deliberately: `in_array($x, ['a'], false)`
/// is PHP's loose `==`, whose equivalence classes are neither reflexive across
/// types nor transitive (`in_array(0, ['a'])` is true on PHP 7, `in_array('1e2',
/// ['100'])` is true, and PHP 8's string<->int change moved the boundary again).
/// No sound OneOf can be minted from a loose test, so the guard is declined. A
/// non-literal haystack is declined since its members are unknown.
pub(crate) fn in_array_literals(
    cx: &Cx,
    call: &CallExpr,
    php_minor: Option<(u16, u16)>,
) -> Option<(String, Vec<Val>)> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only || !callee.eq_ignore_ascii_case("in_array") {
        return None;
    }
    // The third argument must be a literal `true`: strict is what makes the
    // membership an *identity*, and identity is what a `OneOf` means.
    if call.args.len() != 3 || !matches!(call.args[2].value, ArgValue::Bool(true)) {
        return None;
    }
    let ArgValue::Var(var) = &call.args[0].value else { return None };
    let ArgValue::Array(_) = &call.args[1].value else { return None };
    let Some(Val::Array(entries)) = val_of(&call.args[1].value, php_minor) else { return None };
    // A nested array member has no scalar identity the value domain can carry.
    if entries.iter().any(|(_, v)| matches!(v, Val::Array(_))) {
        return None;
    }
    let lits: Vec<Val> = entries.into_iter().map(|(_, v)| v).collect();
    if lits.is_empty() {
        return None;
    }
    Some((var.clone(), lits))
}

/// Collect the type-vocabulary guards a condition establishes at polarity `then`.
/// The polarity walk is [`collect_refine`]'s, verbatim in structure: `Not` flips,
/// `And` contributes on the true path, `Or` on the false one (De Morgan) — so
/// `if ($a && is_string($s))` reaches its narrowing point.
///
/// [`collect_refine`]: crate::refine::collect_refine
fn collect_type_guards(cx: &Cx, cond: &CondExpr, then: bool, out: &mut Vec<TypeGuard>) {
    match cond {
        CondExpr::Call { call, .. } => {
            // A guard that PROVES a string predicate of its subject, on the
            // branch where it holds and nothing on the other (issue #575).
            if then && let Some((var, preds)) = string_proof_guard(cx, call) {
                out.push(TypeGuard::StrPred { var, preds });
            }
            if let Some(pred) = type_predicate(cx, call) {
                if let ArgValue::Var(var) = &call.args[0].value {
                    out.push(TypeGuard::Pred { var: var.clone(), pred, positive: then });
                }
                return;
            }
            if let Some((var, lits)) = in_array_literals(cx, call, cx.php_minor) {
                out.push(TypeGuard::InArray { var, lits, positive: then });
            }
        }
        CondExpr::Not(c) => collect_type_guards(cx, c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_type_guards(cx, a, then, out);
            collect_type_guards(cx, b, then, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_type_guards(cx, a, then, out);
            collect_type_guards(cx, b, then, out);
        }
        _ => {}
    }
}

/// **Apply every type-vocabulary guard of `cond` at polarity `then`** to a branch's
/// cloned env and store (ADR-0064 seam (v)). Runs beside [`apply_refinements`] /
/// [`apply_class_narrowing`] / [`apply_shape_narrowing`] in the branch walk, and on
/// the fall-through of `assert($expr)`.
///
/// [`apply_refinements`]: crate::refine::apply_refinements
/// [`apply_class_narrowing`]: crate::refine::apply_class_narrowing
/// [`apply_shape_narrowing`]: crate::shapes::apply_shape_narrowing
pub(crate) fn apply_type_narrowing(
    cx: &Cx,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let mut guards = Vec::new();
    collect_type_guards(cx, cond, then, &mut guards);
    for g in &guards {
        match g {
            TypeGuard::Pred { var, pred, positive } => {
                // Asked BEFORE the subtraction, which is what erases the evidence:
                // an arm lane the guards above left holding nothing this predicate
                // can prove refutes the positive side outright (issue #445), and
                // `subtract_pred_arms` is about to empty and drop that same lane.
                let refuted = *positive && arms_refute_pred(store, var, *pred);
                subtract_pred_arms(store, var, *pred, *positive);
                // An arm lane collapsed to a single array arm mints its shape fact
                // through the same gated helper `is_array`'s S4 siblings use.
                mint_collapsed_shape(var, env, store);
                if refuted {
                    // The intersection is empty, so the empty domain is the answer.
                    // Left to itself the mint below would seed the predicate's own
                    // base over a binding with no fact — `string` under an
                    // `is_string` the chain above had already ruled out.
                    leave_empty_domain(env, store, var);
                } else {
                    refine_fact_for_pred(env, var, *pred, *positive);
                }
                // A value the guard proved is not an object cannot still carry a
                // heap binding or is-a bound; the declared-arm lane stays.
                if *positive && !pred_kind_sets(*pred).0.contains(&RtKind::Object) {
                    store.refs.remove(var);
                    store.members.remove(var);
                }
            }
            TypeGuard::StrPred { var, preds } => {
                // A proof, not a subtraction: the guard adds what it established
                // and takes nothing away, so the arm lane is untouched and only
                // the value lane moves. `add_str_preds` closes under implication,
                // so a consumer asking the weaker predicate sees it too.
                refine_fact(env, var, Stratum::Verified, |f| Some(add_str_preds(f, *preds)));
            }
            TypeGuard::InArray { var, lits, positive } => {
                // The same composition question, one vocabulary over (issue #445):
                // a membership test proves the needle is one of the haystack
                // literals, and a lane holding none of them refutes every one.
                if *positive
                    && arms_refute(store, var, |arm| {
                        lits.iter()
                            .all(|v| steins_contract::admits_val(arm, v) == Certainty::No)
                    })
                {
                    leave_empty_domain(env, store, var);
                } else {
                    refine_fact_for_in_array(env, var, lits, *positive);
                }
            }
        }
    }
}

/// Arm-lane subtraction for one type predicate: the TRUE branch deletes the arms
/// the predicate **refutes**, the FALSE branch the arms it **proves**. `Maybe`
/// keeps the arm on both. Marks [`Store::narrowed`] when an arm actually died
/// (issue #428).
///
/// An emptied lane takes [`subtract_contract_lane`]'s two endings, not a third of
/// its own (issue #432, closing the residue ADR-0052's 2026-08-19 note recorded):
/// **kept, empty** when every arm it held was `Verified`, so
/// [`Store::contract_emptied`] can read it, and dropped to no-fact otherwise. The
/// soundness argument is the one that function gives, and it does not turn on the
/// subtrahend's shape: `!is_string` deletes a `string` arm because the predicate
/// provably holds on every value that arm admits, exactly as `!== 1` deletes a
/// `1` arm, so a `Verified` lane every one of whose arms a native runtime test
/// deleted is the statement that no value reaches here. Leaving the predicate
/// vocabulary alone made an exhausted native union — `string|int` under
/// `is_string`/`is_int`, ADR-0088 §1's own idiom — indistinguishable from a
/// variable that never had a lane, which is the *absence* answer and the opposite
/// claim.
///
/// Still not a death signal: no branch is pruned, and what the empty lane buys a
/// consumer is silence.
///
/// [`subtract_contract_lane`]: crate::refine::subtract_contract_lane
fn subtract_pred_arms(store: &mut Store, var: &str, pred: TypePred, positive: bool) {
    let Some(arms) = store.contract.get_mut(var) else { return };
    let before = arms.len();
    // Asked before the retain, which is what erases the evidence.
    let all_verified = arms.iter().all(|a| a.stratum == Stratum::Verified);
    arms.retain(|a| {
        let holds = pred_holds_on_arm(pred, &a.ty);
        if positive { holds != Certainty::No } else { !holds.is_yes() }
    });
    let narrowed = arms.len() != before;
    if arms.is_empty() && !all_verified {
        store.contract.remove(var);
    }
    if narrowed {
        store.narrowed.insert(var.to_owned());
    }
}

/// **Does the arm lane already refute this predicate?** (issue #445) — the
/// [`refinement_refuted`] sibling for the type-predicate vocabulary, where the
/// positive side's claim is a base rather than a value.
///
/// True iff the declared lane is present and every arm the guards above left is
/// one the predicate provably does not hold on ([`pred_holds_on_arm`] answering
/// `No`); `Maybe` keeps the arm and so keeps the guard. Must be asked before
/// [`subtract_pred_arms`] runs, since that is the call which empties the lane and
/// drops it.
///
/// [`refinement_refuted`]: crate::refine::refinement_refuted
fn arms_refute_pred(store: &Store, var: &str, pred: TypePred) -> bool {
    arms_refute(store, var, |arm| pred_holds_on_arm(pred, arm) == Certainty::No)
}

/// Value-fact narrowing for one type predicate.
///
/// `None` means "leave the fact exactly as it was", also the answer when the
/// guard's polarity refutes the whole fact — a binding proven `int` under an
/// `is_string` true-branch means the branch is unreachable, and this helper does
/// not own death (ADR-0052 §2: the verdict does). Rewriting there would mint a
/// claim about a path the runtime never takes.
fn refine_fact_for_pred(
    env: &mut HashMap<String, Known>,
    var: &str,
    pred: TypePred,
    positive: bool,
) {
    // `is_string`/`is_int`/`is_float`/`is_bool` prove a single base, and `is_null`
    // a single value, so the true branch can state it outright over a
    // mixed/undeclared binding. Every other predicate names a union of bases
    // (`is_scalar`, `is_numeric`), the array stratum (served by
    // `mint_collapsed_shape` instead), or nothing the domain represents.
    if env.get(var).is_none_or(|k| k.fact.is_none()) {
        if !positive {
            return;
        }
        let minted = match pred {
            TypePred::Null => Some(Fact::Singleton(Val::Null)),
            _ => pred_base(pred).map(|base| Fact::General { base, nullable: false }),
        };
        if let Some(fact) = minted {
            // A closure-only binding carries no scalar fact by construction
            // (ADR-0033); minting one over it would forget the closure target.
            if env.get(var).is_some_and(|k| k.closure.is_some()) {
                return;
            }
            let line = env.get(var).map_or(0, |k| k.line);
            env.insert(
                var.to_owned(),
                Known::value_strat(fact, line, Some("proven on this branch".to_owned()), Stratum::Verified),
            );
        }
        return;
    }
    refine_fact(env, var, Stratum::Verified, |f| {
        // The guard's polarity refutes the binding's own fact => DROP it. The
        // branch is unreachable, and this slice does not own death (ADR-0052 §2).
        // Carrying the refuted fact would premise proof-layer findings about a
        // path the runtime never takes (measured FP class: `new Identifier($name)`
        // inside `if (is_string($name))` under a descent that bound `$name` to an
        // int). Dropping to no-fact is the FP-safe fallback.
        let holds = pred_holds_on_fact(pred, f);
        if (positive && holds == Certainty::No) || (!positive && holds.is_yes()) {
            return None;
        }
        // The finite layers narrow by exact member retention on both polarities —
        // the only lossless subtraction the domain has. The refutation test above
        // already caught the all-members-refuted case.
        if let Some(members) = f.finite_members() {
            let kept: Vec<Val> = members
                .iter()
                .filter(|v| {
                    let h = pred_holds_on_val(pred, v);
                    if positive { h != Certainty::No } else { !h.is_yes() }
                })
                .cloned()
                .collect();
            return Fact::from_vals(kept);
        }
        if !positive {
            // The abstract layers carry no negative-predicate vocabulary (ADR-0052
            // §2). The exception is the nullable bit, the complement of `is_null`
            // — the same channel `!== null` uses.
            return if pred == TypePred::Null { clear_null(f) } else { Some(f.clone()) };
        }
        Some(match (pred, f) {
            // `is_null($x)` true: the value IS null, whatever the fact said.
            (TypePred::Null, _) => Fact::Singleton(Val::Null),
            // A base-naming predicate on a matching base keeps every refinement it
            // had and drops nullability (`is_string(null)` is false).
            (_, Fact::Refined { base, refinement, .. }) if pred_base(pred) == Some(*base) => {
                Fact::refined(*base, *refinement, false)
            }
            (_, Fact::General { base, .. }) if pred_base(pred) == Some(*base) => {
                Fact::General { base: *base, nullable: false }
            }
            // `is_numeric` wires the already-modeled `StrPreds::NUMERIC` (ADR-0064
            // §1): on a string-based fact the true branch intersects the
            // numeric-string class in. On int/float it only drops nullability.
            (
                TypePred::Numeric,
                Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. },
            ) => add_str_preds(&clear_null(f)?, StrPreds::NUMERIC),
            (
                TypePred::Numeric,
                Fact::Refined { base: Base::Int | Base::Float, .. }
                | Fact::General { base: Base::Int | Base::Float, .. },
            ) => clear_null(f)?,
            // `is_scalar` names int|float|string|bool: on any scalar-based fact it
            // proves only non-nullness, which is exactly what it drops.
            (TypePred::Scalar, Fact::Refined { .. } | Fact::General { .. }) => clear_null(f)?,
            // `is_array` on an array-stratum fact proves non-nullness too.
            (TypePred::Array, Fact::Shape { .. }) => clear_null(f)?,
            // Everything else: either the predicate refutes this fact (an
            // unreachable branch — see the doc comment) or it says nothing the
            // domain can hold.
            (_, other) => other.clone(),
        })
    });
}

/// Value-fact narrowing for the strict literal-haystack `in_array` form.
///
/// TRUE branch: the needle is identical to one of the haystack literals, so the
/// fact becomes the `OneOf` of those literals intersected with what was already
/// known (`Fact::from_vals` re-canonicalizes). FALSE branch: subtraction is exact
/// only on a finite fact (`OneOf` minus the literals via `exclude_member`); an
/// abstract fact has no point-complement, so it's left alone. Either polarity
/// emptying the set means the branch is unreachable — the fact is left
/// untouched, since the verdict owns death (ADR-0052 §2).
fn refine_fact_for_in_array(
    env: &mut HashMap<String, Known>,
    var: &str,
    lits: &[Val],
    positive: bool,
) {
    if env.get(var).is_none_or(|k| k.fact.is_none()) {
        if !positive || env.get(var).is_some_and(|k| k.closure.is_some()) {
            return;
        }
        let Some(fact) = Fact::from_vals(lits.to_vec()) else { return };
        let line = env.get(var).map_or(0, |k| k.line);
        env.insert(
            var.to_owned(),
            Known::value_strat(fact, line, Some("proven on this branch".to_owned()), Stratum::Verified),
        );
        return;
    }
    refine_fact(env, var, Stratum::Verified, |f| {
        if positive {
            // No admitted literal ⇒ the membership test cannot hold ⇒ the branch is
            // unreachable, and the fact drops rather than being carried into it (the
            // same rule `refine_fact_for_pred` states at length).
            let kept: Vec<Val> = lits.iter().filter(|v| f.admits(v)).cloned().collect();
            return Fact::from_vals(kept);
        }
        if f.finite_members().is_none() {
            return Some(f.clone());
        }
        let mut cur = f.clone();
        for v in lits {
            match exclude_member(&cur, v) {
                Some(next) => cur = next,
                // Emptied: every member was in the haystack, so the false branch is
                // unreachable — drop, do not carry.
                None => return None,
            }
        }
        Some(cur)
    });
}
