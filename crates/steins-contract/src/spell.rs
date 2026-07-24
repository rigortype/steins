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
//! * `steins-edit`'s docblock renderer ([`render_value_domain`]), which re-layers
//!   its docblock **armor** on top: the `*/`/raw-newline literal-safety widening
//!   that is meaningless in terminal output but corrupts a `/** … */` block. That
//!   armor pre-widens the arm list, then delegates the member assembly, the CAP-
//!   bounded literal-union decision, the predicate-keyword ladder, and the
//!   single-quote escaping to [`spell_arms`] here.
//!
//! The cut is byte-identical against the honesty tests in `steins-edit` (the
//! renderer's oracle) and the cross-crate parity test there.

use steins_domain::{Base, StrPreds, CAP};

use crate::ContractTy;

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
    // The string portion: a summarized set hands us either the numeric-string class
    // (one `StrWith` arm) or the distinct-sorted literal arms — never both.
    let mut string_keyword: Option<String> = None;
    let mut string_lits: Vec<&str> = Vec::new();
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
            // Any other arm (an array, object, interval, class, …) has no faithful
            // plain-scalar spelling — the honest refusal, `type-not-renderable`.
            _ => return None,
        }
    }

    let mut members: Vec<String> = Vec::new();
    if has_int {
        members.push("int".to_owned());
    }
    if has_float {
        members.push("float".to_owned());
    }
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

    if members.is_empty() { None } else { Some(members.join("|")) }
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

/// The tightest refined-string keyword a predicate summary admits (the keyword half
/// of the precision ladder). `numeric-string` ⊐ `non-falsy-string` ⊐
/// `non-empty-string` ⊐ `string`.
#[must_use]
pub fn preds_keyword(preds: StrPreds) -> String {
    if preds.contains_all(StrPreds::NUMERIC) {
        "numeric-string".to_owned()
    } else if preds.contains_all(StrPreds::NON_FALSY) {
        "non-falsy-string".to_owned()
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
