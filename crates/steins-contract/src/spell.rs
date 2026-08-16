//! Plain-text spelling of a summarized contract-arm list (ADR-0053 §7 / ADR-0052 §4).
//!
//! [`normalize::summarize_vals`](crate::normalize::summarize_vals) produces the
//! semantic normal form of a proven value set; this spells it as a
//! terminal-safe phpdoc-grammar type string (`int|numeric-string|null`, …) —
//! used by `steins-infer`'s `annotate`/dump emitters (can't reach
//! `steins-edit`'s docblock renderer, ADR-0053 §7) and by
//! `steins-edit::render_value_domain`, which pre-widens for docblock safety
//! then delegates to [`spell_arms`]. Byte-identical against `steins-edit`'s
//! honesty tests and cross-crate parity test.

use steins_domain::{Base, Certainty, IntRange, Key, PhpStr, StrPreds, Val, CAP};

use crate::{is_array_key_ty, shape_is_list, CallableObl, CField, CKey, ContractTy, MixedCut};

/// Spell a summarized contract-arm list as a terminal-safe phpdoc type, or `None`
/// when no faithful scalar spelling exists; matches
/// [`summarize_vals`](crate::normalize::summarize_vals)'s own `None`.
///
/// Emits members in `summarize_vals`'s canonical order, joined with `|`.
/// String literals spell as a literal, or (≤ [`CAP`]) a literal union, else
/// widen to the tightest refined-string keyword. Unlike the docblock
/// renderer, never widens for the `*/`/newline hazard.
#[must_use]
pub fn spell_arms(arms: &[ContractTy]) -> Option<String> {
    let mut has_int = false;
    let mut has_float = false;
    let mut bool_member: Option<&'static str> = None;
    let mut nullable = false;
    // Int-flavored refinement/literal arms (`positive-int`, `int<1, 5>`, `5`):
    // empty here since summarized ints collapse to `Base(Int)` (ADR-0052 §9).
    let mut int_ranges: Vec<String> = Vec::new();
    let mut int_lits: Vec<i64> = Vec::new();
    let mut float_lits: Vec<f64> = Vec::new();
    // A summarized set yields the numeric-string class or literal arms, never both.
    let mut string_keyword: Option<String> = None;
    let mut string_lits: Vec<&PhpStr> = Vec::new();
    // Array-vocabulary arms (ADR-0062 §6): appended after scalars, never blocking them.
    let mut array_members: Vec<String> = Vec::new();
    // Resource leaf (ADR-0056 §8.4): only reachable via the contract-arm surface
    // (no `Val` is a resource); lets a resource dump as `resource`, not `unknown`.
    let mut has_resource = false;
    for arm in arms {
        match arm {
            ContractTy::Base(Base::Int) => has_int = true,
            ContractTy::Base(Base::Float) => has_float = true,
            ContractTy::Base(Base::Bool) => bool_member = Some("bool"),
            ContractTy::LitBool(true) => bool_member = Some("true"),
            ContractTy::LitBool(false) => bool_member = Some("false"),
            ContractTy::Null => nullable = true,
            ContractTy::StrWith(p) => string_keyword = Some(preds_keyword(*p)),
            // `A&B` over string refinements folds to one predicate set (issue #240).
            ContractTy::Inter(members) => {
                string_keyword = Some(preds_keyword(crate::inter_str_preds(members)?));
            }
            ContractTy::Base(Base::String) => string_keyword = Some("string".to_owned()),
            ContractTy::LitStr(s) => string_lits.push(s),
            ContractTy::IntIn(r) => int_ranges.push(int_range_keyword(*r)),
            ContractTy::LitInt(i) => int_lits.push(*i),
            ContractTy::LitFloat(f) => float_lits.push(*f),
            ContractTy::Resource => has_resource = true,
            ContractTy::ArrayAny { .. }
            | ContractTy::ListOf { .. }
            | ContractTy::MapOf { .. }
            | ContractTy::Shape { .. } => array_members.push(spell_array_arm(arm)),
            // No faithful plain-scalar spelling: the `type-not-renderable`
            // refusal. `unset` lands here and stays refused — it is vocabulary,
            // not a value, so no summarized value set contains it (ADR-0087);
            // the spelling that renders it is `spell_nested`.
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
    // Last: PHP's own type list doesn't include resource.
    if has_resource {
        members.push("resource".to_owned());
    }

    if members.is_empty() { None } else { Some(members.join("|")) }
}

// ---------------------------------------------------------------------------
// Array vocabulary (ADR-0062 §6) — the ONE speller. [`spell_array_arm`]/
// [`spell_nested`] spell a `ContractTy`; [`spell_val`] spells a concrete
// `Val`, sharing brace assembly ([`spell_shape`]) and key spelling ([`spell_key`]).
// ---------------------------------------------------------------------------

/// `array`/`list` with the `non-empty-` modifier PHPStan spells for
/// `ArrayAny`/`ListOf`/`Shape`'s own `non_empty` bit.
fn non_empty_keyword(base: &str, non_empty: bool) -> String {
    if non_empty { format!("non-empty-{base}") } else { base.to_owned() }
}

/// The base keyword a **sealed** shape spells — decided by `is_list` (issue
/// #163), not key structure: `Yes` → `list`, else → `array` (`array{A, B}`
/// for both, the old #159 behavior, collapsed two types and didn't
/// round-trip). The empty shape stays `array{}`; `non-empty-` is implied by
/// (and dropped for) any required key, kept when none is required
/// (`non-empty-array{a?: int}`), since non-emptiness is then a real claim.
///
/// #159's "two or more optional keys" carve-out is gone (redundant with
/// `is_list`). Unsealed shapes are NOT routed here — their tail can admit
/// keys the braces don't show, so `non-empty-`/`list` stay informative.
fn sealed_keyword(is_list: bool, non_empty: bool, fields: &[(Key, bool, String)]) -> String {
    let base = if is_list && !fields.is_empty() { "list" } else { "array" };
    let implied_non_empty = fields.iter().any(|(_, required, _)| *required);
    non_empty_keyword(base, non_empty && !implied_non_empty)
}

/// The shared spelling of the **generic** (fieldless) array vocabulary —
/// `array`, `non-empty-array`, `array<V>`, `array<K, V>`, `list<T>`.
/// `key`/`value` are already-spelled text (`None` = no knowledge); a list
/// never prints a key. `not_list` renders Phan's `associative-array` word
/// (census bucket ix) instead of `array`.
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

/// Spell one array-vocabulary [`ContractTy`] arm. Panics on anything else —
/// callers dispatch on the same variant set [`spell_arms`]'s match arm does.
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
/// lower fields/tail, compute `is_list` ([`shape_is_list`]), hand off.
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
    // Declared shapes have no real order (ADR-0062 §2); canonicalize by key,
    // mirroring `steins_domain::ShapeFact::normalize`.
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

/// Spell a single nested array slot; unlike [`spell_arms`] never refuses
/// (floors to the loosest honest keyword). Class names spell
/// lowered/unqualified-stripped (casing table lives in `steins-infer`'s `Cx`).
/// Render a callable arm back to its obligations (ADR-0063 P3); the
/// parenthesized signature is not rendered (`callable(int): int` → `callable`).
fn spell_callable(obl: CallableObl) -> &'static str {
    match (obl.pure, obl.is_static, obl.closure_only) {
        (true, false, false) => "pure-callable",
        (true, false, true) => "pure-closure",
        (false, true, true) => "static-closure",
        (true, true, true) => "static-pure-closure",
        _ => "callable",
    }
}

/// [`spell_nested`] for tests: what an intersection/union arm reaches
/// (issue #238's round-trip property).
#[cfg(test)]
pub(crate) fn spell_nested_for_test(ty: &ContractTy) -> String {
    spell_nested(ty)
}

fn spell_nested(ty: &ContractTy) -> String {
    match ty {
        ContractTy::Mixed => "mixed".to_owned(),
        ContractTy::Never => "never".to_owned(),
        ContractTy::Opaque => "mixed".to_owned(),
        // The whole reason `unset` has its own variant (ADR-0087): the opaque
        // floor above spells as `mixed`, so parking the word there would lose
        // it, and `\DateTime|unset` would round-trip as `\DateTime|mixed`.
        ContractTy::Unset => "unset".to_owned(),
        ContractTy::MixedMinus(MixedCut::Null) => "non-null-mixed".to_owned(),
        ContractTy::MixedMinus(MixedCut::Falsy) => "non-empty-mixed".to_owned(),
        ContractTy::Class(name) => name.clone(),
        ContractTy::ObjectAny => "object".to_owned(),
        // One spelling for all three forms: open/closed is not modeled.
        ContractTy::Resource => "resource".to_owned(),
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
        // Scalar/literal leaf: reuse spell_arms's own ladder on one element.
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
    /// A typed `...<V>` (key `None` = `array-key` floor) or `...<K, V>`.
    Typed {
        /// The tail's key spelling, when narrower than `array-key`.
        key: Option<String>,
        /// The tail's value spelling.
        value: String,
    },
}

/// The shared brace assembly (ADR-0062 §6): the ONE place deciding `list{…}`
/// vs `array{…}` and keyless-vs-keyed field spelling. `is_list` is the
/// caller's already-decided verdict; `fields` print unreordered (a declared
/// shape's caller sorts by key, ADR-0062 §2; a concrete value's caller
/// passes true insertion order).
///
/// A **sealed** shape's head comes from `is_list` (issue #163); fields print
/// positional only when ALL keys are `0..n-1`, in order, required — one gap
/// and every field prints its key. An **unsealed** shape keeps the
/// per-field auto-index rule instead.
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
    // A non-UTF-8 key has no phpdoc spelling; widens to the bare keyword
    // (ADR-0080 §2.5: decline, never guess).
    if fields.iter().any(|(k, _, _)| matches!(k, Key::Str(s) if !s.is_utf8())) {
        return kw.to_owned();
    }
    // Sealed: one verdict for the whole list. Unsealed: never consulted.
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
        // A list tail never prints a key (`...<V>` only; key class is `int`),
        // same rule [`spell_generic_array`] states for the fieldless forms.
        ShapeTail::Typed { key: Some(k), value } if !is_list => {
            parts.push(format!("...<{k}, {value}>"));
        }
        ShapeTail::Typed { value, .. } => parts.push(format!("...<{value}>")),
    }
    format!("{kw}{{{}}}", parts.join(", "))
}

/// Spell one shape key as PHPStan's `array{}` grammar does: bare identifier
/// unquoted, else through the literal escaper; int keys are bare decimal.
/// Distinct from `steins-infer`'s always-quoted `render_offset_key`.
fn spell_key(k: &Key) -> String {
    match k {
        Key::Int(i) => i.to_string(),
        // Unreachable: `spell_shape` widens non-UTF-8 keys before this loop.
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

/// Value-precise spelling of one concrete PHP value (`spell_arms`' value-side
/// counterpart, ADR-0062 §6). Unlike [`spell_arms`], never refuses — every
/// [`Val`] has a faithful spelling.
#[must_use]
pub fn spell_val(v: &Val) -> String {
    match v {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => float_literal(*f),
        // A byte string has no phpdoc literal spelling; `string` is its honest supertype.
        Val::Str(s) => s.as_str().map_or_else(|| "string".to_owned(), string_literal),
        Val::Bool(true) => "true".to_owned(),
        Val::Bool(false) => "false".to_owned(),
        Val::Null => "null".to_owned(),
        Val::Array(entries) => spell_array_entries(entries),
    }
}

/// A concrete array is order-witnessed (ADR-0062 §2): `is_list` is the exact
/// [`steins_domain::array_is_list`] answer; every field required, tail sealed.
fn spell_array_entries(entries: &[(Key, Val)]) -> String {
    let is_list = steins_domain::array_is_list(entries);
    let fields: Vec<(Key, bool, String)> =
        entries.iter().map(|(k, v)| (k.clone(), true, spell_val(v))).collect();
    spell_shape(is_list, false, &fields, &ShapeTail::Sealed)
}

/// Spell a group of string literals: a single value is its literal, a small
/// set (≤ [`CAP`]) a literal union, else the tightest refined-string keyword.
/// `None` for an empty group; docblock `*/`/newline safety runs earlier.
fn spell_string_literals(strings: &[&PhpStr]) -> Option<Vec<String>> {
    if strings.is_empty() {
        return None;
    }
    let mut distinct: Vec<&PhpStr> = strings.to_vec();
    distinct.sort_unstable();
    distinct.dedup();

    // A byte string has no phpdoc literal spelling (ADR-0080 §2.5), so a group
    // carrying one skips the literal arm and widens to the predicate keyword.
    if distinct.len() <= CAP && distinct.iter().all(|s| s.is_utf8()) {
        return Some(distinct.iter().filter_map(|s| s.as_str()).map(string_literal).collect());
    }

    // Above CAP: widen to the shared implication-closed predicate summary.
    let mut preds = StrPreds::of(distinct[0]);
    for s in &distinct[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    Some(vec![preds_keyword(preds)])
}

/// PHPStan's explicit `int<lo, hi>` form, with `min`/`max` for domain ends.
/// `positive-int`/`non-negative-int`/`negative-int` are input-only sugar
/// (issue #90 — PHPStan always spells a range as the interval). The dump
/// surface's value-fact renderer calls this too, so the paths can't drift.
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

/// The tightest refined-string keyword a predicate summary admits: the
/// **closed grid** `core × casing` (issue #240). core ∈ {—, `non-empty-`,
/// `non-falsy-`, `numeric-`, `non-falsy-numeric-`} (own rung: `NUMERIC`
/// doesn't entail `NON_FALSY`); casing ∈ {—, `lowercase-`, `uppercase-`,
/// `uncased-`} (`uncased-` = `LOWERCASE ∧ UPPERCASE`).
///
/// One keyword, never an intersection (ADR-0030): every cell round-trips
/// through [`crate::lower_identifier`]; replaced a ladder that ranked axes
/// and dropped the loser (#235 probe). The array-key-cast pair is
/// deliberately **not** an axis (#240): adding it would silently re-spell
/// every value-derived decimal set via `StrPreds::of`, so it widens away — a
/// widening, never a lie. Casing still reads through `DECIMAL_INT`'s
/// closure, so a decimal set spells `numeric-uncased-string`.
#[must_use]
pub fn preds_keyword(preds: StrPreds) -> String {
    // `class-string` outranks every core rung (only *contextual* predicate,
    // issue #236) — the entailed `non-falsy-string` would drop the claim.
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

/// Single-quoted phpdoc literal, escaping `\` and `'` per PHP syntax
/// (round-tripped through `steins_phpdoc::parse_type` in honesty tests).
/// Terminal-safe by construction.
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

    /// Spell summarized arms — the dump/annotate emitters' path (summarize → spell).
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

    /// Terminal has no `*/` hazard: a `*/`-bearing literal spells verbatim here.
    #[test]
    fn star_slash_literal_is_spelled_verbatim_in_terminal() {
        assert_eq!(spell_vals(&[s("a*/b")]).unwrap(), "'a*/b'");
    }

    #[test]
    fn escaping_quotes_and_backslashes() {
        assert_eq!(string_literal("a'b"), "'a\\'b'");
        assert_eq!(string_literal("c\\d"), "'c\\\\d'");
    }

    /// Above CAP, literals widen stating BOTH axes (issue #240): `'k0'…'k8'`
    /// are all lowercase, so casing is part of the answer.
    #[test]
    fn over_cap_widens_to_keyword() {
        let vals: Vec<Val> = (0..=CAP as i64).map(|n| s(&format!("k{n}"))).collect();
        assert_eq!(spell_vals(&vals).unwrap(), "non-falsy-lowercase-string");
    }

    /// Mixed casing keeps the bare core rung: `intersect` drops both bits.
    #[test]
    fn over_cap_mixed_casing_widens_to_the_core_rung() {
        let vals: Vec<Val> =
            (0..=CAP as i64).map(|n| s(&format!("{}{n}", if n % 2 == 0 { "k" } else { "K" }))).collect();
        assert_eq!(spell_vals(&vals).unwrap(), "non-falsy-string");
    }
}

/// ADR-0062 §6 — array vocabulary `spell_arms` renders, plus [`spell_val`].
#[cfg(test)]
mod array_vocabulary_tests {
    use super::*;
    use crate::lower_str;

    /// Round-trip a phpdoc array type through `lower` then `spell_arms`.
    fn spell_ty(src: &str) -> String {
        let ty = lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower"));
        spell_arms(std::slice::from_ref(&ty)).unwrap_or_else(|| panic!("{src} did not spell"))
    }

    /// The denotational `is_list` `spell_contract_shape` spells from.
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
        // Issue #163's self-check: the head keyword claims `is_list`, so
        // re-parsing must reproduce it (old #159 rule failed on `list{A, B}`).
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
        // #51 fixture: a seeded array param spells rather than refuses.
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
        // Census bucket ix: round-trips the `not_list` `MapOf` flag.
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
        // `array<V>` lowers its key to `array-key`; speller collapses back to
        // the terse form.
        assert_eq!(spell_ty("array<int>"), "array<int>");
    }

    #[test]
    fn a_required_key_absorbs_the_non_empty_modifier_on_a_sealed_shape() {
        // Issue #159: `a` required proves non-emptiness, so the modifier drops.
        assert_eq!(spell_ty("non-empty-array{a: int}"), "array{a: int}");
    }

    #[test]
    fn a_wholly_optional_sealed_shape_keeps_the_non_empty_modifier() {
        // Exception: no required key means `[]` is admissible, so the modifier stays.
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
        // Issue #163: `array{0: int, 1: string}` is a key SET (`is_list =
        // Maybe`); `list{…}` promises a key SEQUENCE (`Yes`).
        assert_eq!(spell_ty("list{int, string}"), "list{int, string}");
        assert_eq!(spell_ty("array{0: int, 1: string}"), "array{int, string}");
    }

    #[test]
    fn a_single_key_zero_shape_is_a_sequence_however_it_is_declared() {
        // At most key `0` can appear, so `compute_is_list` = `Yes` without a
        // declaration; making the field optional prints its key.
        assert_eq!(spell_ty("array{0: int}"), "list{int}");
        assert_eq!(spell_ty("array{0?: int}"), "list{0?: int}");
    }

    #[test]
    fn two_optional_keys_keep_the_list_word_from_the_fact_not_a_carve_out() {
        // Issue #159 special-cased gapped keys `{0, 1?, 2?}`; #163 removed the
        // carve-out since `is_list` was always the real reason (row unchanged).
        assert_eq!(
            spell_ty("list{int, 1?: string, 2?: int}"),
            "list{0: int, 1?: string, 2?: int}"
        );
        // Same key structure WITHOUT the declaration is only `Maybe`.
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
        // Issue #159: positional form is all-or-nothing; a bare leading value
        // in `array{int, 2: string}` would misname the non-contiguous keys.
        assert_eq!(spell_ty("array{0: int, 2: string}"), "array{0: int, 2: string}");
    }

    #[test]
    fn an_unsealed_shape_is_spelled_exactly_as_before() {
        // Issue #159 is scoped to SEALED shapes; unsealed tails keep
        // `non-empty-`/`list` since the braces don't show all keys.
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
        // Sealed: drops the implied `non-empty-`, keeps `list` (issue #163).
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
        // array_is_list([]) is vacuously true (§3); `array{}` already says it.
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
        // Order-witnessed (§2): `array_is_list` answers exactly, spelled
        // `list{…}` (issue #163).
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
        // Order-witnessed (§2): insertion order 1,0 is NOT array_is_list, so
        // both fields print their key (never sorted).
        assert_eq!(
            spell_val(&av(vec![
                (Key::Int(1), Val::Str("a".into())),
                (Key::Int(0), Val::Str("b".into())),
            ])),
            "array{1: 'a', 0: 'b'}"
        );
    }
}
