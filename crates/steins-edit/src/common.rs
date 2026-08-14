//! Shared machinery for the two phpdoc transforms — promotion (`promote`) and
//! honesty repair (`honesty`). Both sweep the same reverse call-site data
//! (`steins_infer::promote::sweep_free_functions`) and speak the same
//! [`Refusal`](crate::transform::Refusal) vocabulary. Factored here so neither
//! forks the other (ADR-0034): the caller-enumerability refusal reasons, the
//! `has_source_hint` / `arg_to_val` / native-contract helpers, and the
//! value-domain → ADR-0029 phpdoc type rendering (honesty only).

use std::collections::HashMap;

use steins_contract::normalize::summarize_vals;
use steins_contract::spell::spell_arms;
use steins_contract::{ContractTy, admits_val};
use steins_db::{Db, SourceFile, parse};
use steins_domain::{Base, Certainty, Key, PhpStr, StrPreds, Val};
use steins_infer::promote::{FreeFnSweep, MethodSweep};
use steins_syntax::{
    ArgValue, ClassDecl, FunctionDecl, MethodDecl, NativeType, NormKey, Param, ScalarType,
    SourceTree, TypeMember, normalize_array,
};

use crate::transform::SiteRef;

// Shared refusal reason names (ADR-0034 point 2): raised by a reverse
// call-site sweep. `promote` re-exports these as `steins_edit::promote::REASON_*`.

/// A dynamic `$fn(...)` call could target any free function; no candidate proves all callers.
pub const REASON_DYNAMIC_CALL: &str = "dynamic-call-present";
/// The function's name appears as a string/callable value (invisible to call resolution).
pub const REASON_REFERENCED_AS_VALUE: &str = "function-referenced-as-value";
/// The function's name doesn't resolve uniquely (duplicate def or builtin shadow).
pub const REASON_AMBIGUOUS: &str = "resolution-ambiguous";
/// A call reaching this function used named/spread args (unreliable positional mapping).
pub const REASON_NAMED_OR_SPREAD: &str = "named-or-spread-args";
/// At least one relevant call-site argument is not a proven literal.
pub const REASON_ARG_NOT_PROVEN: &str = "argument-not-proven";
/// A non-vendor file contains `eval(...)` — code as data can call any free
/// function invisibly (ADR-0046 §2); a project-global obstacle, every candidate refuses.
pub const REASON_EVAL_PRESENT: &str = "eval-present";
/// A non-vendor `include`/`require` with an unproven or out-of-universe path
/// (ADR-0046 §2) can define/call anything; every candidate refuses.
pub const REASON_DYNAMIC_INCLUDE: &str = "dynamic-include-present";
/// Inheritance-involved candidate method (overridable, overriding, abstract,
/// interface, unresolvable hierarchy) — could break Liskov substitution, so
/// v1 refuses the whole method (ADR-0041 §1 / ADR-0043 §6).
pub const REASON_METHOD_INHERITANCE: &str = "method-inheritance";
/// A magic method (`__construct`, `__wakeup`, `__toString`, any `__*`) is
/// runtime-invoked with no ordinary call site, so never a candidate (ADR-0046 §3).
pub const REASON_MAGIC_METHOD: &str = "magic-method";
/// **Promotion**-only: an empty caller set is vacuous "all callers proven" with
/// zero evidence, so it can't enter the verified stratum (ADR-0037) — the
/// framework reflection-dispatch hole (ADR-0047 §4; amends ADR-0041 §3).
/// Honesty never hits this: its "lie" enumeration needs an observed violation.
pub const REASON_NO_OBSERVED_CALLERS: &str = "no-observed-callers";

// Candidate / call-site helpers

/// Count each FQN across the project so a duplicate (ambiguous) definition refuses.
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

/// A [`SiteRef`] for a function's `@return` site.
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

/// A [`SiteRef`] for a candidate method's `@return` site.
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

/// Project-wide obstacles making "all callers proven" unknowable for a method
/// target (shared by method promotion and `@param` honesty; ADR-0043 §6).
/// `Ok(())` when enumerable, else a named refusal. `named-or-spread-args` is
/// not checked here — it's per-target, checked where observed args are proven.
pub fn check_method_caller_enumerability(
    method_name: &str,
    sweep: &MethodSweep,
) -> Result<(), (&'static str, String)> {
    // Any dynamic method-call site anywhere blocks enumerability (ADR-0047 §6).
    if !sweep.dynamic_method_sites.is_empty() {
        return Err((
            REASON_DYNAMIC_CALL,
            "a dynamic method call (`$o->$m()`) in the project could target this method".to_owned(),
        ));
    }
    let name = method_name.to_ascii_lowercase();
    if sweep.value_referenced_methods.contains_key(&name) {
        return Err((
            REASON_REFERENCED_AS_VALUE,
            format!("`{method_name}` appears as a callable string / callable-array value"),
        ));
    }
    if let Some(sites) = sweep.unresolved_method_names.get(&name) {
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
/// `param.ty == None` is ambiguous (a complex hint like `Foo|Bar $x` also
/// lowers away), so the raw bytes are inspected: skip whitespace and `&`/`...`,
/// then check whether the next token is `$variable`.
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

/// Convert a lowered [`ArgValue`] to a concrete domain [`Val`], or `None` when
/// it is not a self-evident literal (a `$var`, a call, a `new`, a closure, …).
/// Arrays are literal iff every element is.
///
/// No PHP minor is reachable here (the sweeps carry no [`steins_infer::Folder`]),
/// so `None` goes to [`normalize_array`]. ADR-0049 A12: the conservative leg —
/// an array literal straddling the 8.3 next-int change refuses rather than
/// guess a key; threading the minor through would tighten this later.
#[must_use]
pub fn arg_to_val(v: &ArgValue) -> Option<Val> {
    match v {
        ArgValue::Int(i) => Some(Val::Int(*i)),
        ArgValue::Float(f) => Some(Val::Float(*f)),
        ArgValue::Str(s) => Some(Val::Str(s.clone())),
        ArgValue::Bool(b) => Some(Val::Bool(*b)),
        ArgValue::Null => Some(Val::Null),
        ArgValue::Array(items) => {
            let normalized = normalize_array(items, None)?;
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

/// Build the acceptance contract for a **native** type (not phpdoc lowering):
/// scalars → base, `true`/`false` → bool-literal, nullable adds `null`.
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
            // Object member (ADR-0043): the class contract, native-guarded.
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
            // Object intersection lowers to the conjunctive contract.
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

/// Free-function analog of [`check_method_caller_enumerability`]: the
/// project-wide obstacles shared by promotion and `@param` honesty.
pub fn check_caller_enumerability(
    func: &FunctionDecl,
    sweep: &FreeFnSweep,
    fqn_counts: &HashMap<String, usize>,
) -> Result<(), (&'static str, String)> {
    // ADR-0047 §6.
    if !sweep.dynamic_call_sites.is_empty() {
        return Err((
            REASON_DYNAMIC_CALL,
            "a dynamic `$fn(...)` call in the project could target this function".to_owned(),
        ));
    }
    let simple = func.name.to_ascii_lowercase();
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

// Value-domain → phpdoc type rendering (ADR-0029 / ADR-0053 §7). Semantic
// normal form lives in `summarize_vals` (ADR-0052 §4); arm spelling lives in
// `steins_contract::spell` (ADR-0053 §7), shared with `steins-infer`'s
// annotate/dump. What stays here is docblock armor, applied before delegating.

/// Render a proven set of concrete values as a faithful phpdoc type (ADR-0029
/// grammar) safe to embed in a docblock, or `None` when unrenderable
/// (`type-not-renderable`). Pipeline: [`summarize_vals`] normalizes into arms →
/// docblock armor widens unsafe literals → the shared [`spell_arms`] spells the
/// result. Arrays have no faithful scalar spelling and refuse.
#[must_use]
pub fn render_value_domain(vals: &[Val]) -> Option<String> {
    let mut arms = summarize_vals(vals)?;
    docblock_widen_unsafe_literals(&mut arms);
    spell_arms(&arms)
}

/// Docblock armor (ADR-0053 §7): if a `LitStr` group carries any value unsafe
/// for a `/** … */` block ([`docblock_literal_safe`]), replace the whole group
/// with the tightest predicate keyword ([`ContractTy::StrWith`]) before the
/// shared speller runs. No-op when the group is all-safe or absent; the only
/// docblock-specific step (dump/annotate call [`spell_arms`] directly).
fn docblock_widen_unsafe_literals(arms: &mut Vec<ContractTy>) {
    // `summarize_vals` yields the string group as either one `StrWith` arm
    // (numeric collapse) or distinct-sorted `LitStr` arms — never both.
    let lits: Vec<&PhpStr> = arms
        .iter()
        .filter_map(|a| if let ContractTy::LitStr(s) = a { Some(s) } else { None })
        .collect();
    // A byte string is unsafe by construction — phpdoc has no escape for those
    // bytes (ADR-0080 §2.5), so it widens too.
    if lits.is_empty() || lits.iter().all(|s| s.as_str().is_some_and(docblock_literal_safe)) {
        return;
    }
    // The shared, implication-closed predicate summary of the group.
    let mut preds = StrPreds::of(lits[0]);
    for s in &lits[1..] {
        preds = preds.intersect(StrPreds::of(s));
    }
    // Replace the contiguous `LitStr` arms with one keyword arm, preserving order.
    let at = arms.iter().position(|a| matches!(a, ContractTy::LitStr(_))).expect("a LitStr arm");
    arms.retain(|a| !matches!(a, ContractTy::LitStr(_)));
    arms.insert(at, ContractTy::StrWith(preds));
}

/// Whether a string can be a single-quoted phpdoc literal inside a docblock
/// without corrupting it: `*/` closes the block early (a parse error at the
/// callsite), and a raw newline/CR is rejected by the phpdoc lexer — either
/// forces a widen to a keyword. (`\`/`'` are handled by the speller's escaping.)
fn docblock_literal_safe(s: &str) -> bool {
    !s.contains("*/") && !s.contains('\n') && !s.contains('\r')
}

/// Whether `contract` admits *every* value in `vals` with [`Certainty::Yes`] — the
/// "type faithfully covers the proof" test used by the native-contradiction guard.
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
        Val::Str(v.into())
    }

    fn round_trips(rendered: &str) {
        let parsed =
            steins_phpdoc::parse_type(rendered).unwrap_or_else(|e| panic!("`{rendered}`: {e}"));
        assert!(parsed.at_end, "`{rendered}` did not fully parse");
    }

    #[test]
    fn int_and_numeric_strings_render_the_canonical_union() {
        // ADR-0037 canonical union.
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

    #[test]
    fn star_slash_string_never_renders_a_literal() {
        let r = render_value_domain(&[s("a*/b")]).unwrap();
        assert!(!r.contains("*/"), "rendered `{r}` still carries the docblock terminator");
        assert!(!r.contains('\''), "rendered `{r}` is a corrupting literal, not a keyword");
        // issue #240: keyword is the grid cell the predicates name, not one rung.
        assert_eq!(r, "non-falsy-lowercase-string");
        round_trips(&r);
    }

    #[test]
    fn star_slash_in_a_union_forces_a_keyword() {
        let r = render_value_domain(&[s("ok"), s("a*/b")]).unwrap();
        assert!(!r.contains("*/"), "rendered `{r}` still carries the docblock terminator");
        assert_eq!(r, "non-falsy-lowercase-string");
        round_trips(&r);
    }

    #[test]
    fn newline_string_never_renders_a_literal() {
        let r = render_value_domain(&[s("line1\nline2")]).unwrap();
        assert!(!r.contains('\n') && !r.contains('\''), "rendered `{r}` corrupts the tag line");
        assert_eq!(r, "non-falsy-lowercase-string");
        round_trips(&r);
    }

    /// `php_is_numeric` trims newlines, so `"5\n"` is numeric yet newline-bearing.
    #[test]
    fn newline_bearing_numeric_string_renders_the_keyword() {
        let r = render_value_domain(&[s("5\n")]).unwrap();
        // Three predicates (numeric, non-falsy, uncased), one grid cell (issue #240).
        assert_eq!(r, "non-falsy-numeric-uncased-string");
        assert!(!r.contains('\n') && !r.contains('\''));
        round_trips(&r);
    }

    /// Annotate-parity contract (ADR-0053 §7): the shared `spell_arms` — also
    /// called by `annotate`/dump in `steins-infer` — reproduces this renderer
    /// byte-for-byte wherever the docblock armor is a no-op; diverges only on
    /// docblock-unsafe values, where the renderer widens and the speller doesn't.
    #[test]
    fn shared_speller_is_byte_equal_to_the_docblock_renderer_on_safe_sets() {
        let safe_sets: Vec<Vec<Val>> = vec![
            vec![i(1), s("12"), s("34")],
            vec![s("123")],
            vec![s("POST"), s("GET"), s("GET")],
            vec![i(1), i(2), i(1)],
            vec![i(1), Val::Null],
            vec![Val::Bool(true), Val::Bool(false)],
            vec![Val::Bool(true)],
            vec![s("a'b"), s("c\\d")],
            vec![Val::Float(1.5), i(2)],
        ];
        for vals in &safe_sets {
            let docblock = render_value_domain(vals);
            let shared = summarize_vals(vals).and_then(|arms| spell_arms(&arms));
            assert_eq!(shared, docblock, "shared speller diverged from the renderer on {vals:?}");
        }

        // Array-bearing refusal is shared too.
        assert_eq!(render_value_domain(&[Val::Array(vec![])]), None);
        assert_eq!(summarize_vals(&[Val::Array(vec![])]).and_then(|a| spell_arms(&a)), None);

        // Documented divergence: renderer widens, shared speller spells the literal.
        let unsafe_val = vec![s("a*/b")];
        assert_eq!(render_value_domain(&unsafe_val).unwrap(), "non-falsy-lowercase-string");
        assert_eq!(
            summarize_vals(&unsafe_val).and_then(|a| spell_arms(&a)).unwrap(),
            "'a*/b'"
        );
    }
}
