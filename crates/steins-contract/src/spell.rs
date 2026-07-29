//! Plain-text spelling of a summarized contract-arm list (ADR-0053 §7 / ADR-0052
//! §4, slice D2).
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

use steins_domain::{Base, Certainty, IntRange, Key, StrPreds, Val, CAP};

use crate::{is_array_key_ty, shape_is_list, CField, CKey, ContractTy, MixedCut};

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
    // int members collapse to `Base(Int)`, so these buckets stay empty there — the
    // shared speller is unchanged for the docblock renderer, extended for the
    // contract-arm dump surface (ADR-0052 §9).
    let mut int_ranges: Vec<String> = Vec::new();
    let mut int_lits: Vec<i64> = Vec::new();
    let mut float_lits: Vec<f64> = Vec::new();
    // The string portion: a summarized set hands us either the numeric-string class
    // (one `StrWith` arm) or the distinct-sorted literal arms — never both.
    let mut string_keyword: Option<String> = None;
    let mut string_lits: Vec<&str> = Vec::new();
    // Array-vocabulary arms (ADR-0062 §6, D4): spelled in encounter order and
    // appended after the scalar members — the one speller extended, not a second
    // renderer. An array arm never blocks the scalar members around it (a
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
        // A `null`-only set spells `null`; a set with scalar members appends it.
        members.push("null".to_owned());
    }
    members.extend(array_members);

    if members.is_empty() { None } else { Some(members.join("|")) }
}

// ---------------------------------------------------------------------------
// Array vocabulary (ADR-0062 §6, D4) — the ONE speller extended, not a second
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

/// The shared spelling of the **generic** (fieldless) array vocabulary —
/// `array`, `non-empty-array`, `array<V>`, `array<K, V>`, `list<T>` — the
/// sibling of [`spell_shape`]'s brace assembly, and the ONE place that decides
/// it. Used by [`spell_array_arm`]'s degenerate arms and by the dump surface's
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

/// Spell a `Shape` arm through the shared D4 brace assembly ([`spell_shape`]):
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
fn spell_nested(ty: &ContractTy) -> String {
    match ty {
        ContractTy::Mixed => "mixed".to_owned(),
        ContractTy::Never => "never".to_owned(),
        ContractTy::Opaque => "mixed".to_owned(),
        ContractTy::MixedMinus(MixedCut::Null) => "non-null-mixed".to_owned(),
        ContractTy::MixedMinus(MixedCut::Falsy) => "non-empty-mixed".to_owned(),
        ContractTy::Class(name) => name.clone(),
        ContractTy::ObjectAny => "object".to_owned(),
        ContractTy::CallableTy(_) => "callable".to_owned(),
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

/// The shared D4 brace assembly (ADR-0062 §6): the ONE place that decides
/// `list{…}` vs `array{…}` and keyless-vs-keyed field spelling, used by both
/// the contract-arm path ([`spell_contract_shape`]) and the concrete-value
/// path ([`spell_val`]).
///
/// `is_list` is the caller's already-decided verdict (denotational
/// `Certainty::Yes`, per [`shape_is_list`], or the exact
/// [`steins_domain::array_is_list`] answer for a concrete array — never
/// recomputed here). `fields` are `(key, required, spelled value)` **in the
/// order they print** — this function does not reorder them, deliberately:
/// a declared shape has no real order and its caller canonicalizes by
/// sorting the key (mirroring [`steins_domain::ShapeFact::normalize`]'s own
/// field order, ADR-0062 §2's "contract lane is order-declared"), while a
/// concrete value's caller passes true insertion order (the value lane is
/// order-witnessed, §2 again — the one place order is sound to print). A
/// field spells **keyless** — a bare value, no key label — exactly when it
/// is required and its key is the next positional auto-index (PHP's own
/// shape-key rule, `shape_keys` in `lib.rs`, run in reverse); this is what
/// makes a fully-required `0..n-1`-keyed shape spell `array{T, U}` and a
/// `list{…}`-declared/forced shape spell `list{T, U}` with the SAME
/// per-field logic, differing only in the keyword.
#[must_use]
pub fn spell_shape(
    is_list: bool,
    non_empty: bool,
    fields: &[(Key, bool, String)],
    tail: &ShapeTail,
) -> String {
    let kw = non_empty_keyword(if is_list { "list" } else { "array" }, non_empty);

    let mut next_auto: i64 = 0;
    let mut parts: Vec<String> = Vec::with_capacity(fields.len() + 1);
    for (key, required, value) in fields {
        let keyless = *required && matches!(key, Key::Int(i) if *i == next_auto);
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
        ShapeTail::Typed { key: None, value } => parts.push(format!("...<{value}>")),
        ShapeTail::Typed { key: Some(k), value } => parts.push(format!("...<{k}, {value}>")),
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
        Key::Str(s) => if is_bare_ident(s) { s.clone() } else { string_literal(s) },
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
        Val::Str(s) => string_literal(s),
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
fn spell_string_literals(strings: &[&str]) -> Option<Vec<String>> {
    if strings.is_empty() {
        return None;
    }
    let mut distinct: Vec<&str> = strings.to_vec();
    distinct.sort_unstable();
    distinct.dedup();

    if distinct.len() <= CAP {
        // One value, or a small enum-like set: precise literal / literal union.
        return Some(distinct.iter().map(|s| string_literal(s)).collect());
    }

    // Larger than CAP: widen to the tightest predicate keyword the shared,
    // implication-closed predicate summary admits.
    let mut preds = StrPreds::of(distinct[0]);
    for s in &distinct[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    Some(vec![preds_keyword(preds)])
}

/// The tightest int-range keyword a phpdoc interval arm spells as: the three named
/// predicate classes, else the explicit `int<lo, hi>` interval (PHPStan's own
/// spelling, spaces after the comma). Mirrors the dump surface's own ladder so the
/// contract-arm renderer and the value-fact renderer agree.
fn int_range_keyword(r: IntRange) -> String {
    if r == IntRange::POSITIVE {
        "positive-int".to_owned()
    } else if r == IntRange::NEGATIVE {
        "negative-int".to_owned()
    } else if r == IntRange::NON_NEGATIVE {
        "non-negative-int".to_owned()
    } else {
        format!("int<{}, {}>", r.lo(), r.hi())
    }
}

/// Spell a float literal as PHPStan does: an integral value keeps a visible
/// fractional part (`5.0`, not `5`); every other value uses its shortest
/// round-tripping decimal (`3.14`).
fn float_literal(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 { format!("{f:.1}") } else { f.to_string() }
}

/// The tightest refined-string keyword a predicate summary admits (the keyword half
/// of the precision ladder). `numeric-string` ⊐ `non-falsy-string` ⊐
/// `non-empty-string` ⊐ `string`, with the casing pair spelled where PHPStan has a
/// **single** word for it: `lowercase-string` / `uppercase-string` and their
/// `non-empty-` intersections.
///
/// One keyword comes out, never an intersection (`non-falsy-string&lowercase-string`
/// is PHPStan's spelling for that set; this crate emits phpdoc that has to lower
/// back through [`crate::lower_identifier`], so it keeps to the named atoms). When a
/// set is nameable on both axes the core class wins and the casing half is dropped —
/// a *widening*, which is the safe direction for every consumer of a spelling, and
/// it keeps every casing-free set spelled exactly as before this pair existed.
/// `Lowercase ∧ Uppercase` (a string with no cased character) has no single keyword
/// either, so it falls through to the core ladder for the same reason.
///
/// The array-key-cast pair is deliberately **not** a rung. `decimal-int-string`
/// would be a legitimate one (it is tighter than `numeric-string`), but the
/// predicate is computed by `StrPreds::of` for every string value, so adding it
/// would silently re-spell every *value-derived* all-canonical-decimal set —
/// `'1'|'2'` widening to `decimal-int-string` rather than `numeric-string` — as
/// a side effect of teaching the checker a keyword. `non-decimal-int-string` is
/// not a rung for the mirror reason: nearly every string carries the bit, so it
/// says almost nothing about a set. Both therefore widen away here, which is the
/// same safe direction the casing half already takes. A declared
/// `decimal-int-string` consequently round-trips as `numeric-string` — a
/// widening, never a lie.
#[must_use]
pub fn preds_keyword(preds: StrPreds) -> String {
    let casing = match (
        preds.contains_all(StrPreds::LOWERCASE),
        preds.contains_all(StrPreds::UPPERCASE),
    ) {
        (true, false) => Some("lowercase"),
        (false, true) => Some("uppercase"),
        _ => None,
    };
    if preds.contains_all(StrPreds::NUMERIC) {
        "numeric-string".to_owned()
    } else if preds.contains_all(StrPreds::NON_FALSY) {
        "non-falsy-string".to_owned()
    } else if let Some(c) = casing {
        if preds.contains_all(StrPreds::NON_EMPTY) {
            format!("non-empty-{c}-string")
        } else {
            format!("{c}-string")
        }
    } else if preds.contains_all(StrPreds::NON_EMPTY) {
        "non-empty-string".to_owned()
    } else {
        "string".to_owned()
    }
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
        Val::Str(v.to_owned())
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
    /// it). This is the deliberate divergence D2 introduces.
    #[test]
    fn star_slash_literal_is_spelled_verbatim_in_terminal() {
        assert_eq!(spell_vals(&[s("a*/b")]).unwrap(), "'a*/b'");
    }

    #[test]
    fn escaping_quotes_and_backslashes() {
        assert_eq!(string_literal("a'b"), "'a\\'b'");
        assert_eq!(string_literal("c\\d"), "'c\\\\d'");
    }

    /// Above CAP distinct literals widen to the tightest keyword.
    #[test]
    fn over_cap_widens_to_keyword() {
        let vals: Vec<Val> = (0..=CAP as i64).map(|n| s(&format!("k{n}"))).collect();
        assert_eq!(spell_vals(&vals).unwrap(), "non-falsy-string");
    }
}

/// ADR-0062 §6 (D4) — the array vocabulary `spell_arms` learns in this slice,
/// plus its concrete-value counterpart [`spell_val`].
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

    #[test]
    fn seeded_optional_shape_spells_instead_of_refusing() {
        // The #51 L1 fixture: a seeded array param no longer refuses (the "no
        // declared contract" flip lives on the steins-infer dump surface; this
        // pins the underlying spelling it now has to spell from).
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
    fn non_empty_shape_spells_as_declared() {
        assert_eq!(spell_ty("non-empty-array{a: int}"), "non-empty-array{a: int}");
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
    fn declared_list_shape_keeps_the_list_keyword_even_though_two_sequential_required_keys_denote_maybe() {
        // D4: a `list{…}`-declared shape forces is_list Yes (A-G1, mirrored by
        // `shape_is_list`'s `given` seed) even though the same two-field key
        // structure denotes Maybe on its own (see the next test) — the keyword
        // tracks the DECLARATION's promise, not just the bare key structure.
        assert_eq!(spell_ty("list{int, string}"), "list{int, string}");
    }

    #[test]
    fn maybe_list_with_sequential_keys_spells_keyless_array_not_list() {
        // The D4 divergence point: `array{0: int, 1: string}` is a Maybe-list
        // (two realizable insertion orders, §3) — NOT forced Yes — so it spells
        // the order-agnostic keyless `array{…}`, which round-trips back to the
        // same Maybe verdict (re-lowering `array{int, string}` gives the same
        // two required sequential keys). Spelling it `list{…}` would be a LIE
        // (it would assert a fixed order the shape does not promise).
        assert_eq!(spell_ty("array{0: int, 1: string}"), "array{int, string}");
    }

    #[test]
    fn single_zero_field_is_denotationally_a_list_even_when_declared_with_array() {
        // A single field at key 0 (required or optional), sealed, is Yes-list
        // by the domain's own computation regardless of the `array{}` keyword
        // the user wrote — D4 spells the canonical answer, not the source
        // spelling.
        assert_eq!(spell_ty("array{0: int}"), "list{int}");
        assert_eq!(spell_ty("array{0?: int}"), "list{0?: int}");
    }

    #[test]
    fn stringly_keys_spell_keyed_array() {
        assert_eq!(spell_ty("array{a: int, b: string}"), "array{a: int, b: string}");
    }

    #[test]
    fn a_gap_only_keys_the_field_that_breaks_the_run() {
        // Key 0 is still the next auto-index at its position, so it spells
        // keyless even though the whole shape is No-list (the gap at 1 makes
        // no admitted value a list) — the keyless shortcut is a per-field rule
        // (`key == next_auto && required`), not an all-or-nothing shape verdict.
        assert_eq!(spell_ty("array{0: int, 2: string}"), "array{int, 2: string}");
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
    fn empty_array_value_is_a_list() {
        // array_is_list([]) is vacuously true (§3) — the D4-native spelling,
        // deliberately diverging from PHPStan stable's `array{}` for the empty
        // case (ADR-0062 §6, the nsrt-measured divergence).
        assert_eq!(spell_val(&av(vec![])), "list{}");
    }

    #[test]
    fn keyed_string_map_value_spells_array() {
        assert_eq!(
            spell_val(&av(vec![(Key::Str("a".to_owned()), Val::Str("v".to_owned()))])),
            "array{a: 'v'}"
        );
    }

    #[test]
    fn sequential_list_value_spells_list() {
        assert_eq!(
            spell_val(&av(vec![
                (Key::Int(0), Val::Str("x".to_owned())),
                (Key::Int(1), Val::Str("y".to_owned())),
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
    fn out_of_order_int_keys_are_not_a_list_but_still_print_keyless() {
        // Order-witnessed (§2): [1 => 'a', 0 => 'b'] is NOT array_is_list (the
        // keys are not 0..n-1 IN INSERTION ORDER), so the keyword is `array`, not
        // `list` — but the key SET is still exactly {0, 1}, and concrete values
        // print in their real insertion order (never sorted), so the first entry
        // (key 1) is not eligible for the keyless slot (next_auto starts at 0);
        // the second entry (key 0) is required but out of position too, so both
        // print keyed. This is the Maybe-with-non-sequential-encounter-order
        // case: the keyless shortcut is an encounter-order optimization, not a
        // promise that every list-shaped key set collapses to it.
        assert_eq!(
            spell_val(&av(vec![
                (Key::Int(1), Val::Str("a".to_owned())),
                (Key::Int(0), Val::Str("b".to_owned())),
            ])),
            "array{1: 'a', 0: 'b'}"
        );
    }
}
