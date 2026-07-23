//! Shared machinery for the two phpdoc transforms — promotion (`promote`) and
//! honesty repair (`honesty`).
//!
//! Both transforms enumerate free-function sites, prove facts against the same
//! reverse call-site sweep (`steins_infer::promote::sweep_free_functions`), and
//! speak the same first-class [`Refusal`](crate::transform::Refusal) vocabulary
//! for the obstacles that make "all callers" unknowable. This module factors the
//! pieces they genuinely share so neither forks the other (ADR-0034):
//!
//! - the four project-wide caller-enumerability refusal reasons plus
//!   `argument-not-proven` (the reasons a *reverse sweep* can raise);
//! - the `has_source_hint` / `arg_to_val` / native-contract helpers;
//! - the value-domain → ADR-0029 phpdoc **type rendering** honesty uses to spell
//!   a proven value set, and promotion re-uses nothing of (kept here so the
//!   grammar-rendering policy lives in one place).

use std::collections::HashMap;

use steins_contract::normalize::summarize_vals;
use steins_contract::{ContractTy, admits_val};
use steins_db::{Db, SourceFile, parse};
use steins_domain::{Base, Certainty, Key, StrPreds, Val, CAP};
use steins_infer::promote::{FreeFnSweep, MethodSweep};
use steins_syntax::{
    ArgValue, ClassDecl, FunctionDecl, MethodDecl, NativeType, NormKey, Param, ScalarType,
    SourceTree, TypeMember, normalize_array,
};

use crate::transform::SiteRef;

// ---- Shared refusal reason names (ADR-0034 point 2) -----------------------
//
// These are the reasons a reverse call-site sweep raises — the obstacles that
// make "every caller is accounted for" unknowable. Both promotion and honesty's
// `@param` widening share them verbatim; `promote` re-exports them so its public
// `steins_edit::promote::REASON_*` paths keep resolving.

/// A dynamic `$fn(...)` call exists in the project — it could target any free
/// function, so no free-function candidate can prove all its callers.
pub const REASON_DYNAMIC_CALL: &str = "dynamic-call-present";
/// The function's name appears as a string / first-class-callable value (a
/// `call_user_func`-style caller invisible to call resolution).
pub const REASON_REFERENCED_AS_VALUE: &str = "function-referenced-as-value";
/// The function's name does not resolve uniquely project-wide (duplicate
/// definition or builtin shadow), so its callers cannot be enumerated soundly.
pub const REASON_AMBIGUOUS: &str = "resolution-ambiguous";
/// A call reaching this function used named or spread arguments (positional
/// mapping is unreliable).
pub const REASON_NAMED_OR_SPREAD: &str = "named-or-spread-args";
/// At least one relevant call-site argument is not a proven literal.
pub const REASON_ARG_NOT_PROVEN: &str = "argument-not-proven";
/// A non-vendor project file contains an `eval(...)` — code as data can call any
/// free function with no CST call site (ADR-0046 §2), so "all callers proven" is
/// unknowable project-wide. A project-global obstacle: every candidate refuses.
pub const REASON_EVAL_PRESENT: &str = "eval-present";
/// A non-vendor `include`/`require` whose path is unproven, or a proven literal
/// that does not resolve inside the analyzed universe (ADR-0046 §2) — out-of-
/// universe code (compiled-template caches) can define/call anything. A project-
/// global obstacle: every candidate refuses.
pub const REASON_DYNAMIC_INCLUDE: &str = "dynamic-include-present";
/// The candidate is a method that is inheritance-involved — overridable, overriding
/// an ancestor, abstract, an interface method, or in a class whose hierarchy is not
/// fully resolvable (parent unresolvable, or a trait `use` that merges methods).
/// Narrowing it could break Liskov substitution, so v1 refuses the whole method
/// (ADR-0041 §1 eligibility split / ADR-0043 §6).
pub const REASON_METHOD_INHERITANCE: &str = "method-inheritance";
/// The candidate is a magic method (`__construct`, `__wakeup`, `__toString`, any
/// `__*`): a magic method is invoked by the runtime with no ordinary call site
/// (and `__wakeup`/`__unserialize` by any `unserialize`), so it is never a
/// promotion/honesty candidate (ADR-0046 §3).
pub const REASON_MAGIC_METHOD: &str = "magic-method";
/// A **promotion** candidate whose enumerated caller set is empty: no call site
/// anywhere in the analyzed universe resolved to this function/method. "All
/// callers proven" over zero callers is vacuously true but carries zero evidence
/// — it cannot enter the verified stratum (ADR-0037), and it is exactly the hole
/// a framework's convention-reflection dispatch opens (a test runner invoking a
/// data-provider method with no visible call site, ADR-0047 §4; amends the
/// ADR-0041 §3 taxonomy). Honesty never reaches this: its own "lie" enumeration
/// requires an observed violating value, so it cannot act on empty evidence by
/// construction — this reason is promotion-only.
pub const REASON_NO_OBSERVED_CALLERS: &str = "no-observed-callers";

// ---- Candidate / call-site helpers ----------------------------------------

/// Count each FQN across the project so a duplicate definition (which makes
/// resolution ambiguous) refuses rather than acts on thin evidence.
#[must_use]
pub fn count_fqns(db: &dyn Db, files: &[SourceFile]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for &file in files {
        for f in parse(db, file).functions() {
            *counts.entry(f.fqn.clone()).or_default() += 1;
        }
    }
    counts
}

/// A [`SiteRef`] for a candidate parameter.
#[must_use]
pub fn param_site(path: &str, tree: &SourceTree, func: &FunctionDecl, param: &Param) -> SiteRef {
    let p = tree.position(param.span.start);
    SiteRef::new(
        path.to_owned(),
        p.line,
        p.column,
        format!("function {}() param ${}", func.name, param.name),
    )
}

/// A [`SiteRef`] for a function's `@return` site (anchored at the declaration).
#[must_use]
pub fn return_site(path: &str, tree: &SourceTree, func: &FunctionDecl) -> SiteRef {
    let p = tree.position(func.span.start);
    SiteRef::new(path.to_owned(), p.line, p.column, format!("function {}() @return", func.name))
}

/// A [`SiteRef`] for a candidate **method** parameter (ADR-0043 §6).
#[must_use]
pub fn method_param_site(
    path: &str,
    tree: &SourceTree,
    class: &ClassDecl,
    method: &MethodDecl,
    param: &Param,
) -> SiteRef {
    let p = tree.position(param.span.start);
    SiteRef::new(
        path.to_owned(),
        p.line,
        p.column,
        format!("method {}::{}() param ${}", class.name, method.name, param.name),
    )
}

/// A [`SiteRef`] for a candidate method's `@return` site (anchored at the method
/// name identifier).
#[must_use]
pub fn method_return_site(
    path: &str,
    tree: &SourceTree,
    class: &ClassDecl,
    method: &MethodDecl,
) -> SiteRef {
    let p = tree.position(method.span.start);
    SiteRef::new(
        path.to_owned(),
        p.line,
        p.column,
        format!("method {}::{}() @return", class.name, method.name),
    )
}

/// The project-wide obstacles that make "all callers proven" unknowable for a
/// **method** target of name `method_name` (shared by method promotion and method
/// `@param` honesty; ADR-0043 §6). `Ok(())` when the method's callers are
/// enumerable; otherwise a named refusal.
///
/// The per-target `named-or-spread-args` obstacle is not here — it is a fact of one
/// target's observed calls, checked where the observed args are proven.
pub fn check_method_caller_enumerability(
    method_name: &str,
    sweep: &MethodSweep,
) -> Result<(), (&'static str, String)> {
    // Non-empty site list == the old `any_dynamic_method == true` (ADR-0047 §6).
    if !sweep.dynamic_method_sites.is_empty() {
        return Err((
            REASON_DYNAMIC_CALL,
            "a dynamic method call (`$o->$m()`) in the project could target this method".to_owned(),
        ));
    }
    let name = method_name.to_ascii_lowercase();
    // Key-presence == the old set membership.
    if sweep.value_referenced_methods.contains_key(&name) {
        return Err((
            REASON_REFERENCED_AS_VALUE,
            format!("`{method_name}` appears as a callable string / callable-array value"),
        ));
    }
    if let Some(sites) = sweep.unresolved_method_names.get(&name) {
        // The first recorded site is the representative (source order) — identical to
        // the pre-Slice-B single-site value.
        let site = &sites[0];
        return Err((
            REASON_AMBIGUOUS,
            format!(
                "a `->{method_name}()` / `::{method_name}()` call at {}:{}:{} resolves to no unique method (unknown receiver class), so callers of every `{method_name}` are open",
                site.path, site.line, site.column
            ),
        ));
    }
    Ok(())
}

/// Whether the source text at `param.span.start` carries a native type hint.
///
/// `param.ty == None` alone is ambiguous: it also means a *complex* hint was
/// lowered away (`Foo|Bar $x`). So we inspect the raw bytes: from the parameter
/// start, skip whitespace and the `&` / `...` markers; if the next token is the
/// `$variable`, there is no hint.
#[must_use]
pub fn has_source_hint(source: &str, param: &Param) -> bool {
    let start = param.span.start as usize;
    let bytes = source.as_bytes();
    let mut k = start;
    loop {
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if bytes[k..].starts_with(b"...") {
            k += 3;
            continue;
        }
        if bytes.get(k) == Some(&b'&') {
            k += 1;
            continue;
        }
        break;
    }
    bytes.get(k) != Some(&b'$')
}

/// Convert a lowered [`ArgValue`] to a concrete domain [`Val`], or `None` when it
/// is not a self-evident literal (a `$var`, a call, a `new`, a closure, …). Arrays
/// are literal iff every element is.
#[must_use]
pub fn arg_to_val(v: &ArgValue) -> Option<Val> {
    match v {
        ArgValue::Int(i) => Some(Val::Int(*i)),
        ArgValue::Float(f) => Some(Val::Float(*f)),
        ArgValue::Str(s) => Some(Val::Str(s.clone())),
        ArgValue::Bool(b) => Some(Val::Bool(*b)),
        ArgValue::Null => Some(Val::Null),
        ArgValue::Array(items) => {
            let normalized = normalize_array(items);
            let mut out = Vec::with_capacity(normalized.len());
            for (k, e) in normalized {
                out.push((norm_key(&k), arg_to_val(&e)?));
            }
            Some(Val::Array(out))
        }
        _ => None,
    }
}

fn norm_key(k: &NormKey) -> Key {
    match k {
        NormKey::Int(i) => Key::Int(*i),
        NormKey::Str(s) => Key::Str(s.clone()),
    }
}

/// Build the acceptance contract for a **native** type (native semantics, not
/// phpdoc lowering): scalars → base, `true`/`false` → bool-literal, nullable adds
/// `null`.
#[must_use]
pub fn native_contract(nt: &NativeType) -> ContractTy {
    let mut members: Vec<ContractTy> = nt
        .members
        .iter()
        .map(|m| match m {
            TypeMember::Scalar(ScalarType::Int) => ContractTy::Base(Base::Int),
            TypeMember::Scalar(ScalarType::Float) => ContractTy::Base(Base::Float),
            TypeMember::Scalar(ScalarType::String) => ContractTy::Base(Base::String),
            TypeMember::Scalar(ScalarType::Bool) => ContractTy::Base(Base::Bool),
            TypeMember::BoolLiteral(b) => ContractTy::LitBool(*b),
            // Object member (ADR-0043): the class contract. Callers that could feed
            // an `Instance`-bearing type here guard it out first (the native-guard
            // scalar domain), so this arm is exercised only once the object-world
            // acceptance path opens; it is the honest lowering regardless.
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
        })
        .collect();
    if nt.nullable {
        members.push(ContractTy::Null);
    }
    if members.len() == 1 {
        members.pop().expect("non-empty")
    } else {
        ContractTy::Union(members)
    }
}

/// The project-wide obstacles that make "all callers proven" unknowable for a
/// free-function target (shared by promotion and `@param` honesty). `Ok(())` when
/// the callers of `func` are enumerable; otherwise a named refusal.
///
/// The per-target `named-or-spread-args` obstacle is *not* here — it is a fact of
/// one target's observed calls, checked where the observed args are proven.
pub fn check_caller_enumerability(
    func: &FunctionDecl,
    sweep: &FreeFnSweep,
    fqn_counts: &HashMap<String, usize>,
) -> Result<(), (&'static str, String)> {
    // Non-empty site list == the old `any_dynamic_call == true` (ADR-0047 §6).
    if !sweep.dynamic_call_sites.is_empty() {
        return Err((
            REASON_DYNAMIC_CALL,
            "a dynamic `$fn(...)` call in the project could target this function".to_owned(),
        ));
    }
    let simple = func.name.to_ascii_lowercase();
    // Key-presence == the old set membership.
    if sweep.value_referenced_names.contains_key(&simple)
        || sweep.value_referenced_names.contains_key(&func.fqn)
    {
        return Err((
            REASON_REFERENCED_AS_VALUE,
            format!("`{}` appears as a string / first-class-callable value", func.name),
        ));
    }
    if fqn_counts.get(&func.fqn).copied().unwrap_or(0) > 1
        || sweep.unresolved_simple_names.contains_key(&simple)
    {
        return Err((
            REASON_AMBIGUOUS,
            format!("`{}` does not resolve uniquely project-wide", func.name),
        ));
    }
    Ok(())
}

// ---- Value-domain → phpdoc type rendering (ADR-0029) ----------------------
//
// The *semantic* normal form — sort/dedup, the predicate-class collapse (numeric
// literals → numeric-string, the bool pair → bool, null-fold) — lives in
// `steins_contract::normalize::summarize_vals` (ADR-0052 §4, slice N1). What
// stays here is **rendering policy**: docblock literal-safety (`*/`, raw
// newlines), the CAP-bounded literal-union spelling decision, quoting/escaping,
// and member spelling order. The cut is byte-identical against the honesty
// tests below (the renderer's oracle).

/// Render a proven set of concrete values as a faithful phpdoc type (ADR-0029
/// grammar), or `None` when no faithful spelling exists (`type-not-renderable`).
///
/// The set is normalized by [`summarize_vals`] (subsumed members collapse;
/// duplicates removed) into an arm list; this function spells those arms.
/// Integer values render as `int`, string values as literal unions (`'a'|'b'`)
/// or a refined-string keyword (`numeric-string`, `non-falsy-string`,
/// `non-empty-string`) when a single predicate class captures them — but never
/// over-widens: an array-bearing set (no faithful scalar spelling) refuses.
/// Members are emitted in a stable order (int, float, string(s), bool, then
/// `null`).
#[must_use]
pub fn render_value_domain(vals: &[Val]) -> Option<String> {
    let arms = summarize_vals(vals)?;

    let mut has_int = false;
    let mut has_float = false;
    let mut bool_member: Option<&'static str> = None;
    let mut nullable = false;
    // The string portion: `summarize_vals` hands us either the numeric-string
    // class (one `StrWith` arm) or the distinct-sorted literal arms — never both.
    let mut string_keyword: Option<String> = None;
    let mut string_lits: Vec<&str> = Vec::new();
    for arm in &arms {
        match arm {
            ContractTy::Base(Base::Int) => has_int = true,
            ContractTy::Base(Base::Float) => has_float = true,
            ContractTy::Base(Base::Bool) => bool_member = Some("bool"),
            ContractTy::LitBool(true) => bool_member = Some("true"),
            ContractTy::LitBool(false) => bool_member = Some("false"),
            ContractTy::Null => nullable = true,
            // A pre-collapsed predicate class (the numeric-string arm).
            ContractTy::StrWith(p) => string_keyword = Some(preds_keyword(*p)),
            ContractTy::Base(Base::String) => string_keyword = Some("string".to_owned()),
            ContractTy::LitStr(s) => string_lits.push(s),
            // `summarize_vals` produces no other arm shapes from a value set.
            other => debug_assert!(false, "unexpected summarized arm: {other:?}"),
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
        // A `null`-only proof spells `null`; a set with scalar members appends it.
        members.push("null".to_owned());
    }

    // `summarize_vals` returns `Some` only for a non-empty scalar/null-bearing
    // set, so `members` is always non-empty here.
    Some(members.join("|"))
}

/// Spell a group of distinct string literals as phpdoc — the rendering policy
/// half of the string ladder (the numeric-string collapse already happened in
/// [`summarize_vals`]). A single docblock-safe value is its literal; a small
/// safe set is a literal union; a value that cannot be embedded in a docblock
/// (`*/` or a raw newline — [`docblock_literal_safe`]) or a set larger than
/// [`CAP`] widens to the tightest refined-string keyword its shared predicate
/// summary admits. `None` for an empty group (no string members).
fn spell_string_literals(strings: &[&str]) -> Option<Vec<String>> {
    if strings.is_empty() {
        return None;
    }
    let mut distinct: Vec<&str> = strings.to_vec();
    distinct.sort_unstable();
    distinct.dedup();

    // A single-quoted literal is only faithful when every value can be embedded in
    // a docblock without corrupting it: no `*/` (which closes the enclosing
    // `/** … */`) and no raw newline (which the phpdoc lexer rejects in a quoted
    // literal). PHP single-quote escaping cannot represent either, so a value that
    // carries one has no literal spelling — it must widen to a keyword instead.
    let all_literal_safe = distinct.iter().all(|s| docblock_literal_safe(s));

    if all_literal_safe && distinct.len() == 1 {
        // One safe observed value: its literal is the honest, tightest spelling.
        return Some(vec![string_literal(distinct[0])]);
    }
    if all_literal_safe && distinct.len() <= CAP {
        // A small enum-like set of docblock-safe values: a literal union is precise
        // and faithful.
        return Some(distinct.iter().map(|s| string_literal(s)).collect());
    }

    // Unsafe to embed, or larger than CAP: widen to the tightest predicate keyword
    // the shared, implication-closed predicate summary admits. Numeric widening
    // (`numeric-string`) still applies — it is a keyword, never an embedded
    // literal, and admits the value via the same `is_numeric` classifier (the
    // single-unsafe-numeric case, e.g. `"5\n"`).
    let mut preds = StrPreds::of(distinct[0]);
    for s in &distinct[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    Some(vec![preds_keyword(preds)])
}

/// The tightest refined-string keyword a predicate summary admits (the keyword
/// half of the precision ladder). `numeric-string` ⊐ `non-falsy-string` ⊐
/// `non-empty-string` ⊐ `string`.
fn preds_keyword(preds: StrPreds) -> String {
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

/// Whether a string can be spelled as a single-quoted phpdoc literal *inside a
/// docblock* without corrupting it. Two byte sequences have no representation in a
/// `/** … */` block and no single-quote escape can encode them:
/// - `*/` closes the block comment early (a hard PHP parse error at the callsite);
/// - a raw newline / carriage return, which the phpdoc lexer rejects in a quoted
///   literal (it would also split the tag across physical lines).
///
/// A value carrying either has no faithful literal spelling and must widen to a
/// keyword. (`\` and `'` themselves are handled by [`string_literal`]'s escaping.)
fn docblock_literal_safe(s: &str) -> bool {
    !s.contains("*/") && !s.contains('\n') && !s.contains('\r')
}

/// Render one PHP string as a single-quoted phpdoc literal, escaping `\` and `'`
/// exactly as PHP single-quoted syntax requires (round-tripped through
/// [`steins_phpdoc::parse_type`] in the honesty tests).
fn string_literal(s: &str) -> String {
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

/// Whether `contract` admits *every* value in `vals` with [`Certainty::Yes`] — the
/// "the type faithfully covers the proof" test the native-contradiction guard and
/// the prefixed/plain reconciliation both use.
#[must_use]
pub fn admits_all(contract: &ContractTy, vals: &[Val]) -> bool {
    vals.iter().all(|v| admits_val(contract, v) == Certainty::Yes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.to_owned())
    }

    fn round_trips(rendered: &str) {
        let parsed =
            steins_phpdoc::parse_type(rendered).unwrap_or_else(|e| panic!("`{rendered}`: {e}"));
        assert!(parsed.at_end, "`{rendered}` did not fully parse");
    }

    #[test]
    fn int_and_numeric_strings_render_the_canonical_union() {
        // ADR-0037 canonical: int + numeric-string callers → int|numeric-string.
        let r = render_value_domain(&[i(1), s("12"), s("34")]).unwrap();
        assert_eq!(r, "int|numeric-string");
        round_trips(&r);
    }

    #[test]
    fn single_string_is_its_literal() {
        let r = render_value_domain(&[s("123")]).unwrap();
        assert_eq!(r, "'123'");
        round_trips(&r);
    }

    #[test]
    fn enum_like_strings_render_a_literal_union() {
        let r = render_value_domain(&[s("POST"), s("GET"), s("GET")]).unwrap();
        // Distinct + sorted; not numeric → literal union.
        assert_eq!(r, "'GET'|'POST'");
        round_trips(&r);
    }

    #[test]
    fn dedup_collapses_repeated_values() {
        let r = render_value_domain(&[i(1), i(2), i(1)]).unwrap();
        assert_eq!(r, "int");
        round_trips(&r);
    }

    #[test]
    fn nullable_appends_null() {
        let r = render_value_domain(&[i(1), Val::Null]).unwrap();
        assert_eq!(r, "int|null");
        round_trips(&r);
    }

    #[test]
    fn bool_pair_is_bool_single_is_literal() {
        assert_eq!(render_value_domain(&[Val::Bool(true), Val::Bool(false)]).unwrap(), "bool");
        assert_eq!(render_value_domain(&[Val::Bool(true)]).unwrap(), "true");
    }

    #[test]
    fn array_bearing_set_is_not_renderable() {
        assert_eq!(render_value_domain(&[Val::Array(vec![])]), None);
    }

    #[test]
    fn literal_escaping_round_trips() {
        let r = render_value_domain(&[s("a'b"), s("c\\d")]).unwrap();
        round_trips(&r);
    }

    /// A string bearing the docblock terminator `*/` has no faithful single-quoted
    /// spelling (it would close the enclosing `/** … */`), so it must widen to a
    /// keyword rather than render a corrupting literal.
    #[test]
    fn star_slash_string_never_renders_a_literal() {
        let r = render_value_domain(&[s("a*/b")]).unwrap();
        assert!(!r.contains("*/"), "rendered `{r}` still carries the docblock terminator");
        assert!(!r.contains('\''), "rendered `{r}` is a corrupting literal, not a keyword");
        assert_eq!(r, "non-falsy-string");
        round_trips(&r);
    }

    /// The literal-union path (multiple distinct values ≤ CAP) is equally unsafe:
    /// one `*/`-bearing member must force the whole group to a keyword.
    #[test]
    fn star_slash_in_a_union_forces_a_keyword() {
        let r = render_value_domain(&[s("ok"), s("a*/b")]).unwrap();
        assert!(!r.contains("*/"), "rendered `{r}` still carries the docblock terminator");
        assert_eq!(r, "non-falsy-string");
        round_trips(&r);
    }

    /// A newline-bearing string cannot be a single-quoted phpdoc literal (the lexer
    /// rejects raw newlines) — it widens to a keyword.
    #[test]
    fn newline_string_never_renders_a_literal() {
        let r = render_value_domain(&[s("line1\nline2")]).unwrap();
        assert!(!r.contains('\n') && !r.contains('\''), "rendered `{r}` corrupts the tag line");
        assert_eq!(r, "non-falsy-string");
        round_trips(&r);
    }

    /// `php_is_numeric` trims newlines, so `"5\n"` is numeric yet newline-bearing:
    /// the single-value literal path would corrupt it — the numeric-string keyword
    /// catches it (and admits it, since admission also trims).
    #[test]
    fn newline_bearing_numeric_string_renders_the_keyword() {
        let r = render_value_domain(&[s("5\n")]).unwrap();
        assert_eq!(r, "numeric-string");
        assert!(!r.contains('\n') && !r.contains('\''));
        round_trips(&r);
    }
}
