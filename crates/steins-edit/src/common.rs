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
use steins_contract::spell::spell_arms;
use steins_contract::{ContractTy, admits_val};
use steins_db::{Db, SourceFile, parse};
use steins_domain::{Base, Certainty, Key, StrPreds, Val};
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
            // An object intersection lowers to the conjunctive contract.
            TypeMember::InstanceInter(cs) => {
                ContractTy::Inter(cs.iter().map(|c| ContractTy::Class(c.fqn.clone())).collect())
            }
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

// ---- Value-domain → phpdoc type rendering (ADR-0029 / ADR-0053 §7) ---------
//
// The *semantic* normal form — sort/dedup, the predicate-class collapse (numeric
// literals → numeric-string, the bool pair → bool, null-fold) — lives in
// `steins_contract::normalize::summarize_vals` (ADR-0052 §4, slice N1). The
// **plain-text arm spelling** — member ordering/join, the CAP-bounded literal-
// union decision, the predicate-keyword ladder, and single-quote escaping — now
// lives in `steins_contract::spell` (ADR-0053 §7, slice D2), shared with the
// `annotate`/dump emitters in `steins-infer` (which cannot reach this crate: the
// dependency runs steins-edit → steins-infer).
//
// What *stays* here is the **docblock armor**: the `*/`/raw-newline literal-safety
// widening that is meaningless in terminal output but corrupts a `/** … */` block.
// It pre-widens the arm list before delegating to the shared speller. The cut is
// byte-identical against the honesty tests below (the renderer's oracle) and the
// cross-crate parity test.

/// Render a proven set of concrete values as a faithful phpdoc type (ADR-0029
/// grammar) *safe to embed in a docblock*, or `None` when no faithful spelling
/// exists (`type-not-renderable`).
///
/// The set is normalized by [`summarize_vals`] into an arm list; the docblock
/// armor ([`docblock_widen_unsafe_literals`]) widens any literal group that cannot
/// be embedded in a `/** … */` block (`*/` or a raw newline) to the tightest
/// predicate keyword; then the shared [`spell_arms`] spells the (now docblock-safe)
/// arms — member ordering, the CAP-bounded literal-union decision, the keyword
/// ladder, and `\`/`'` escaping. Integer values render as `int`, string values as
/// literal unions (`'a'|'b'`) or a refined-string keyword; an array-bearing set
/// has no faithful scalar spelling and refuses.
#[must_use]
pub fn render_value_domain(vals: &[Val]) -> Option<String> {
    let mut arms = summarize_vals(vals)?;
    docblock_widen_unsafe_literals(&mut arms);
    spell_arms(&arms)
}

/// Docblock armor (ADR-0053 §7): if a `LitStr` group carries any value that cannot
/// be embedded in a `/** … */` block ([`docblock_literal_safe`]), replace the whole
/// group with the tightest predicate-keyword arm ([`ContractTy::StrWith`]) *before*
/// the shared speller runs. A single-quoted literal cannot represent `*/` (which
/// closes the block early) or a raw newline (the phpdoc lexer rejects it), so such
/// a value has no faithful literal spelling in a docblock and must widen.
///
/// A no-op when the group is all-safe (the shared speller then spells the literals)
/// or absent. This is the only docblock-specific transformation; everything else is
/// the shared terminal spelling. Terminal output has no such hazard, so the dump/
/// annotate emitters call [`spell_arms`] directly, skipping this.
fn docblock_widen_unsafe_literals(arms: &mut Vec<ContractTy>) {
    // `summarize_vals` yields the string group as either one `StrWith` arm (numeric
    // collapse) or distinct-sorted `LitStr` arms — never both.
    let lits: Vec<&str> = arms
        .iter()
        .filter_map(|a| if let ContractTy::LitStr(s) = a { Some(s.as_str()) } else { None })
        .collect();
    if lits.is_empty() || lits.iter().all(|s| docblock_literal_safe(s)) {
        return;
    }
    // The shared, implication-closed predicate summary of the group (the same
    // intersection the terminal keyword ladder would compute).
    let mut preds = StrPreds::of(lits[0]);
    for s in &lits[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    // Replace the (canonically contiguous) `LitStr` arms with one keyword arm at the
    // string slot, preserving the member order the speller re-imposes.
    let at = arms.iter().position(|a| matches!(a, ContractTy::LitStr(_))).expect("a LitStr arm");
    arms.retain(|a| !matches!(a, ContractTy::LitStr(_)));
    arms.insert(at, ContractTy::StrWith(preds));
}

/// Whether a string can be spelled as a single-quoted phpdoc literal *inside a
/// docblock* without corrupting it. Two byte sequences have no representation in a
/// `/** … */` block and no single-quote escape can encode them:
/// - `*/` closes the block comment early (a hard PHP parse error at the callsite);
/// - a raw newline / carriage return, which the phpdoc lexer rejects in a quoted
///   literal (it would also split the tag across physical lines).
///
/// A value carrying either has no faithful literal spelling and must widen to a
/// keyword. (`\` and `'` themselves are handled by the shared speller's escaping.)
fn docblock_literal_safe(s: &str) -> bool {
    !s.contains("*/") && !s.contains('\n') && !s.contains('\r')
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

    /// The **annotate parity** contract (ADR-0053 §7, slice D2): the shared
    /// `steins_contract::spell::spell_arms` — the one the `annotate`/dump emitters
    /// in `steins-infer` call, byte-for-byte — reproduces this crate's docblock
    /// renderer wherever the docblock armor is a no-op (every docblock-safe value
    /// set). The extraction seam is byte-identical, so a dump and an `annotate`
    /// margin for the same expression will spell the same fact the same way (the
    /// full same-position pin lands with the emitters at D3). Where a value is not
    /// docblock-safe (`*/` / raw newline), the two deliberately diverge — the
    /// docblock renderer widens, the terminal speller spells the literal — and that
    /// divergence is exactly the armor D2 keeps in `steins-edit`.
    #[test]
    fn shared_speller_is_byte_equal_to_the_docblock_renderer_on_safe_sets() {
        let safe_sets: Vec<Vec<Val>> = vec![
            vec![i(1), s("12"), s("34")],       // int|numeric-string
            vec![s("123")],                     // '123'
            vec![s("POST"), s("GET"), s("GET")], // 'GET'|'POST'
            vec![i(1), i(2), i(1)],             // int
            vec![i(1), Val::Null],              // int|null
            vec![Val::Bool(true), Val::Bool(false)], // bool
            vec![Val::Bool(true)],              // true
            vec![s("a'b"), s("c\\d")],          // escaped literal union
            vec![Val::Float(1.5), i(2)],        // float|int-ish
        ];
        for vals in &safe_sets {
            let docblock = render_value_domain(vals);
            let shared = summarize_vals(vals).and_then(|arms| spell_arms(&arms));
            assert_eq!(shared, docblock, "shared speller diverged from the renderer on {vals:?}");
        }

        // The array-bearing refusal is shared too: both return `None`.
        assert_eq!(render_value_domain(&[Val::Array(vec![])]), None);
        assert_eq!(summarize_vals(&[Val::Array(vec![])]).and_then(|a| spell_arms(&a)), None);

        // Documented divergence on a docblock-*unsafe* value: the renderer widens to
        // a keyword, the shared terminal speller spells the (escaped) literal.
        let unsafe_val = vec![s("a*/b")];
        assert_eq!(render_value_domain(&unsafe_val).unwrap(), "non-falsy-string");
        assert_eq!(
            summarize_vals(&unsafe_val).and_then(|a| spell_arms(&a)).unwrap(),
            "'a*/b'"
        );
    }
}
