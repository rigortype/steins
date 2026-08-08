//! Plain-text spelling of a summarized contract-arm list (ADR-0053 §7 / ADR-0052
//! §4).
//!
//! [`normalize::summarize_vals`](crate::normalize::summarize_vals) produces the
//! *semantic* normal form of a proven value set — a sorted, deduped, precision-
//! collapsed arm list. This module turns that arm list into a **terminal-safe**
//! phpdoc-grammar type string (`int|numeric-string|null`, `'GET'|'POST'`, …). It is
//! the one shared spelling of contract arms, consumed by:
//!
//! * the `annotate` / dump emitters in `steins-infer` (which cannot reach the
//!   docblock renderer in `steins-edit` — the dependency runs
//!   `steins-edit → steins-infer`, ADR-0053 §7); and
//! * `steins-edit`'s docblock renderer (`render_value_domain`), which re-layers
//!   its docblock **armor** on top: the `*/`/raw-newline literal-safety widening
//!   that is meaningless in terminal output but corrupts a `/** … */` block. That
//!   armor pre-widens the arm list, then delegates the member assembly, the CAP-
//!   bounded literal-union decision, the predicate-keyword ladder, and the
//!   single-quote escaping to [`spell_arms`] here.
//!
//! The cut is byte-identical against the honesty tests in `steins-edit` (the
//! renderer's oracle) and the cross-crate parity test there.

use steins_domain::{Base, Certainty, IntRange, Key, PhpStr, StrPreds, Val, CAP};

use crate::{is_array_key_ty, shape_is_list, CallableObl, CField, CKey, ContractTy, MixedCut};

/// Spell a summarized contract-arm list as a terminal-safe phpdoc type, or `None`
/// when no faithful scalar spelling exists (an array/object/otherwise-unmodeled
/// arm — the honest `type-not-renderable` refusal, matching
/// [`summarize_vals`](crate::normalize::summarize_vals)'s own `None`).
///
/// `arms` is expected in the canonical order
/// [`summarize_vals`](crate::normalize::summarize_vals) produces (int, float,
/// string(s), bool, then `null`); the members are emitted in that order and joined
/// with `|`. String literals ([`ContractTy::LitStr`]) are spelled as a
/// single-quoted literal (one value) or a small literal union (≤ [`CAP`] distinct
/// values), and widen to the tightest refined-string keyword above that — the
/// CAP-bounded ladder. A predicate class ([`ContractTy::StrWith`], the numeric-
/// string collapse) spells its keyword directly.
///
/// Unlike the docblock renderer, this never widens on the `*/`/newline hazard:
/// terminal output has no `/** … */` to corrupt, so a `*/`-bearing literal is
/// spelled as its (escaped) literal here. A caller that needs docblock-safe output
/// applies that armor to the arm list *before* calling this (see
/// `steins_edit::common::render_value_domain`).
#[must_use]
pub fn spell_arms(arms: &[ContractTy]) -> Option<String> {
    let mut has_int = false;
    let mut has_float = false;
    let mut bool_member: Option<&'static str> = None;
    let mut nullable = false;
    // Int-flavored refinement/literal arms a lowered phpdoc envelope carries but a
    // summarized value set never does (`positive-int`, `int<1, 5>`, `5`). The
    // value-domain callers reach [`spell_arms`] only through `summarize_vals`, whose
    // int members collapse to `Base(Int)`, so these buckets stay empty there and
    // carry only the contract-arm dump surface (ADR-0052 §9).
    let mut int_ranges: Vec<String> = Vec::new();
    let mut int_lits: Vec<i64> = Vec::new();
    let mut float_lits: Vec<f64> = Vec::new();
    // The string portion: a summarized set hands us either the numeric-string class
    // (one `StrWith` arm) or the distinct-sorted literal arms — never both.
    let mut string_keyword: Option<String> = None;
    let mut string_lits: Vec<&PhpStr> = Vec::new();
    // Array-vocabulary arms (ADR-0062 §6): spelled in encounter order and
    // appended after the scalar members, by the one speller (not a second
    // renderer). An array arm never blocks the scalar members around it (a
    // `mixed`-ish union of `int|array{…}` still spells both sides), unlike the
    // catch-all refusal below, which is reserved for arms with no faithful
    // spelling at all (`object`, a class, `callable`, …).
    let mut array_members: Vec<String> = Vec::new();
    for arm in arms {
        match arm {
            ContractTy::Base(Base::Int) => has_int = true,
            ContractTy::Base(Base::Float) => has_float = true,
            ContractTy::Base(Base::Bool) => bool_member = Some("bool"),
            ContractTy::LitBool(true) => bool_member = Some("true"),
            ContractTy::LitBool(false) => bool_member = Some("false"),
            ContractTy::Null => nullable = true,
            ContractTy::StrWith(p) => string_keyword = Some(preds_keyword(*p)),
            // A declared `A&B` over string refinements is one predicate set
            // (issue #240) — folded by the ONE fold, then spelled by the ONE
            // ladder, so a conjunction reaches the same keyword the equivalent
            // computed set would. Any other intersection keeps the refusal below.
            ContractTy::Inter(members) => {
                string_keyword = Some(preds_keyword(crate::inter_str_preds(members)?));
            }
            ContractTy::Base(Base::String) => string_keyword = Some("string".to_owned()),
            ContractTy::LitStr(s) => string_lits.push(s),
            ContractTy::IntIn(r) => int_ranges.push(int_range_keyword(*r)),
            ContractTy::LitInt(i) => int_lits.push(*i),
            ContractTy::LitFloat(f) => float_lits.push(*f),
            ContractTy::ArrayAny { .. }
            | ContractTy::ListOf { .. }
            | ContractTy::MapOf { .. }
            | ContractTy::Shape { .. } => array_members.push(spell_array_arm(arm)),
            // Any other arm (an object, a class, `callable`, …) has no faithful
            // plain-scalar spelling — the honest refusal, `type-not-renderable`.
            _ => return None,
        }
    }

    let mut members: Vec<String> = Vec::new();
    if has_int {
        members.push("int".to_owned());
    }
    members.extend(int_ranges);
    members.extend(int_lits.iter().map(i64::to_string));
    if has_float {
        members.push("float".to_owned());
    }
    members.extend(float_lits.iter().map(|f| float_literal(*f)));
    if let Some(kw) = string_keyword {
        members.push(kw);
    } else if let Some(spelled) = spell_string_literals(&string_lits) {
        members.extend(spelled);
    }
    if let Some(b) = bool_member {
        members.push(b.to_owned());
    }
    if nullable {
        members.push("null".to_owned());
    }
    members.extend(array_members);

    if members.is_empty() { None } else { Some(members.join("|")) }
}

// ---------------------------------------------------------------------------
// Array vocabulary (ADR-0062 §6) — the ONE speller, not a second
// renderer. [`spell_array_arm`]/[`spell_nested`] spell a `ContractTy` array
// form; [`spell_val`] spells a concrete `Val` (the "value-side counterpart"),
// sharing the same brace assembly ([`spell_shape`]) and key spelling
// ([`spell_key`]) so a declared shape and a concrete array agree on every
// rendering decision they share.
// ---------------------------------------------------------------------------

/// `array`/`list` with the `non-empty-` modifier PHPStan spells for
/// `ArrayAny`/`ListOf`/`Shape`'s own `non_empty` bit.
fn non_empty_keyword(base: &str, non_empty: bool) -> String {
    if non_empty { format!("non-empty-{base}") } else { base.to_owned() }
}

/// The base keyword a **sealed** shape spells — decided by the shape's own
/// `is_list` fact (issue #163), not by its key structure.
///
/// The #14939 model the domain already implements draws the line denotationally:
/// `array{…}` is an order-agnostic key *set*, `list{…}` a key *sequence*, and
/// `steins_domain`'s own `compute_is_list` answers `Yes` only when no
/// permutation is realizable. So the head keyword states which of the two the
/// shape actually is:
///
/// * **`is_list == Yes` → `list`**, because the sequence guarantee is a fact we
///   hold and `array{…}` would drop it on the way out. A sealed `array{0: A, 1:
///   B}` admits `[1 => B, 0 => A]` and so carries `Maybe`; a `list{A, B}`
///   carries `Yes`. Spelling both `array{A, B}` (as issue #159 did) makes one
///   name for two types and does not round-trip — re-parsing the rendering must
///   yield a shape with the same `is_list`, and that is what pins this rule.
/// * **anything else → `array`.** `Maybe` and `No` are both "not proven a
///   sequence", which is exactly what the key-set spelling says.
///
/// Two things the keys still decide, unchanged from issue #159:
///
/// * **the empty shape stays `array{}`.** It is vacuously a `Yes`-list, but its
///   braces already say "no keys at all", so neither word adds anything and both
///   re-parse to `Yes` — the reference model's own spelling wins the tie.
/// * **`non-empty-` is implied by any required key.** `array{a: int}` cannot be
///   the empty array; writing `non-empty-array{a: int}` says it twice. A sealed
///   shape with *no* required key (`non-empty-array{a?: int}`, which denotes
///   exactly `['a' => …]`) keeps the modifier — there the non-emptiness is a
///   real extra claim the keys do not make.
///
/// Issue #159's "two or more optional keys" carve-out is gone, and it is gone
/// because it was redundant rather than wrong: it kept `list` exactly where the
/// key set alone admits a gap, which is a proxy for "the keys do not prove
/// list-ness" — the thing `is_list` says directly. Every shape it selected has
/// `is_list == Yes` (nothing else reaches a `list` head at all), so the fact now
/// selects a superset and no row it protected changed.
///
/// Unsealed shapes are deliberately NOT routed here: an unsealed tail can admit
/// keys the braces never mention, so `non-empty-` stays genuinely informative and
/// keeps the spelling it has always had.
fn sealed_keyword(is_list: bool, non_empty: bool, fields: &[(Key, bool, String)]) -> String {
    let base = if is_list && !fields.is_empty() { "list" } else { "array" };
    let implied_non_empty = fields.iter().any(|(_, required, _)| *required);
    non_empty_keyword(base, non_empty && !implied_non_empty)
}

/// The shared spelling of the **generic** (fieldless) array vocabulary —
/// `array`, `non-empty-array`, `array<V>`, `array<K, V>`, `list<T>` — the
/// sibling of [`spell_shape`]'s brace assembly, and the ONE place that decides
/// it. Used by `spell_array_arm`'s degenerate arms and by the dump surface's
/// abstract-shape renderer, which must spell a fieldless shape fact (A-G1's
/// degenerate forms) exactly as the contract arm it lowered from was spelled.
///
/// `key`/`value` are already-spelled slot text; `None` is "no knowledge". A
/// list never prints a key (its key class is `int` by definition), and a
/// value-less list/map prints `mixed` — the loosest honest keyword, so the
/// spelling still round-trips to the same fact.
///
/// `not_list` renders Phan's `associative-array` base word instead of `array`
/// (census bucket ix) — mutually exclusive with `is_list` by construction (a
/// `ListOf` arm never sets it), never both true.
#[must_use]
pub fn spell_generic_array(
    is_list: bool,
    not_list: bool,
    non_empty: bool,
    key: Option<&str>,
    value: Option<&str>,
) -> String {
    if is_list {
        return format!(
            "{}<{}>",
            non_empty_keyword("list", non_empty),
            value.unwrap_or("mixed")
        );
    }
    let base = if not_list { "associative-array" } else { "array" };
    let kw = non_empty_keyword(base, non_empty);
    match (key, value) {
        (None, None) => kw,
        (None, Some(v)) => format!("{kw}<{v}>"),
        (Some(k), v) => format!("{kw}<{k}, {}>", v.unwrap_or("mixed")),
    }
}

/// Spell one array-vocabulary [`ContractTy`] arm (`ArrayAny`/`ListOf`/`MapOf`/
/// `Shape`). Panics if handed anything else — callers dispatch on the same
/// variant set [`spell_arms`]'s match arm does.
fn spell_array_arm(ty: &ContractTy) -> String {
    match ty {
        ContractTy::ArrayAny { non_empty } => {
            spell_generic_array(false, false, *non_empty, None, None)
        }
        ContractTy::ListOf { elem, non_empty } => {
            spell_generic_array(true, false, *non_empty, None, Some(&spell_nested(elem)))
        }
        ContractTy::MapOf { key, val, non_empty, not_list } => {
            let k = (!is_array_key_ty(key)).then(|| spell_nested(key));
            spell_generic_array(false, *not_list, *non_empty, k.as_deref(), Some(&spell_nested(val)))
        }
        ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => {
            spell_contract_shape(*list, fields, *sealed, *non_empty, unsealed)
        }
        _ => unreachable!("spell_array_arm is only called on the four array-vocabulary arms"),
    }
}

/// Spell a `Shape` arm through the shared brace assembly ([`spell_shape`]):
/// lower its declared fields/tail into the shared `(Key, required, spelled
/// value)` shape, compute the denotational `is_list` ([`shape_is_list`], the
/// ONE computation, `steins_domain::ShapeFact::normalize` underneath), and
/// hand off.
fn spell_contract_shape(
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
) -> String {
    let is_list = shape_is_list(list, fields, sealed, non_empty, unsealed) == Certainty::Yes;
    let mut spelled_fields: Vec<(Key, bool, String)> = fields
        .iter()
        .map(|f| (ckey_to_key(&f.key), !f.optional, spell_nested(&f.ty)))
        .collect();
    // A declared shape has no real order (ADR-0062 §2: the contract lane is
    // order-declared) — canonicalize by key, mirroring
    // `steins_domain::ShapeFact::normalize`'s own field order.
    spelled_fields.sort_by(|a, b| a.0.cmp(&b.0));
    let tail = match unsealed {
        None if sealed => ShapeTail::Sealed,
        None => ShapeTail::Untyped,
        Some((key, val)) => {
            let value = spell_nested(val);
            let key = key.as_ref().filter(|k| !is_array_key_ty(k)).map(|k| spell_nested(k));
            ShapeTail::Typed { key, value }
        }
    };
    spell_shape(is_list, non_empty, &spelled_fields, &tail)
}

fn ckey_to_key(k: &CKey) -> Key {
    match k {
        CKey::Int(i) => Key::Int(*i),
        CKey::Str(s) => Key::Str(s.clone()),
    }
}

/// Spell a single (non-union-flattened) type reaching an array's element/key/
/// value slot. Unlike [`spell_arms`], this never refuses: a nested slot with
/// no precise vocabulary here still needs *some* text so the enclosing shape
/// stays renderable, so it floors to the loosest honest keyword rather than
/// failing the whole shape. Class names spell in their lowered (lowercase,
/// unqualified-stripped) form — this crate has no source-casing table
/// (that lives in `steins-infer`'s `Cx`, the wrong dependency direction); a
/// class-typed array member is therefore a known, deliberate casing
/// divergence from the top-level dump surface's class rendering.
/// Render a callable arm back to the spelling its obligations came from
/// (ADR-0063 P3). The five reachable combinations are exactly the vocabulary
/// `callable_obl` recognizes, so this is a faithful round-trip; any unreachable
/// combination floors to the loosest honest keyword rather than inventing syntax
/// no analyzer would accept.
///
/// The parenthesized signature is *not* rendered here: `callable(int): int`
/// spells back as `callable`.
fn spell_callable(obl: CallableObl) -> &'static str {
    match (obl.pure, obl.is_static, obl.closure_only) {
        (true, false, false) => "pure-callable",
        (true, false, true) => "pure-closure",
        (false, true, true) => "static-closure",
        (true, true, true) => "static-pure-closure",
        _ => "callable",
    }
}

/// [`spell_nested`] for the crate's own tests: the nested spelling is what an
/// intersection/union arm reaches, and issue #238's round-trip property is stated
/// about exactly that rendering.
#[cfg(test)]
pub(crate) fn spell_nested_for_test(ty: &ContractTy) -> String {
    spell_nested(ty)
}

fn spell_nested(ty: &ContractTy) -> String {
    match ty {
        ContractTy::Mixed => "mixed".to_owned(),
        ContractTy::Never => "never".to_owned(),
        ContractTy::Opaque => "mixed".to_owned(),
        ContractTy::MixedMinus(MixedCut::Null) => "non-null-mixed".to_owned(),
        ContractTy::MixedMinus(MixedCut::Falsy) => "non-empty-mixed".to_owned(),
        ContractTy::Class(name) => name.clone(),
        ContractTy::ObjectAny => "object".to_owned(),
        ContractTy::CallableTy { obl, .. } => spell_callable(*obl).to_owned(),
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::Shape { .. } => spell_array_arm(ty),
        ContractTy::IterableOf { key, val } => {
            if is_array_key_ty(key) {
                format!("iterable<{}>", spell_nested(val))
            } else {
                format!("iterable<{}, {}>", spell_nested(key), spell_nested(val))
            }
        }
        ContractTy::Union(members) => spell_arms(members)
            .unwrap_or_else(|| members.iter().map(spell_nested).collect::<Vec<_>>().join("|")),
        ContractTy::Inter(members) => {
            members.iter().map(spell_nested).collect::<Vec<_>>().join("&")
        }
        // Every scalar/literal leaf: reuse spell_arms's own ladder on a
        // one-element slice rather than re-deriving it.
        _ => spell_arms(std::slice::from_ref(ty)).unwrap_or_else(|| "mixed".to_owned()),
    }
}

/// What a shape's undeclared keys may do (the third [`spell_shape`] input) —
/// mirrors [`steins_domain::Tail`], re-expressed with already-spelled text so
/// this module stays the only place that decides array-brace spelling.
pub enum ShapeTail {
    /// No unsealed tail: nothing printed.
    Sealed,
    /// A bare, untyped `...`.
    Untyped,
    /// A typed `...<V>` (key `None`, the `array-key` floor collapsed away) or
    /// `...<K, V>`.
    Typed {
        /// The tail's key spelling, when narrower than `array-key`.
        key: Option<String>,
        /// The tail's value spelling.
        value: String,
    },
}

/// The shared brace assembly (ADR-0062 §6): the ONE place that decides
/// `list{…}` vs `array{…}` and keyless-vs-keyed field spelling, used by both
/// the contract-arm path (`spell_contract_shape`) and the concrete-value
/// path ([`spell_val`]).
///
/// `is_list` is the caller's already-decided verdict (denotational
/// `Certainty::Yes`, per `shape_is_list`, or the exact
/// [`steins_domain::array_is_list`] answer for a concrete array — never
/// recomputed here). `fields` are `(key, required, spelled value)` **in the
/// order they print** — this function does not reorder them, deliberately:
/// a declared shape has no real order and its caller canonicalizes by
/// sorting the key (mirroring [`steins_domain::ShapeFact::normalize`]'s own
/// field order, ADR-0062 §2's "contract lane is order-declared"), while a
/// concrete value's caller passes true insertion order (the value lane is
/// order-witnessed, §2 again — the one place order is sound to print).
///
/// **A sealed shape spells its head from its own `is_list` fact** (issue #163):
/// the keyword comes from `sealed_keyword` (private, just above) — `list{…}`
/// when the fact says the shape is a key sequence, `array{…}` otherwise — and the
/// fields are positional
/// (`array{T, U}` — bare values, no key labels) exactly when the printed keys
/// are `0..n-1` in order and every one of them is required. That is an
/// all-or-nothing decision over the whole field list, not a per-field one: one
/// gap or one optional key and *every* field prints its key
/// (`array{0: T, 2: U}`, `array{0: T, 1?: U}`), because a bare value in a
/// list whose positions are not contiguous would name the wrong key.
///
/// **An unsealed shape keeps the per-field rule** it has always had — a field
/// spells keyless when it is required and its key is the next positional
/// auto-index (PHP's own shape-key rule, `shape_keys` in `lib.rs`, run in
/// reverse) — and keeps the plain `list`/`non-empty-` keyword, because an
/// unsealed tail can admit keys the braces never mention and both modifiers
/// therefore still say something the fields do not.
#[must_use]
pub fn spell_shape(
    is_list: bool,
    non_empty: bool,
    fields: &[(Key, bool, String)],
    tail: &ShapeTail,
) -> String {
    let sealed = matches!(tail, ShapeTail::Sealed);
    let kw = if sealed {
        sealed_keyword(is_list, non_empty, fields)
    } else {
        non_empty_keyword(if is_list { "list" } else { "array" }, non_empty)
    };
    // A key whose bytes are not UTF-8 has no phpdoc spelling at all: the
    // grammar's quoted key is text, and it has no `\xNN` escape to carry the
    // byte. The whole shape therefore widens to its bare keyword — a
    // supertype, which is the honest direction for a spelled contract
    // (ADR-0080 §2.5: decline, never guess).
    if fields.iter().any(|(k, _, _)| matches!(k, Key::Str(s) if !s.is_utf8())) {
        return kw.to_owned();
    }
    // Sealed: one verdict for the whole field list (vacuously true for the
    // fieldless `array{}`). Unsealed: never consulted — the per-field rule
    // below decides there.
    let positional = sealed
        && fields.iter().enumerate().all(|(i, (key, required, _))| {
            *required && matches!(key, Key::Int(n) if *n == i as i64)
        });

    let mut next_auto: i64 = 0;
    let mut parts: Vec<String> = Vec::with_capacity(fields.len() + 1);
    for (key, required, value) in fields {
        let keyless = if sealed {
            positional
        } else {
            *required && matches!(key, Key::Int(i) if *i == next_auto)
        };
        if keyless {
            parts.push(value.clone());
            next_auto += 1;
        } else {
            if let Key::Int(i) = key {
                next_auto = next_auto.max(i + 1);
            }
            let mark = if *required { "" } else { "?" };
            parts.push(format!("{}{mark}: {value}", spell_key(key)));
        }
    }
    match tail {
        ShapeTail::Sealed => {}
        ShapeTail::Untyped => parts.push("...".to_owned()),
        // A LIST tail never prints a key: the phpdoc grammar's list-shape tail is
        // `...<V>` and has no key slot at all (its key class is `int` by
        // definition), so a `list{…, ...<int, V>}` does not re-parse. This is the
        // rule [`spell_generic_array`] already states for the fieldless forms,
        // applied at the one other place a tail is printed.
        ShapeTail::Typed { key: Some(k), value } if !is_list => {
            parts.push(format!("...<{k}, {value}>"));
        }
        ShapeTail::Typed { value, .. } => parts.push(format!("...<{value}>")),
    }
    format!("{kw}{{{}}}", parts.join(", "))
}

/// Spell one shape key the way PHPStan's `array{}` grammar does: a bare
/// identifier-shaped string key unquoted (`a:`), everything else through the
/// shared literal escaper (`'a b':`); an int key is bare decimal. Distinct
/// from `steins-infer`'s `render_offset_key` (always-quoted "Steins phrasing"
/// for evidence clauses) — that rule is not the phpdoc-grammar rule, so it is
/// not reused here (and could not be: the dependency runs the other way).
fn spell_key(k: &Key) -> String {
    match k {
        Key::Int(i) => i.to_string(),
        // The `None` arm is unreachable: `spell_shape` widens any shape that
        // carries a non-UTF-8 key before it reaches this loop.
        Key::Str(s) => match s.as_str() {
            Some(t) if is_bare_ident(t) => t.to_owned(),
            Some(t) => string_literal(t),
            None => "string".to_owned(),
        },
    }
}

/// A PHP-identifier-shaped string: `[A-Za-z_][A-Za-z0-9_]*`. PHPStan's own
/// shape grammar accepts a bare key exactly on this shape.
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Value-precise spelling of one concrete PHP value (the `spell_arms`
/// "value-side counterpart", ADR-0062 §6): scalars as literals, arrays
/// through the shared [`spell_shape`] assembly, recursing on nested arrays.
/// Unlike [`spell_arms`]/[`summarize_vals`](crate::normalize::summarize_vals),
/// this never refuses — every [`Val`] has a faithful spelling, which is
/// exactly why the dump surface's concrete-value path (`Fact::Singleton`/
/// `OneOf` members) calls this instead of going through the arm-list
/// normalizer.
#[must_use]
pub fn spell_val(v: &Val) -> String {
    match v {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => float_literal(*f),
        // A byte string has no phpdoc literal spelling; `string` is its honest
        // supertype (ADR-0080 §2.5).
        Val::Str(s) => s.as_str().map_or_else(|| "string".to_owned(), string_literal),
        Val::Bool(true) => "true".to_owned(),
        Val::Bool(false) => "false".to_owned(),
        Val::Null => "null".to_owned(),
        Val::Array(entries) => spell_array_entries(entries),
    }
}

/// A concrete array value: order-witnessed (ADR-0062 §2), so `is_list` is the
/// exact [`steins_domain::array_is_list`] answer, never the trinary — every
/// field is `Required` (a concrete value has no unknowns) and the tail is
/// always `Sealed` (the value's entries are its whole denotation).
fn spell_array_entries(entries: &[(Key, Val)]) -> String {
    let is_list = steins_domain::array_is_list(entries);
    let fields: Vec<(Key, bool, String)> =
        entries.iter().map(|(k, v)| (k.clone(), true, spell_val(v))).collect();
    spell_shape(is_list, false, &fields, &ShapeTail::Sealed)
}

/// Spell a group of string literals (terminal-safe): a single value is its escaped
/// literal, a small set (≤ [`CAP`] distinct) is a literal union, and a larger set
/// widens to the tightest refined-string keyword its shared predicate summary
/// admits. `None` for an empty group (no string members).
///
/// This is the CAP-bounded half of the string ladder. The `*/`/newline docblock
/// safety is deliberately absent — that armor lives in the docblock renderer and
/// runs before this, so any literal reaching here is safe to embed *as terminal
/// text* (single-quote/backslash escaping still applies via [`string_literal`]).
fn spell_string_literals(strings: &[&PhpStr]) -> Option<Vec<String>> {
    if strings.is_empty() {
        return None;
    }
    let mut distinct: Vec<&PhpStr> = strings.to_vec();
    distinct.sort_unstable();
    distinct.dedup();

    // A byte string has no phpdoc literal spelling at all (ADR-0080 §2.5), so a
    // group carrying one skips the literal arm and widens to the shared
    // predicate keyword below — a supertype, never a guessed spelling.
    if distinct.len() <= CAP && distinct.iter().all(|s| s.is_utf8()) {
        // One value, or a small enum-like set: precise literal / literal union.
        return Some(distinct.iter().filter_map(|s| s.as_str()).map(string_literal).collect());
    }

    // Larger than CAP: widen to the tightest predicate keyword the shared,
    // implication-closed predicate summary admits.
    let mut preds = StrPreds::of(distinct[0]);
    for s in &distinct[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    Some(vec![preds_keyword(preds)])
}

/// How an int interval spells: always PHPStan's explicit `int<lo, hi>` form, with
/// `min`/`max` for the domain ends and a space after the comma.
///
/// `positive-int`, `non-negative-int`, and `negative-int` are phpdoc **input**
/// sugar — [`crate::lower_identifier`] still accepts all three — but they are not
/// output spellings: PHPStan folds each into an integer range and describes every
/// range as the interval, which is why no nsrt fixture asserts a keyword form
/// anywhere (issue #90). Spelling the sugar back made the dump disagree with
/// PHPStan on a set the two actually agreed about.
///
/// The `min`/`max` sentinels matter for the same reason: `int<17, max>` is
/// PHPStan's spelling of a half-open range, and printing `i64::MAX` in full
/// digits was a second way to say the same set differently.
///
/// This is the ONE int-range spelling — the value-fact renderer on the dump
/// surface calls it too, so the two paths cannot drift.
#[must_use]
pub fn int_range_keyword(r: IntRange) -> String {
    let bound = |v: i64, sentinel: i64, name: &str| {
        if v == sentinel { name.to_owned() } else { v.to_string() }
    };
    format!(
        "int<{}, {}>",
        bound(r.lo(), i64::MIN, "min"),
        bound(r.hi(), i64::MAX, "max")
    )
}

/// Spell a float literal as PHPStan does: an integral value keeps a visible
/// fractional part (`5.0`, not `5`); every other value uses its shortest
/// round-tripping decimal (`3.14`).
fn float_literal(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 { format!("{f:.1}") } else { f.to_string() }
}

/// The tightest refined-string keyword a predicate summary admits: the **closed
/// grid** `core × casing` (issue #240), where
///
/// * core ∈ {—, `non-empty-`, `non-falsy-`, `numeric-`, `non-falsy-numeric-`},
///   the length/numeric ladder — `non-falsy-numeric-` is its own rung because
///   `NUMERIC` does not entail `NON_FALSY` (`'0'` is both numeric and falsy); and
/// * casing ∈ {—, `lowercase-`, `uppercase-`, `uncased-`}, where `uncased-` is
///   Steins' own word for `LOWERCASE ∧ UPPERCASE` (a string with no cased
///   character), the set PHPStan spells `lowercase-string&uppercase-string`.
///
/// One keyword comes out, never an intersection. That is ADR-0030's vocabulary
/// rule, not a limitation: this crate emits phpdoc that has to lower back through
/// [`crate::lower_identifier`], and every cell here does
/// (`crate::grid_str_preds` is the inverse, pinned by `every_grid_cell_round_trips`)
/// while `A&B` would need a matching entry per cell anyway.
///
/// The grid replaced a single-keyword ladder that ranked the axes against each
/// other and widened the loser away — a set holding `{NON_FALSY, LOWERCASE}`
/// spelled `non-falsy-string` and the casing half was *invisible*, which the #235
/// probe measured as the speller's whole loss. Every casing-free set keeps exactly
/// the spelling that ladder gave it, save the one new core rung.
///
/// The array-key-cast pair is deliberately **not** an axis, and issue #240 did not
/// change that. `decimal-int-string` would be a legitimate rung (it is tighter than
/// `numeric-string`), but the predicate is computed by `StrPreds::of` for every
/// string value, so adding it would silently re-spell every *value-derived*
/// all-canonical-decimal set — `'1'|'2'` widening to `decimal-int-string` rather
/// than `numeric-string` — as a side effect of teaching the checker a keyword.
/// `non-decimal-int-string` is not a rung for the mirror reason: nearly every
/// string carries the bit, so it says almost nothing about a set. Both therefore
/// widen away here, and a declared `decimal-int-string` still round-trips to a
/// strictly wider cell — a widening, never a lie.
///
/// The casing axis DOES read through `DECIMAL_INT`'s closure (`⇒ LOWERCASE ∧
/// UPPERCASE`, since a canonical decimal has no cased character), so that
/// round-trip is now `numeric-uncased-string` rather than `numeric-string` and a
/// value-derived decimal set spells `uncased-` too. That is not the re-spelling
/// refused above: the casing bits are *true of every member* and are said by the
/// axis that already exists, while the refusal is about not minting a keyword for
/// the array-key-cast predicate itself. Measured over the nsrt corpus, no row
/// moved away from admissible.
#[must_use]
pub fn preds_keyword(preds: StrPreds) -> String {
    // `class-string` outranks every core rung: it is the only *contextual*
    // predicate (issue #236), so it says something none of the character-level
    // rungs can, and dropping it in favour of `non-falsy-string` — which it
    // entails — would throw away the whole claim. Round-trips through
    // `lower_identifier` back to the same set.
    if preds.contains_all(StrPreds::CLASS_STRING) {
        return "class-string".to_owned();
    }
    let casing = match (
        preds.contains_all(StrPreds::LOWERCASE),
        preds.contains_all(StrPreds::UPPERCASE),
    ) {
        (true, true) => "uncased-",
        (true, false) => "lowercase-",
        (false, true) => "uppercase-",
        (false, false) => "",
    };
    let core = if preds.contains_all(StrPreds::NUMERIC) {
        if preds.contains_all(StrPreds::NON_FALSY) { "non-falsy-numeric-" } else { "numeric-" }
    } else if preds.contains_all(StrPreds::NON_FALSY) {
        "non-falsy-"
    } else if preds.contains_all(StrPreds::NON_EMPTY) {
        "non-empty-"
    } else {
        ""
    };
    format!("{core}{casing}string")
}

/// Render one PHP string as a single-quoted phpdoc literal, escaping `\` and `'`
/// exactly as PHP single-quoted syntax requires (round-tripped through
/// `steins_phpdoc::parse_type` in the honesty tests). Terminal-safe by
/// construction; the docblock renderer decides *whether* a value may be spelled as
/// a literal at all before calling this.
#[must_use]
pub fn string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::summarize_vals;
    use steins_domain::Val;

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.into())
    }

    /// Spell the summarized arms of a value set — the path the dump/annotate
    /// emitters take (summarize → spell), with no docblock armor in the way.
    fn spell_vals(vals: &[Val]) -> Option<String> {
        spell_arms(&summarize_vals(vals)?)
    }

    #[test]
    fn int_and_numeric_strings_render_the_canonical_union() {
        assert_eq!(spell_vals(&[i(1), s("12"), s("34")]).unwrap(), "int|numeric-string");
    }

    #[test]
    fn single_string_is_its_literal() {
        assert_eq!(spell_vals(&[s("123")]).unwrap(), "'123'");
    }

    #[test]
    fn enum_like_strings_render_a_sorted_literal_union() {
        assert_eq!(spell_vals(&[s("POST"), s("GET"), s("GET")]).unwrap(), "'GET'|'POST'");
    }

    #[test]
    fn dedup_and_nullable_and_bool() {
        assert_eq!(spell_vals(&[i(1), i(2), i(1)]).unwrap(), "int");
        assert_eq!(spell_vals(&[i(1), Val::Null]).unwrap(), "int|null");
        assert_eq!(spell_vals(&[Val::Bool(true), Val::Bool(false)]).unwrap(), "bool");
        assert_eq!(spell_vals(&[Val::Bool(true)]).unwrap(), "true");
    }

    #[test]
    fn array_bearing_set_is_not_renderable() {
        assert_eq!(spell_vals(&[Val::Array(vec![])]), None);
    }

    /// Terminal spelling has no `*/` hazard: a `*/`-bearing literal is spelled as
    /// its escaped literal here (the docblock renderer, not this function, widens
    /// it). This is a deliberate divergence from the docblock renderer.
    #[test]
    fn star_slash_literal_is_spelled_verbatim_in_terminal() {
        assert_eq!(spell_vals(&[s("a*/b")]).unwrap(), "'a*/b'");
    }

    #[test]
    fn escaping_quotes_and_backslashes() {
        assert_eq!(string_literal("a'b"), "'a\\'b'");
        assert_eq!(string_literal("c\\d"), "'c\\\\d'");
    }

    /// Above CAP distinct literals widen to the tightest keyword — which since
    /// issue #240 states BOTH axes of the shared summary. `'k0'…'k8'` really are
    /// all lowercase, so the casing half is part of the answer; the ladder that
    /// spelled this `non-falsy-string` was dropping a predicate every member has.
    #[test]
    fn over_cap_widens_to_keyword() {
        let vals: Vec<Val> = (0..=CAP as i64).map(|n| s(&format!("k{n}"))).collect();
        assert_eq!(spell_vals(&vals).unwrap(), "non-falsy-lowercase-string");
    }

    /// …and a set whose members disagree about casing keeps the bare core rung:
    /// `intersect` drops both bits, so the grid's casing half is empty.
    #[test]
    fn over_cap_mixed_casing_widens_to_the_core_rung() {
        let vals: Vec<Val> =
            (0..=CAP as i64).map(|n| s(&format!("{}{n}", if n % 2 == 0 { "k" } else { "K" }))).collect();
        assert_eq!(spell_vals(&vals).unwrap(), "non-falsy-string");
    }
}

/// ADR-0062 §6 — the array vocabulary `spell_arms` renders, plus its
/// concrete-value counterpart [`spell_val`].
#[cfg(test)]
mod array_vocabulary_tests {
    use super::*;
    use crate::lower_str;

    /// Round-trip a phpdoc array type through `lower` then `spell_arms` on a
    /// one-arm slice — the path a seeded `@param` contract arm takes.
    fn spell_ty(src: &str) -> String {
        let ty = lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower"));
        spell_arms(std::slice::from_ref(&ty)).unwrap_or_else(|| panic!("{src} did not spell"))
    }

    /// The denotational `is_list` of a lowered array-shape arm — the same ONE
    /// computation `spell_contract_shape` spells from.
    fn is_list_of(src: &str) -> Certainty {
        match lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower")) {
            ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => {
                shape_is_list(list, &fields, sealed, non_empty, &unsealed)
            }
            _ => panic!("{src} did not lower to a shape"),
        }
    }

    #[test]
    fn re_parsing_a_spelled_shape_yields_the_same_is_list() {
        // Issue #163's self-check, and the property issue #159 broke: the head
        // keyword is a claim about the shape's `is_list`, so reading our own
        // output back has to reproduce it. Any rule that spells from key
        // structure instead of from the fact fails this on the `list{A, B}` row —
        // it renders `array{A, B}`, which re-parses to `Maybe`.
        for src in [
            // Sealed, every list-ness verdict the domain can reach.
            "array{}",
            "array{int}",
            "array{0: int}",
            "array{0?: int}",
            "non-empty-array{0?: int}",
            "list{int}",
            "list{int, string}",
            "array{0: int, 1: string}",
            "array{0: int, 2: string}",
            "list{int, 1?: string, 2?: int}",
            "array{0: int, 1?: string, 2?: int}",
            "array{0: int, name?: string}",
            "array{a: int}",
            "non-empty-array{a: int}",
            "non-empty-array{a?: int}",
            "array{a?: string, b?: string}",
            "array{a: int, b: string}",
            "array{'a b': int}",
            // Unsealed, which issue #159 left alone and #163 keeps alone.
            "array{a: int, ...}",
            "non-empty-array{a: int, ...}",
            "array{a: int, ...<string, int>}",
            "array{0: int, ...}",
        ] {
            let before = is_list_of(src);
            let spelled = spell_ty(src);
            assert_eq!(
                is_list_of(&spelled),
                before,
                "{src} spelled {spelled}, whose is_list is not the one it was spelled from"
            );
        }
    }

    #[test]
    fn seeded_optional_shape_spells_instead_of_refusing() {
        // The #51 fixture: a seeded array param spells rather than refuses (the
        // "no declared contract" flip lives on the steins-infer dump surface; this
        // pins the underlying spelling it spells from).
        assert_eq!(
            spell_ty("array{a?: string, b?: string}"),
            "array{a?: string, b?: string}"
        );
    }

    #[test]
    fn list_generic_spells_unchanged() {
        assert_eq!(spell_ty("list<string>"), "list<string>");
        assert_eq!(spell_ty("non-empty-list<int>"), "non-empty-list<int>");
    }

    #[test]
    fn map_generic_spells_both_key_and_value() {
        assert_eq!(spell_ty("array<string, int>"), "array<string, int>");
    }

    #[test]
    fn associative_array_generic_spells_the_phan_keyword() {
        // Census bucket ix: round-trips through the `not_list` `MapOf` flag,
        // not a bare `array<K, V>` — the whole point of the spelling.
        assert_eq!(
            spell_ty("associative-array<int, string>"),
            "associative-array<int, string>"
        );
        assert_eq!(
            spell_ty("non-empty-associative-array<string, int>"),
            "non-empty-associative-array<string, int>"
        );
    }

    #[test]
    fn map_generic_single_arg_collapses_the_array_key_floor() {
        // A single-arg `array<V>` lowers its key to the `array-key` union
        // (`lib.rs::array_key`); the speller collapses that back to the terser
        // single-arg spelling rather than the verbose `array<int|string, V>`.
        assert_eq!(spell_ty("array<int>"), "array<int>");
    }

    #[test]
    fn a_required_key_absorbs_the_non_empty_modifier_on_a_sealed_shape() {
        // Issue #159: `a` is required, so the shape cannot be the empty array
        // and the modifier is saying it a second time — the reference model
        // never writes it. Nothing is lost: re-lowering `array{a: int}` proves
        // non-emptiness from the key again.
        assert_eq!(spell_ty("non-empty-array{a: int}"), "array{a: int}");
    }

    #[test]
    fn a_wholly_optional_sealed_shape_keeps_the_non_empty_modifier() {
        // The exception: with no required key the braces admit `[]`, so
        // `non-empty-` is a real extra claim (this denotes exactly
        // `['a' => …]`) and dropping it would widen the type.
        assert_eq!(
            spell_ty("non-empty-array{a?: int}"),
            "non-empty-array{a?: int}"
        );
    }

    #[test]
    fn untyped_unsealed_tail_spells_bare_ellipsis() {
        assert_eq!(spell_ty("array{a: int, ...}"), "array{a: int, ...}");
    }

    #[test]
    fn typed_unsealed_tail_spells_key_and_value() {
        assert_eq!(
            spell_ty("array{a: int, ...<string, int>}"),
            "array{a: int, ...<string, int>}"
        );
    }

    #[test]
    fn bare_array_and_non_empty_array_keywords() {
        assert_eq!(spell_ty("array"), "array");
        assert_eq!(spell_ty("non-empty-array"), "non-empty-array");
    }

    #[test]
    fn a_declared_list_shape_keeps_the_list_word_a_keyed_one_never_earns() {
        // Issue #163, and the whole point of it. These two are NOT the same
        // type: `array{0: int, 1: string}` is a key SET and admits
        // `[1 => 'x', 0 => 1]`, so its `is_list` is `Maybe`; the `list{…}`
        // declaration promises a key SEQUENCE and carries `Yes`. The head
        // keyword states which one we hold.
        assert_eq!(spell_ty("list{int, string}"), "list{int, string}");
        assert_eq!(spell_ty("array{0: int, 1: string}"), "array{int, string}");
    }

    #[test]
    fn a_single_key_zero_shape_is_a_sequence_however_it_is_declared() {
        // At most one key — key `0` — can appear, so no permutation is
        // realizable and `compute_is_list` answers `Yes` without any
        // declaration: both of these ARE sequences and say so. Making the field
        // optional also takes the shape out of the `0..n-1` all-required run, so
        // its key is printed.
        assert_eq!(spell_ty("array{0: int}"), "list{int}");
        assert_eq!(spell_ty("array{0?: int}"), "list{0?: int}");
    }

    #[test]
    fn two_optional_keys_keep_the_list_word_from_the_fact_not_a_carve_out() {
        // Issue #159 special-cased this row (PHPStan's `shouldBeDescribedAsAList`):
        // keys `{0, 1?, 2?}` admit the gapped `[0 => …, 2 => …]`, so dropping the
        // word would WIDEN the type. Issue #163 removed the special case and the
        // row did not move — the declaration's `Yes` is what kept the word, and
        // "the keys do not prove list-ness" was only ever a proxy for it.
        assert_eq!(
            spell_ty("list{int, 1?: string, 2?: int}"),
            "list{0: int, 1?: string, 2?: int}"
        );
        // The same key structure WITHOUT the declaration is only `Maybe`, and
        // that is the row the carve-out could never have distinguished.
        assert_eq!(
            spell_ty("array{0: int, 1?: string, 2?: int}"),
            "array{0: int, 1?: string, 2?: int}"
        );
    }

    #[test]
    fn stringly_keys_spell_keyed_array() {
        assert_eq!(spell_ty("array{a: int, b: string}"), "array{a: int, b: string}");
    }

    #[test]
    fn a_gap_keys_every_field_not_just_the_one_that_breaks_the_run() {
        // Issue #159: on a sealed shape the positional form is an all-or-nothing
        // verdict over the whole field list, not the per-field auto-index rule.
        // Key 0 would still be at its own position, but a bare leading value in
        // `array{int, 2: string}` reads as "the keys run 0, 1, …", which these
        // keys do not — so both fields print their key, as the reference model
        // prints them.
        assert_eq!(spell_ty("array{0: int, 2: string}"), "array{0: int, 2: string}");
    }

    #[test]
    fn an_unsealed_shape_is_spelled_exactly_as_before() {
        // Issue #159 is scoped to SEALED shapes. An unsealed tail can admit keys
        // the braces never mention, so `non-empty-` and `list` still say
        // something the fields do not — the pin that the sealed rule did not
        // leak across the boundary. Driven through `spell_shape` directly so the
        // tail variants are exercised as such.
        let one = [(Key::Int(0), true, "int".to_owned())];
        assert_eq!(
            spell_shape(true, true, &one, &ShapeTail::Untyped),
            "non-empty-list{int, ...}"
        );
        assert_eq!(
            spell_shape(false, true, &one, &ShapeTail::Untyped),
            "non-empty-array{int, ...}"
        );
        assert_eq!(
            spell_shape(
                false,
                false,
                &one,
                &ShapeTail::Typed { key: Some("string".to_owned()), value: "int".to_owned() }
            ),
            "array{int, ...<string, int>}"
        );
        // …and the same field list, sealed, drops the implied `non-empty-` while
        // keeping the `list` word the `is_list` argument asserts (issue #163).
        assert_eq!(spell_shape(true, true, &one, &ShapeTail::Sealed), "list{int}");
        assert_eq!(spell_ty("non-empty-array{a: int, ...}"), "non-empty-array{a: int, ...}");
    }

    #[test]
    fn quoted_keys_spell_bare_when_identifier_shaped_else_quoted() {
        assert_eq!(spell_ty("array{'a': int}"), "array{a: int}");
        assert_eq!(spell_ty("array{'a b': int}"), "array{'a b': int}");
    }

    // ---- The value-side counterpart: spell_val -----------------------------

    fn av(entries: Vec<(Key, Val)>) -> Val {
        Val::Array(entries)
    }

    #[test]
    fn empty_array_value_spells_the_empty_shape() {
        // array_is_list([]) is vacuously true (§3), so this is a `Yes`-list —
        // the one place issue #163 does NOT print the word. `array{}` already
        // says "no keys at all", both spellings re-parse to the same `Yes`, and
        // the reference model's own spelling wins the tie.
        assert_eq!(spell_val(&av(vec![])), "array{}");
    }

    #[test]
    fn keyed_string_map_value_spells_array() {
        assert_eq!(
            spell_val(&av(vec![(Key::Str("a".into()), Val::Str("v".into()))])),
            "array{a: 'v'}"
        );
    }

    #[test]
    fn sequential_list_value_spells_the_positional_list() {
        // A concrete array is order-witnessed (ADR-0062 §2), so `array_is_list`
        // answers exactly and this value IS a sequence — spelled `list{…}` in
        // the positional form (issue #163).
        assert_eq!(
            spell_val(&av(vec![
                (Key::Int(0), Val::Str("x".into())),
                (Key::Int(1), Val::Str("y".into())),
            ])),
            "list{'x', 'y'}"
        );
    }

    #[test]
    fn nested_array_values_recurse() {
        assert_eq!(
            spell_val(&av(vec![(
                Key::Int(0),
                av(vec![(Key::Int(0), Val::Int(1)), (Key::Int(1), Val::Int(2))])
            )])),
            "list{list{1, 2}}"
        );
    }

    #[test]
    fn out_of_order_int_keys_print_their_keys() {
        // Order-witnessed (§2): [1 => 'a', 0 => 'b'] is NOT array_is_list (the
        // keys are not 0..n-1 IN INSERTION ORDER). The key SET is still exactly
        // {0, 1}, but concrete values print in their real insertion order
        // (never sorted), so the printed keys are 1, 0 — not the positional
        // run — and every field prints its key. Order is the whole content of
        // this row: `array{'a', 'b'}` would name the wrong value for each key.
        assert_eq!(
            spell_val(&av(vec![
                (Key::Int(1), Val::Str("a".into())),
                (Key::Int(0), Val::Str("b".into())),
            ])),
            "array{1: 'a', 0: 'b'}"
        );
    }
}
