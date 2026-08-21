//! Declaration lowering: the file-wide CST walk and the functions, classes,
//! interfaces, traits, enums, methods, properties, parameters and type hints it
//! emits, plus the docblock index (ADR-0029) and the `#[Pure]`/`#[Effect]`
//! attribute recognition (ADR-0018).

use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, Attribute, Class, ClassLikeMember, ClassLikeMemberSelector, Expression,
    Function, FunctionCall, Hint, Identifier, Literal, MagicConstant, Method, MethodBody, Modifier,
    Node, PartialArgument, PlainProperty, Program, Property, PropertyItem, TriviaKind,
    UnaryPrefixOperator, UseItems,
};

use crate::ast::{
    AnonClassEdge, ArgValue, CatchClause, ClassAliasEdge, ClassConstDecl, ClassDecl, ClassRef,
    DynamismKind, DynamismSite, EffectEnvelope, EnumCaseDecl, FunctionDecl, GlobalConstDecl,
    IncludePath, MethodDecl, NameRef, NativeType, Param, PropertyDecl, ReflectionKind,
    ReflectionSite, RetBoundKeyword, RetBoundKind, ScalarType, Span, StaticClass, TypeMember,
    Visibility, normalize_const_fqn,
};
use crate::lower_effect::{
    EffectScanCx, body_aliased, collect_body_callables, receiver_writes, scan_effect_origins,
    scan_throw_origins,
};
use crate::lower_expr::{
    class_const_name, instantiation_class, is_strict_types_one, lower_arg_value, lower_call,
    method_name_of, trace_static_class,
};
use crate::stack_guard;
use crate::tree::Lowered;
use crate::{
    PREG_FLAG_CONST_NAMES, RefResolver, bytes_to_string, children, ctx_of, name_ref, strip_dollar,
    to_span, use_binds_php_version_id, use_binds_preg_flag_const,
};

// ---------------------------------------------------------------------------
// Lowering (private): walk the Mago CST, emit owned data.
// ---------------------------------------------------------------------------

pub(crate) fn walk(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
    typed_sig: bool,
    out: &mut Lowered,
) {
    match node {
        Node::Function(f) => out.functions.push(lower_function(f, aliases, docs, rc, conditional)),
        Node::FunctionCall(c) => {
            // `class_alias(...)` (ADR-0049 §2): two compile-time names (string literal or
            // `X::class`, issue #36) mint an index alias edge; a runtime-minted name dams
            // instead. Collected file-wide, before the call itself is lowered.
            classify_class_alias(c, rc, out);
            // `define(...)` (ADR-0078, issue #198): same split as `class_alias` above —
            // literal name mints a global constant, computed name dams.
            classify_define(c, out);
            // `func_get_args()` under a typed signature (issue #30, report-only): the
            // declared argument shape is one the body then bypasses.
            if typed_sig
                && let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("func_get_args")
            {
                out.reflection.push(ReflectionSite {
                    kind: ReflectionKind::FuncGetArgsInTypedSignature,
                    span: to_span(c.span()),
                });
            }
            // `define('…PHP_VERSION_ID', …)` with a literal name (issue #29): name-only,
            // over-broad — one hit disables the version-guard fold project-wide.
            if let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define")
                && let Some(first) = c.argument_list.arguments.iter().next()
                && let Expression::Literal(Literal::String(ls)) = first.value().unparenthesized()
                && ls.value.is_some_and(|bytes| bytes_to_string(bytes).ends_with("PHP_VERSION_ID"))
            {
                out.php_version_id_declared = true;
            }
            // `define('…PREG_SET_ORDER', …)` and siblings (issue #168): same name-only,
            // over-broad scan — one hit disables the engine-constant flags resolution.
            if let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define")
                && let Some(first) = c.argument_list.arguments.iter().next()
                && let Expression::Literal(Literal::String(ls)) = first.value().unparenthesized()
                && ls.value.is_some_and(|bytes| {
                    let name = bytes_to_string(bytes);
                    PREG_FLAG_CONST_NAMES.iter().any(|n| name.ends_with(n))
                })
            {
                out.preg_flag_const_declared = true;
            }
            out.calls.push(lower_call(c));
        }
        // `const PHP_VERSION_ID = …;` (issue #29): a userland twin, name-only, ns-blind.
        Node::Constant(con) => {
            if con.items.iter().any(|i| bytes_to_string(i.name.value) == "PHP_VERSION_ID") {
                out.php_version_id_declared = true;
            }
            // `const PREG_SET_ORDER = …;` and siblings (issue #168): same reading.
            if con
                .items
                .iter()
                .any(|i| PREG_FLAG_CONST_NAMES.contains(&bytes_to_string(i.name.value).as_str()))
            {
                out.preg_flag_const_declared = true;
            }
            // …and the same statement declares global constants (ADR-0078, issue #198),
            // one per item (a class constant is the separate `ClassLikeConstant` node).
            for item in con.items.iter() {
                let name = bytes_to_string(item.name.value);
                let offset = to_span(item.name.span).start;
                out.global_const_decls.push(GlobalConstDecl {
                    fqn: normalize_const_fqn(&qualify_const_decl(rc, offset, &name)),
                    span: to_span(item.name.span),
                });
            }
        }
        // `use const … as PHP_VERSION_ID` (issue #29): an unqualified `PHP_VERSION_ID` in
        // this file then names the import, not the engine constant (exact, case-sensitive
        // match). Const imports are otherwise unlowered; this flag is all that's read.
        Node::Use(u) => {
            if use_binds_php_version_id(u) {
                out.php_version_id_aliased = true;
            }
            if use_binds_preg_flag_const(u) {
                out.preg_flag_const_aliased = true;
            }
        }
        // Reflection-driven invocation, recognized by method name alone (issue #30,
        // report-only guess — see [`ReflectionKind`]).
        Node::MethodCall(mc) => push_reflection_method(&mc.method, to_span(mc.span()), out),
        Node::NullSafeMethodCall(mc) => push_reflection_method(&mc.method, to_span(mc.span()), out),
        // Anonymous class (ADR-0049 A4): edge-only lowering — inheritance refs, no
        // members/FQN. The S6 descendant-closure walk reads these to taint a closure.
        Node::AnonymousClass(ac) => {
            out.anon_class_edges.push(AnonClassEdge {
                parent: ac.extends.as_ref().and_then(|e| e.types.iter().next()).map(name_ref),
                implements: ac
                    .implements
                    .as_ref()
                    .map(|i| i.types.iter().map(name_ref).collect())
                    .unwrap_or_default(),
                span: to_span(ac.span()),
            });
            // …and the SAME names are hard refs too (issue #182): a missing parent/
            // interface fatals at the declaring `new`, like a named class fatals at load.
            push_inheritance_refs(ac.extends.as_ref(), ac.implements.as_ref(), out);
        }
        // Class-reference positions verified to break at run time (ADR-0049 §5/S4,
        // widened by issue #182); only explicitly-named classes are collected, so
        // `class.undefined` never fires on self/static/parent/dynamic forms.
        //
        // (a) The original four hard-error expression positions.
        Node::Instantiation(inst) => {
            if let Some(r) = instantiation_class(inst) {
                out.hard_class_refs.push(r);
            }
        }
        Node::StaticMethodCall(sc) => {
            if let Some(StaticClass::Named(r)) = trace_static_class(sc.class) {
                if closure_bind_computed_scope(&r, sc) {
                    out.reflection.push(ReflectionSite {
                        kind: ReflectionKind::ClosureBindComputedScope,
                        span: to_span(sc.span()),
                    });
                }
                out.hard_class_refs.push(r);
            }
        }
        Node::ClassConstantAccess(cc) => {
            // `X::class` is a plain string since PHP 8.0 — never a hard-error site.
            let is_class_const =
                class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"));
            if !is_class_const
                && let Some(StaticClass::Named(r)) = trace_static_class(cc.class)
            {
                out.hard_class_refs.push(r);
            }
        }
        Node::StaticPropertyAccess(sp) => {
            if let Some(StaticClass::Named(r)) = trace_static_class(sp.class) {
                out.hard_class_refs.push(r);
            }
        }
        // member absence (ADR-0078, issue #197)
        // Every property-write lvalue, collected wherever the walk visits a node — so a
        // write buried in a sub-expression (`f($o->dyn = 1)`) is seen too. Nested scopes
        // are NOT skipped (unlike `collect_assign_writes`): a closure's property write counts.
        Node::Assignment(a) => out.property_writes.push_lvalue(a.lhs),
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                out.property_writes.push_lvalue(u.operand);
            }
        }
        Node::UnaryPostfix(u) => out.property_writes.push_lvalue(u.operand),
        // `foreach ($xs as $o->p)` binds the property on every iteration.
        Node::ForeachValueTarget(t) => out.property_writes.push_lvalue(t.value),
        Node::ForeachKeyValueTarget(t) => {
            out.property_writes.push_lvalue(t.key);
            out.property_writes.push_lvalue(t.value);
        }
        // end member absence (ADR-0078, issue #197)
        // A bare constant fetch (ADR-0078, issue #198): the one read position, fatal
        // `Error: Undefined constant "X"` since PHP 8.0 (`php -r`-witnessed on 8.5.9).
        // The grammar excludes `X::CONST`/`__LINE__`/`true`/`false`/`null` by construction;
        // the textual check below is belt-and-braces for the case-insensitive reserved trio.
        Node::ConstantAccess(ca) => {
            let r = name_ref(&ca.name);
            let reserved = !r.raw.contains('\\')
                && ["true", "false", "null"].iter().any(|k| r.raw.eq_ignore_ascii_case(k));
            if !reserved {
                out.const_refs.push(r);
            }
        }
        // (b) Inheritance (issue #182): `extends`/`implements`/trait `use` — every one
        // fatals at CLASS LOAD time, the strongest consequence in the family.
        Node::Class(c) => push_inheritance_refs(c.extends.as_ref(), c.implements.as_ref(), out),
        Node::Interface(i) => push_inheritance_refs(i.extends.as_ref(), None, out),
        Node::Enum(e) => push_inheritance_refs(None, e.implements.as_ref(), out),
        Node::TraitUse(tu) => out.hard_class_refs.extend(tu.trait_names.iter().map(name_ref)),
        // (c) `catch (X $e)` (issue #182): a missing class never matches, silently
        // dead-handling. Reuses `lower_catch_clause` (ADR-0040's caught-name set); a
        // clause with an unresolvable member contributes nothing, not even resolvable arms.
        Node::TryCatchClause(c) => {
            let clause = lower_catch_clause(c);
            if !clause.has_unresolvable {
                out.hard_class_refs.extend(clause.classes);
            }
        }
        // (d) Native type declarations (issue #182): a missing class in a param/return/
        // property type raises `TypeError` on first typed use; built-ins excluded
        // structurally.
        Node::FunctionLikeParameter(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
            }
        }
        Node::FunctionLikeReturnTypeHint(r) => {
            collect_hint_class_refs(&r.hint, &mut out.hard_class_refs);
        }
        Node::PlainProperty(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
            }
        }
        Node::HookedProperty(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
            }
        }
        Node::DeclareItem(d) if is_strict_types_one(d) => out.strict_types = true,
        // Dynamic-code constructs (ADR-0046 §2), collected file-wide, not per-scope.
        Node::EvalConstruct(ec) => {
            out.dynamism.push(DynamismSite { kind: DynamismKind::Eval, span: to_span(ec.span()) });
        }
        Node::IncludeConstruct(ic) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(ic.value)),
            span: to_span(ic.span()),
        }),
        Node::IncludeOnceConstruct(ic) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(ic.value)),
            span: to_span(ic.span()),
        }),
        Node::RequireConstruct(rq) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(rq.value)),
            span: to_span(rq.span()),
        }),
        Node::RequireOnceConstruct(rq) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(rq.value)),
            span: to_span(rq.span()),
        }),
        _ => {}
    }
    // A function reached only through the program root/namespace is unconditional
    // (ADR-0049 A2i); anything else nested below makes declarations conditional —
    // the same rule the class conditional flag uses.
    let child_conditional = conditional || !is_decl_transparent(node);
    // The typed-signature flag belongs to the *nearest enclosing* function-like, so
    // every function-like node recomputes it (a nested untyped closure stays untyped).
    let child_typed = match node {
        Node::Function(f) => signature_is_typed(&f.parameter_list, f.return_type_hint.as_ref()),
        Node::Method(m) => signature_is_typed(&m.parameter_list, m.return_type_hint.as_ref()),
        Node::Closure(c) => signature_is_typed(&c.parameter_list, c.return_type_hint.as_ref()),
        Node::ArrowFunction(a) => signature_is_typed(&a.parameter_list, a.return_type_hint.as_ref()),
        _ => typed_sig,
    };
    for child in children(node) {
        walk(&child, aliases, docs, rc, child_conditional, child_typed, out);
    }
}

/// Push every name an inheritance clause pair mentions onto the hard-reference list
/// (issue #182): `extends`/`implements` are `Identifier` sequences in every case
/// (class/interface/enum), always textual — no `extends $x`/`self` — so nothing excludes.
fn push_inheritance_refs(
    extends: Option<&mago_syntax::cst::Extends<'_>>,
    implements: Option<&mago_syntax::cst::Implements<'_>>,
    out: &mut Lowered,
) {
    if let Some(e) = extends {
        out.hard_class_refs.extend(e.types.iter().map(name_ref));
    }
    if let Some(i) = implements {
        out.hard_class_refs.extend(i.types.iter().map(name_ref));
    }
}

/// Collect every class-like name a native type declaration mentions (issue #182), one
/// [`NameRef`] per named arm (`?X`, `X|Y`, `X&Y`, DNF `(A&B)|null`). Built-ins are excluded
/// structurally: each is its own `Hint` variant, so only `Hint::Identifier` names a class.
fn collect_hint_class_refs(hint: &Hint<'_>, out: &mut Vec<NameRef>) {
    match hint {
        Hint::Identifier(id) => out.push(name_ref(id)),
        Hint::Nullable(n) => collect_hint_class_refs(n.hint, out),
        Hint::Union(u) => {
            collect_hint_class_refs(u.left, out);
            collect_hint_class_refs(u.right, out);
        }
        Hint::Intersection(i) => {
            collect_hint_class_refs(i.left, out);
            collect_hint_class_refs(i.right, out);
        }
        Hint::Parenthesized(p) => collect_hint_class_refs(p.hint, out),
        _ => {}
    }
}

/// Whether a function-like signature declares **any** native type hint. Deliberately
/// "any" not "all": one hint is already a shape claim the body can bypass (`func_get_args()`).
fn signature_is_typed(
    params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
    ret: Option<&mago_syntax::cst::FunctionLikeReturnTypeHint<'_>>,
) -> bool {
    ret.is_some() || params.parameters.iter().any(|p| p.hint.is_some())
}

/// Record an `->invoke*()` / `->newInstance*()` reflection site (issue #30), matched
/// on method name only (no receiver type is knowable) — an acknowledged over-inclusion
/// ([`ReflectionKind`]). `__invoke` itself is not matched (prefix is `invoke`, not `_`).
fn push_reflection_method(selector: &ClassLikeMemberSelector<'_>, span: Span, out: &mut Lowered) {
    let Some(name) = method_name_of(selector) else { return };
    // `get(..n)`, never `[..n]`: PHP identifiers can be multibyte, so byte index `n`
    // may not be a char boundary — a mid-character slice isn't the ASCII prefix sought.
    let has_prefix =
        |p: &str| name.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p));
    let kind = if has_prefix("invoke") {
        ReflectionKind::Invoke
    } else if has_prefix("newInstance") {
        ReflectionKind::NewInstance
    } else {
        return;
    };
    out.reflection.push(ReflectionSite { kind, span });
}

/// Whether `Closure::bind(...)`'s third (scope) argument is computed — anything but
/// a string literal, `X::class`, or `null` — meaning the rebound closure's reachable
/// private/protected surface isn't statically known (issue #30). Named args/`bindTo` excluded.
fn closure_bind_computed_scope(class: &NameRef, sc: &mago_syntax::cst::StaticMethodCall<'_>) -> bool {
    if !class.simple().eq_ignore_ascii_case("Closure")
        || !method_name_of(&sc.method).is_some_and(|m| m.eq_ignore_ascii_case("bind"))
    {
        return false;
    }
    let scope = sc
        .argument_list
        .arguments
        .iter()
        .filter_map(|a| match a {
            Argument::Positional(p) if p.ellipsis.is_none() => Some(p.value),
            _ => None,
        })
        .nth(2);
    scope.is_some_and(|e| !is_literal_class_name(e))
}

/// Whether an expression names a class statically for `Closure::bind`'s scope
/// argument: a string literal, `X::class`, or the `null` unbind.
fn is_literal_class_name(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::String(_) | Literal::Null(_)) => true,
        Expression::Access(Access::ClassConstant(cc)) => {
            class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"))
        }
        _ => false,
    }
}

/// The proven prefix of a concatenation chain: a literal, `__DIR__`-anchored, or unproven.
enum ConcatVal {
    Str(String),
    DirRel(String),
    Unproven,
}

/// Lower an `include`/`require` path expression to a judgeable [`IncludePath`]
/// (ADR-0046 §2): literals, literal concatenations, and `__DIR__ . '<suffix>'` resolve;
/// everything else is [`IncludePath::Unproven`] (sound default — unprovable is an obstacle).
fn lower_include_path(expr: &Expression<'_>) -> IncludePath {
    match lower_concat(expr) {
        ConcatVal::Str(s) => IncludePath::Literal(s),
        ConcatVal::DirRel(s) => IncludePath::DirRelative(s),
        ConcatVal::Unproven => IncludePath::Unproven,
    }
}

/// Fold a string-concatenation subtree into its proven value: `__DIR__` anchors a
/// directory-relative result, a literal-only chain folds to a literal, else unproven.
fn lower_concat(expr: &Expression<'_>) -> ConcatVal {
    // A long `.` chain recurses once per operand (issue #264); out of headroom, unproven.
    if stack_guard::exhausted() {
        return ConcatVal::Unproven;
    }
    match expr.unparenthesized() {
        // A name lane (include paths, `class_alias` args), not a value lane: looked up
        // in a `String`-keyed universe, so non-UTF-8 bytes are unproven, never lossily
        // decoded (ADR-0080 §2.5).
        Expression::Literal(Literal::String(ls)) => ls
            .value
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map_or(ConcatVal::Unproven, |s| ConcatVal::Str(s.to_owned())),
        Expression::MagicConstant(MagicConstant::Directory(_)) => ConcatVal::DirRel(String::new()),
        Expression::Binary(b) if b.operator.is_concatenation() => {
            match (lower_concat(b.lhs), lower_concat(b.rhs)) {
                (ConcatVal::Str(l), ConcatVal::Str(r)) => ConcatVal::Str(format!("{l}{r}")),
                (ConcatVal::DirRel(l), ConcatVal::Str(r)) => ConcatVal::DirRel(format!("{l}{r}")),
                _ => ConcatVal::Unproven,
            }
        }
        _ => ConcatVal::Unproven,
    }
}

/// Classify a `class_alias(...)` call (ADR-0049 §2): two **compile-time** class names
/// mint an index [`ClassAliasEdge`]; a runtime-only name (variable, call, computed
/// string) dams as [`DynamismKind::ClassAlias`]. Only the global `class_alias`
/// (unqualified/fully-qualified) is recognized — `Foo\class_alias` differs. The
/// compile-time set (via [`lower_alias_name`]) also accepts `X::class` (issue #36).
fn classify_class_alias(c: &FunctionCall<'_>, rc: &RefResolver, out: &mut Lowered) {
    let Expression::Identifier(id) = c.function else { return };
    if !matches!(id, Identifier::Local(_) | Identifier::FullyQualified(_)) {
        return;
    }
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("class_alias") {
        return;
    }
    let span = to_span(c.span());

    // The first two positional (non-spread) arguments must both name a class at compile
    // time; anything else (named/spread/runtime-minted) dams. Already index-key normalized.
    let mut names: Vec<String> = Vec::new();
    let mut clean = true;
    for arg in c.argument_list.arguments.iter() {
        if names.len() >= 2 {
            break;
        }
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => match lower_alias_name(p.value, rc) {
                Some(s) => names.push(s),
                None => {
                    clean = false;
                    break;
                }
            },
            _ => {
                clean = false;
                break;
            }
        }
    }

    if clean && names.len() == 2 {
        // `class_alias($class, $alias)` — arg 0 is the existing class, arg 1 resolves to it.
        out.class_alias_edges.push(ClassAliasEdge {
            alias_fqn: names[1].clone(),
            target_fqn: names[0].clone(),
            span,
        });
    } else {
        out.dynamism.push(DynamismSite { kind: DynamismKind::ClassAlias, span });
    }
}

/// Classify a `define(...)` call (ADR-0078, issue #198), the constant-side twin of
/// [`classify_class_alias`]: a compile-time name mints a [`GlobalConstDecl`]; a runtime-
/// only name dams as [`DynamismKind::DefineDynamic`]. Two differences from `class_alias`:
/// the name is NOT resolved against namespace/`use` (`define('FOO',1)` in `namespace App;`
/// declares global `FOO`, not `App\FOO` — `php -r`-witnessed on 8.5.9), and `X::class` isn't
/// accepted. Callee recognition matches `class_alias`'s (unqualified/fully-qualified only).
fn classify_define(c: &FunctionCall<'_>, out: &mut Lowered) {
    let Expression::Identifier(id) = c.function else { return };
    if !matches!(id, Identifier::Local(_) | Identifier::FullyQualified(_)) {
        return;
    }
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define") {
        return;
    }
    let span = to_span(c.span());
    // The name is the FIRST positional (non-spread) argument; a named/spread one dams.
    let literal = match c.argument_list.arguments.iter().next() {
        Some(Argument::Positional(p)) if p.ellipsis.is_none() => {
            match lower_concat(p.value.unparenthesized()) {
                ConcatVal::Str(s) => Some(s),
                _ => None,
            }
        }
        _ => None,
    };
    match literal {
        Some(name) => out
            .global_const_decls
            .push(GlobalConstDecl { fqn: normalize_const_fqn(name.trim()), span }),
        None => out.dynamism.push(DynamismSite { kind: DynamismKind::DefineDynamic, span }),
    }
}

/// The FQN a `const NAME = …;` statement at `offset` declares: namespace joined to
/// the name, or the name alone globally. Case is preserved; [`normalize_const_fqn`] folds it.
fn qualify_const_decl(rc: &RefResolver, offset: u32, name: &str) -> String {
    let ns = &ctx_of(rc.contexts, rc.regions, offset).namespace;
    if ns.is_empty() { name.to_owned() } else { format!("{ns}\\{name}") }
}

/// Lower one `class_alias` argument to the normalized index-key FQN it names at
/// **compile time**, or `None` when only known at run time (dams — ADR-0049 §2).
/// Two shapes qualify, normalized *differently*:
///
/// - a **string literal** (or literal-only concat): the full FQN as written — PHP
///   doesn't resolve it against `use`/namespace, so neither does [`normalize_alias_fqn`].
/// - **`X::class`** (issue #36): a compile-time string since PHP 8.0 (no autoload,
///   class need not exist), so it must not dam. Its spelling IS resolved via the
///   same [`RefResolver`] every class reference uses.
///
/// Not widened past those two: `self`/`parent::class` are lexically knowable but this
/// walk has no enclosing-class context, and `static::class` is late-bound — all three
/// dam, like any variable/call/concatenation ([`lower_concat`] folds only literals/`__DIR__`).
fn lower_alias_name(expr: &Expression<'_>, rc: &RefResolver) -> Option<String> {
    let expr = expr.unparenthesized();
    // `X::class` — an explicitly-named class only (`self`/`static`/`parent`/dynamic
    // exprs fall through to the literal path, which rejects them).
    if let Expression::Access(Access::ClassConstant(cc)) = expr
        && class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"))
    {
        return match cc.class {
            Expression::Identifier(id) => {
                Some(normalize_alias_fqn(&rc.class_display_fqn(&name_ref(id))))
            }
            _ => None,
        };
    }
    match lower_concat(expr) {
        ConcatVal::Str(s) => Some(normalize_alias_fqn(&s)),
        _ => None,
    }
}

/// Normalize a `class_alias` class name to the index key shape: trimmed, leading `\`
/// stripped, lowercased. Applied to an already-resolved name, so no context lookup here.
fn normalize_alias_fqn(s: &str) -> String {
    s.trim().trim_start_matches('\\').to_ascii_lowercase()
}

fn lower_function(
    f: &Function<'_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
) -> FunctionDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    let cx = EffectScanCx::new(
        &f.parameter_list,
        collect_body_callables(f.body.statements.iter()),
        body_aliased(f.body.statements.iter()),
        receiver_writes(f.body.statements.iter()),
    );
    for s in f.body.statements.iter() {
        scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
    }

    FunctionDecl {
        name: bytes_to_string(f.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        params: lower_params(&f.parameter_list, rc),
        ret: f.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        ret_span: f.return_type_hint.as_ref().map(|r| to_span(r.hint.span())),
        span: to_span(f.name.span()),
        body_span: to_span(f.body.span()),
        effect_envelope: attrs_effect_envelope(&f.attribute_lists, aliases),
        effect_origins,
        throw_origins,
        docblock: docs.preceding(to_span(f.span()).start),
        docblock_span: docs.preceding_span(to_span(f.span()).start),
        conditional,
    }
}

/// Lower a parameter list to owned [`Param`]s (shared by functions and methods).
pub(crate) fn lower_params(list: &mago_syntax::cst::FunctionLikeParameterList<'_>, rc: &RefResolver) -> Vec<Param> {
    list.parameters
        .iter()
        .map(|p| Param {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty: p.hint.as_ref().and_then(|h| lower_hint(h, rc)),
            // The syntactic answer, kept beside the modeling one (issue #200).
            hint_span: p.hint.as_ref().map(|h| to_span(h.span())),
            variadic: p.is_variadic(),
            by_ref: p.is_reference(),
            has_null_default: p
                .default_value
                .as_ref()
                .is_some_and(|d| matches!(d.value.unparenthesized(), Expression::Literal(Literal::Null(_)))),
            has_default: p.default_value.is_some(),
            default: p
                .default_value
                .as_ref()
                .map(|d| lower_arg_value(d.value))
                .filter(|v| !matches!(v, ArgValue::Other)),
            span: to_span(p.span()),
        })
        .collect()
}

/// Lower every `class`/`interface`/`enum`/`trait` declaration reachable from `node`
/// (ADR-0043 enums; ADR-0049 §5 trait names). `conditional` (ADR-0049 A2i) starts
/// `false` at the program root, turning `true` under any non-namespace/program node.
pub(crate) fn lower_classes(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
) -> Vec<ClassDecl> {
    let mut out = Vec::new();
    lower_classes_into(node, aliases, docs, rc, false, &mut out);
    out
}

fn lower_classes_into(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
    out: &mut Vec<ClassDecl>,
) {
    match node {
        Node::Class(c) => out.push(lower_class(c, aliases, docs, rc, conditional)),
        Node::Interface(i) => out.push(lower_interface(i, aliases, docs, rc, conditional)),
        Node::Enum(e) => out.push(lower_enum(e, aliases, docs, rc, conditional)),
        Node::Trait(t) => out.push(lower_trait(t, conditional)),
        _ => {}
    }
    // A declaration reached only through a plain namespace/program node is
    // unconditional; anything else below it makes nested declarations conditional.
    let child_conditional = conditional || !is_decl_transparent(node);
    for child in children(node) {
        lower_classes_into(&child, aliases, docs, rc, child_conditional, out);
    }
}

/// Whether descending through `node` keeps a declaration **unconditional** (ADR-0049
/// A2i): only the program root, namespace nodes, and the `Statement` wrapper are
/// transparent; every other node (control flow, function/method body, block) taints it.
fn is_decl_transparent(node: &Node<'_, '_>) -> bool {
    matches!(
        node,
        Node::Program(_)
            | Node::Statement(_)
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
    )
}

/// Lower a `trait` declaration to a name-only [`ClassDecl`] (ADR-0049 §5, C8/A2i):
/// it joins the class-like index but has no members/flattening, only its FQN.
fn lower_trait(t: &mago_syntax::cst::Trait<'_>, conditional: bool) -> ClassDecl {
    ClassDecl {
        name: bytes_to_string(t.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        display: String::new(),
        is_final: false,
        is_abstract: false,
        is_interface: false,
        is_enum: false,
        is_trait: true,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent: None,
        implements: Vec::new(),
        methods: Vec::new(),
        properties: Vec::new(),
        consts: Vec::new(),
        const_visibility: Vec::new(),
        const_decls: Vec::new(),
        hooked_properties: Vec::new(),
        // A trait is inert here — `uses_traits` on the using class already obstructs.
        allows_dynamic_properties: false,
        uses_traits: false,
        // No member docblock can observe a trait-level `@template`.
        docblock: None,
        docblock_span: None,
        span: to_span(t.name.span()),
    }
}

fn lower_class(c: &Class<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
    let parent = c
        .extends
        .as_ref()
        .and_then(|e| e.types.iter().next())
        .map(name_ref);
    let implements: Vec<NameRef> = c
        .implements
        .as_ref()
        .map(|i| i.types.iter().map(name_ref).collect())
        .unwrap_or_default();

    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut consts = Vec::new();
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
    let mut hooked_properties = Vec::new();
    let mut uses_traits = false;
    for member in c.members.iter() {
        match member {
            ClassLikeMember::Method(m) => {
                // A constructor's promoted params are properties too (ADR-0036).
                if bytes_to_string(m.name.value).eq_ignore_ascii_case("__construct") {
                    lower_promoted_params(m, rc, &mut properties);
                }
                methods.push(lower_method(m, aliases, docs, rc));
            }
            ClassLikeMember::Property(Property::Plain(p)) => {
                lower_plain_property(p, docs, rc, &mut properties);
            }
            // Hooked properties are virtual/computed, not heap-tracked/checked as stored
            // values — only their NAME is kept, so member-existence checks aren't fooled
            // (ADR-0078, issue #185).
            ClassLikeMember::Property(Property::Hooked(h)) => {
                hooked_properties.push(strip_dollar(bytes_to_string(match &h.item {
                    PropertyItem::Abstract(a) => a.variable.name,
                    PropertyItem::Concrete(c) => c.variable.name,
                })));
            }
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
            ClassLikeMember::TraitUse(_) => uses_traits = true,
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(c.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        display: String::new(),
        is_final: c.modifiers.iter().any(Modifier::is_final),
        is_abstract: c.modifiers.iter().any(Modifier::is_abstract),
        is_interface: false,
        is_enum: false,
        is_trait: false,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent,
        implements,
        methods,
        properties,
        consts,
        const_visibility,
        const_decls,
        hooked_properties,
        // member absence (ADR-0078, issue #197)
        allows_dynamic_properties: attrs_allow_dynamic_properties(&c.attribute_lists),
        // end member absence (ADR-0078, issue #197)
        uses_traits,
        // Class-level docblock (whole declaration incl. attributes/modifiers) — read
        // for `@template` names that shadow same-named classes in member docblocks (issue #5).
        docblock: docs.preceding(to_span(c.span()).start),
        docblock_span: docs.preceding_span(to_span(c.span()).start),
        span: to_span(c.name.span()),
    }
}

/// Lower a `const NAME = <expr>[, …];` class-member declaration into `(name, value)`
/// pairs, keeping only **literal** initializers (ADR-0043 §2) — absence in `out` means
/// "no proven literal", not "no such constant". `vis` gets every name + visibility
/// (ADR-0078 #185) regardless — the one list whose absence means the constant truly
/// doesn't exist. `decls` gets each name's declaration shape (ADR-0078 #200): PHP 8.3's
/// native constant-type span + docblock, shared across `const A = 1, B = 2;`.
fn lower_class_consts(
    k: &mago_syntax::cst::ClassLikeConstant<'_>,
    docs: &DocIndex,
    out: &mut Vec<(String, ArgValue)>,
    vis: &mut Vec<(String, Visibility)>,
    decls: &mut Vec<ClassConstDecl>,
) {
    let visibility = visibility_of(&k.modifiers);
    // untyped surface (ADR-0078, issue #200)
    let hint_span = k.hint.as_ref().map(|h| to_span(h.span()));
    let docblock = docs.preceding(to_span(k.span()).start);
    // end untyped surface (ADR-0078, issue #200)
    for item in k.items.iter() {
        let name = bytes_to_string(item.name.value);
        vis.push((name.clone(), visibility));
        decls.push(ClassConstDecl {
            name: name.clone(),
            hint_span,
            docblock: docblock.clone(),
            span: to_span(item.name.span()),
        });
        let v = lower_arg_value(item.value);
        if !matches!(v, ArgValue::Other) {
            out.push((name, v));
        }
    }
}

/// The read-visibility a modifier sequence declares; defaults to `Public` (PHP semantics).
fn visibility_of(modifiers: &mago_syntax::cst::Sequence<'_, Modifier<'_>>) -> Visibility {
    if modifiers.iter().any(Modifier::is_private) {
        Visibility::Private
    } else if modifiers.iter().any(Modifier::is_protected) {
        Visibility::Protected
    } else {
        Visibility::Public
    }
}

/// Lower a plain property declaration (possibly multi-item `public int $a, $b;`)
/// into one [`PropertyDecl`] per declared variable (ADR-0036).
fn lower_plain_property(p: &PlainProperty<'_>, docs: &DocIndex, rc: &RefResolver, out: &mut Vec<PropertyDecl>) {
    let readonly = p.modifiers.iter().any(Modifier::is_readonly);
    let is_static = p.modifiers.iter().any(Modifier::is_static);
    let visibility = visibility_of(&p.modifiers);
    let ty = p.hint.as_ref().and_then(|h| lower_hint(h, rc));
    let hint_span = p.hint.as_ref().map(|h| to_span(h.span()));
    let docblock = docs.preceding(to_span(p.span()).start);
    let span = to_span(p.span());
    for item in p.items.iter() {
        let (name, has_default, default) = match item {
            PropertyItem::Abstract(a) => (strip_dollar(bytes_to_string(a.variable.name)), false, None),
            PropertyItem::Concrete(ci) => {
                let v = lower_arg_value(ci.value);
                let default = (!matches!(v, ArgValue::Other)).then_some(v);
                (strip_dollar(bytes_to_string(ci.variable.name)), true, default)
            }
        };
        out.push(PropertyDecl {
            name,
            ty: ty.clone(),
            hint_span,
            readonly,
            is_static,
            visibility,
            has_default,
            default,
            promoted: false,
            hooked: false,
            docblock: docblock.clone(),
            span,
        });
    }
}

/// Lower a constructor's promoted parameters into [`PropertyDecl`]s (ADR-0036).
/// A parameter is promoted iff it carries a modifier (visibility / `readonly`).
fn lower_promoted_params(m: &Method<'_>, rc: &RefResolver, out: &mut Vec<PropertyDecl>) {
    for p in m.parameter_list.parameters.iter() {
        if !p.is_promoted_property() {
            continue;
        }
        let readonly = p.modifiers.iter().any(Modifier::is_readonly);
        let visibility = visibility_of(&p.modifiers);
        let ty = p.hint.as_ref().and_then(|h| lower_hint(h, rc));
        let has_default = p.default_value.is_some();
        let default = p
            .default_value
            .as_ref()
            .map(|d| lower_arg_value(d.value))
            .filter(|v| !matches!(v, ArgValue::Other));
        out.push(PropertyDecl {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty,
            hint_span: p.hint.as_ref().map(|h| to_span(h.span())),
            readonly,
            is_static: false,
            visibility,
            has_default,
            default,
            promoted: true,
            // A hook on a promoted param (PHP 8.4) makes every write/read go through
            // arbitrary code — bind no fact (FP class 16). `readonly`+hook is a PHP fatal.
            hooked: p.hooks.is_some(),
            docblock: None,
            span: to_span(p.span()),
        });
    }
}

/// Lower an `interface` declaration to a [`ClassDecl`] with `is_interface = true`
/// (ADR-0033 Liskov): methods carry effect envelopes/`@throws` docblocks as abstract
/// signatures. `extends` (interfaces can extend several) splits into `parent`+`implements`.
fn lower_interface(i: &mago_syntax::cst::Interface<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
    let mut extended: Vec<NameRef> =
        i.extends.as_ref().map(|e| e.types.iter().map(name_ref).collect()).unwrap_or_default();
    let parent = if extended.is_empty() { None } else { Some(extended.remove(0)) };

    let mut methods = Vec::new();
    let mut consts = Vec::new();
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
    for member in i.members.iter() {
        match member {
            ClassLikeMember::Method(m) => methods.push(lower_method(m, aliases, docs, rc)),
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(i.name.value),
        fqn: String::new(),
        display: String::new(),
        is_final: false,
        is_abstract: false,
        is_interface: true,
        is_enum: false,
        is_trait: false,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent,
        implements: extended,
        methods,
        properties: Vec::new(),
        consts,
        const_visibility,
        const_decls,
        hooked_properties: Vec::new(),
        // An interface declares no properties at all, so it can never be open.
        allows_dynamic_properties: false,
        uses_traits: false,
        // Class-level docblock — `@template` names shadow same-named classes in the
        // interface's method docblocks (issue #5).
        docblock: docs.preceding(to_span(i.span()).start),
        docblock_span: docs.preceding_span(to_span(i.span()).start),
        span: to_span(i.name.span()),
    }
}

/// Lower an `enum` declaration to a [`ClassDecl`] with `is_enum = true` (ADR-0043).
/// Implicitly `final`, cannot extend; joins the class index for subtyping. `implements`
/// feeds the is-a oracle (plus implicit `UnitEnum`/`BackedEnum`); cases + backing scalar
/// are recorded for value reasoning. Method bodies are not analyzed: `methods` stays empty.
fn lower_enum(e: &mago_syntax::cst::Enum<'_>, _aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
    let implements: Vec<NameRef> = e
        .implements
        .as_ref()
        .map(|i| i.types.iter().map(name_ref).collect())
        .unwrap_or_default();

    // Backing scalar: only `int`/`string` are legal; anything else records no backing.
    let enum_backing = e.backing_type_hint.as_ref().and_then(|b| match &b.hint {
        Hint::Integer(_) => Some(ScalarType::Int),
        Hint::String(_) => Some(ScalarType::String),
        _ => None,
    });

    let mut enum_cases = Vec::new();
    let mut consts = Vec::new();
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
    for member in e.members.iter() {
        match member {
            ClassLikeMember::EnumCase(case) => {
                let (name_id, value) = match &case.item {
                    mago_syntax::cst::EnumCaseItem::Unit(u) => (&u.name, None),
                    mago_syntax::cst::EnumCaseItem::Backed(b) => {
                        let v = lower_arg_value(b.value);
                        (&b.name, (!matches!(v, ArgValue::Other)).then_some(v))
                    }
                };
                enum_cases.push(EnumCaseDecl {
                    name: bytes_to_string(name_id.value),
                    value,
                    span: to_span(case.span()),
                });
            }
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
            _ => {}
        }
    }

    // Keep the class-like lowerer signature uniform; enum members need no name resolution.
    let _ = rc;

    ClassDecl {
        name: bytes_to_string(e.name.value),
        fqn: String::new(),
        display: String::new(),
        is_final: true, // enums are implicitly final in PHP
        is_abstract: false,
        is_interface: false,
        is_enum: true,
        is_trait: false,
        conditional,
        enum_backing,
        enum_cases,
        parent: None,
        implements,
        methods: Vec::new(),
        properties: Vec::new(),
        consts,
        const_visibility,
        const_decls,
        hooked_properties: Vec::new(),
        // An enum cannot declare a property, dynamic or otherwise.
        allows_dynamic_properties: false,
        uses_traits: false,
        // No analyzed member can observe an enum-level `@template`.
        docblock: None,
        docblock_span: None,
        span: to_span(e.name.span()),
    }
}

fn lower_method(m: &Method<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver) -> MethodDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    if let MethodBody::Concrete(block) = &m.body {
        let cx = EffectScanCx::new(
            &m.parameter_list,
            collect_body_callables(block.statements.iter()),
            body_aliased(block.statements.iter()),
            receiver_writes(block.statements.iter()),
        );
        for s in block.statements.iter() {
            scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
            scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
        }
    }

    let visibility = visibility_of(&m.modifiers);

    let name = bytes_to_string(m.name.value);
    let is_constructor = name.eq_ignore_ascii_case("__construct");

    MethodDecl {
        name,
        params: lower_params(&m.parameter_list, rc),
        ret: m.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        ret_bound_keyword: m.return_type_hint.as_ref().and_then(|r| ret_bound_keyword(&r.hint)),
        ret_span: m.return_type_hint.as_ref().map(|r| to_span(r.hint.span())),
        span: to_span(m.name.span()),
        body_span: match &m.body {
            MethodBody::Concrete(block) => Some(to_span(block.span())),
            // Abstract and interface methods have a `;` where a block would be.
            MethodBody::Abstract(_) => None,
        },
        effect_envelope: attrs_effect_envelope(&m.attribute_lists, aliases),
        effect_origins,
        throw_origins,
        visibility,
        is_static: m.modifiers.iter().any(Modifier::is_static),
        is_final: m.modifiers.iter().any(Modifier::is_final),
        is_abstract: m.is_abstract(),
        is_constructor,
        docblock: docs.preceding(to_span(m.span()).start),
        docblock_span: docs.preceding_span(to_span(m.span()).start),
    }
}

/// Recognize a bare `self`/`static`/`parent` return hint (or its `?`-nullable),
/// recording its keyword shape (ADR-0043 amendment §2); anything else returns `None`.
/// Runs at method lowering (no class context) — the FQN-stamping pass resolves the bound.
fn ret_bound_keyword(hint: &Hint<'_>) -> Option<RetBoundKeyword> {
    match hint {
        Hint::Static(_) => Some(RetBoundKeyword { kind: RetBoundKind::Static, nullable: false }),
        Hint::Self_(_) => Some(RetBoundKeyword { kind: RetBoundKind::SelfKw, nullable: false }),
        Hint::Parent(_) => Some(RetBoundKeyword { kind: RetBoundKind::Parent, nullable: false }),
        // `?self` / `?static` / `?parent`: the nullable of a bare keyword. Any
        // other nullable inner shape falls through to `None` via the inner call.
        Hint::Nullable(n) => {
            let mut kw = ret_bound_keyword(n.hint)?;
            kw.nullable = true;
            Some(kw)
        }
        _ => None,
    }
}

/// An index of the file's `/** … */` docblock trivia, letting a declaration adopt
/// the docblock immediately preceding its head (ADR-0029) — associated only when
/// whitespace alone separates them; a wrong association would be a wrong contract,
/// so the rule is deliberately strict.
pub(crate) struct DocIndex<'a> {
    source: &'a str,
    /// `(span, text)` of each docblock in source order: full file span + exact source text.
    blocks: Vec<(Span, String)>,
}

impl<'a> DocIndex<'a> {
    pub(crate) fn build(source: &'a str, program: &Program<'_>) -> Self {
        let blocks = program
            .trivia
            .iter()
            .filter(|t| matches!(t.kind, TriviaKind::DocBlockComment))
            .map(|t| (to_span(t.span), bytes_to_string(t.value)))
            .collect();
        Self { source, blocks }
    }

    /// The docblock preceding `decl_start` (whitespace-only gap), as `(span, text)`.
    fn preceding_block(&self, decl_start: u32) -> Option<(Span, &String)> {
        let mut best: Option<(Span, &String)> = None;
        for (span, text) in &self.blocks {
            if span.end <= decl_start && best.is_none_or(|(bs, _)| span.end > bs.end) {
                best = Some((*span, text));
            }
        }
        let (span, text) = best?;
        let gap = self.source.get(span.end as usize..decl_start as usize)?;
        gap.chars().all(char::is_whitespace).then_some((span, text))
    }

    /// The text of the docblock immediately preceding `decl_start`, if any.
    pub(crate) fn preceding(&self, decl_start: u32) -> Option<String> {
        self.preceding_block(decl_start).map(|(_, text)| text.clone())
    }

    /// The file span of the docblock immediately preceding `decl_start`, if any.
    pub(crate) fn preceding_span(&self, decl_start: u32) -> Option<Span> {
        self.preceding_block(decl_start).map(|(span, _)| span)
    }
}

/// The canonical, case-folded identity of `Steins\Pure`: leading `\` stripped, lowercased.
const PURE_CLASS: &str = "steins\\pure";

/// The canonical, case-folded identity of the `Steins\Effect` class (ADR-0018).
const EFFECT_CLASS: &str = "steins\\effect";

/// The local names a file's `use` statements bind to `Steins\Pure`/`Steins\Effect`
/// (lowercased), so a bare or aliased attribute resolves ([`collect_steins_aliases`]).
#[derive(Default)]
pub(crate) struct SteinsAttrAliases {
    pure: HashSet<String>,
    effect: HashSet<String>,
}

/// Normalize an attribute/use identifier vs [`PURE_CLASS`]: strip leading `\`, lowercase.
fn normalize_class(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

/// Collect the local names (lowercased) a file's `use` statements bind to
/// `Steins\Pure`/`Steins\Effect`, so bare or aliased attributes resolve (`use
/// Steins\Pure;` binds `pure`; `use Steins\Effect as X;` binds `x`). Only plain
/// `use A\B[ as C];` is lowered, not grouped `use A\{B};` — a miss only fails to
/// recognize an envelope, the conservative side.
pub(crate) fn collect_steins_aliases(node: &Node<'_, '_>) -> SteinsAttrAliases {
    let mut aliases = SteinsAttrAliases::default();
    collect_steins_aliases_into(node, &mut aliases);
    aliases
}

fn collect_steins_aliases_into(node: &Node<'_, '_>, out: &mut SteinsAttrAliases) {
    if let Node::Use(u) = node
        && let UseItems::Sequence(seq) = &u.items
    {
        for item in seq.items.iter() {
            let full = normalize_class(&bytes_to_string(item.name.value()));
            let set = if full == PURE_CLASS {
                &mut out.pure
            } else if full == EFFECT_CLASS {
                &mut out.effect
            } else {
                continue;
            };
            // The bound local name: the explicit alias, else the last segment.
            let local = match &item.alias {
                Some(a) => bytes_to_string(a.identifier.value),
                None => bytes_to_string(item.name.last_segment()),
            };
            set.insert(local.to_ascii_lowercase());
        }
    }
    for child in children(node) {
        collect_steins_aliases_into(&child, out);
    }
}

/// Recognize a `#[\Steins\Pure]` or `#[\Steins\Effect(...)]` envelope attribute on a
/// function/method declaration, returning the resolved [`EffectEnvelope`]. Deliberately
/// conservative: matches only a fully-/qualified `\Steins\Pure`/`\Steins\Effect`, or a
/// bare/aliased name a `use Steins\Pure[ as X];` import binds. Case-insensitive.
/// `#[\Steins\Effect(...)]` arguments must be **plain string literals**; any non-literal
/// argument makes the whole attribute *unrecognized*. Both attributes on one declaration
/// contradict; **Pure wins** (empty upper bound), silently.
// member absence (ADR-0078, issue #197)
/// Whether an attribute list carries PHP's own `#[AllowDynamicProperties]` (ADR-0078,
/// issue #197) — see [`ClassDecl::allows_dynamic_properties`] for what it licenses.
fn attrs_allow_dynamic_properties(
    attribute_lists: &mago_syntax::cst::Sequence<'_, mago_syntax::cst::AttributeList<'_>>,
) -> bool {
    attribute_lists.iter().any(|list| {
        list.attributes
            .iter()
            .any(|attr| normalize_class(&bytes_to_string(attr.name.value())) == "allowdynamicproperties")
    })
}
// end member absence (ADR-0078, issue #197)

fn attrs_effect_envelope(
    attribute_lists: &mago_syntax::cst::Sequence<'_, mago_syntax::cst::AttributeList<'_>>,
    aliases: &SteinsAttrAliases,
) -> Option<EffectEnvelope> {
    let mut pure_span: Option<Span> = None;
    let mut effect: Option<(Vec<String>, Span)> = None;

    for list in attribute_lists.iter() {
        for attr in list.attributes.iter() {
            let norm = normalize_class(&bytes_to_string(attr.name.value()));
            let is_pure = match attr.name {
                Identifier::Local(_) => aliases.pure.contains(&norm),
                Identifier::Qualified(_) | Identifier::FullyQualified(_) => norm == PURE_CLASS,
            };
            let is_effect = match attr.name {
                Identifier::Local(_) => aliases.effect.contains(&norm),
                Identifier::Qualified(_) | Identifier::FullyQualified(_) => norm == EFFECT_CLASS,
            };

            if is_pure {
                pure_span.get_or_insert_with(|| to_span(attr.span()));
            } else if is_effect
                && effect.is_none()
                && let Some(labels) = effect_attr_labels(attr)
            {
                // Recognized only when *all* arguments are string literals; else `None`.
                effect = Some((labels, to_span(attr.span())));
            }
        }
    }

    // Pure wins the contradiction (empty upper bound is the tighter bound).
    if let Some(span) = pure_span {
        return Some(EffectEnvelope { labels: Vec::new(), span });
    }
    effect.map(|(labels, span)| EffectEnvelope { labels, span })
}

/// The effect labels declared by a recognized `#[\Steins\Effect(...)]` attribute, or
/// `None` when any argument isn't a plain string literal. No/empty args yield an empty
/// label set (same tight bound as `Pure`).
fn effect_attr_labels(attr: &Attribute<'_>) -> Option<Vec<String>> {
    let Some(list) = attr.argument_list.as_ref() else {
        return Some(Vec::new());
    };
    let mut labels = Vec::new();
    for arg in list.arguments.iter() {
        let PartialArgument::Positional(p) = arg else {
            return None; // named / placeholder / variadic-placeholder → unrecognized
        };
        if p.ellipsis.is_some() {
            return None; // spread argument → unrecognized
        }
        match p.value.unparenthesized() {
            // `?` widens an undecodable literal to unrecognized, like a non-string arg.
            Expression::Literal(Literal::String(ls)) => labels.push(bytes_to_string(ls.value?)),
            _ => return None, // constant / concatenation / non-string literal → unrecognized
        }
    }
    Some(labels)
}

/// Lower a `catch (A|B $e)` clause to its caught classes plus bound variable
/// (ADR-0040). A caught-type member that is not a plain class name marks the
/// clause `has_unresolvable` (→ absorption `Maybe`).
pub(crate) fn lower_catch_clause(c: &mago_syntax::cst::TryCatchClause<'_>) -> CatchClause {
    let mut classes = Vec::new();
    let mut has_unresolvable = false;
    lower_catch_hint(&c.hint, &mut classes, &mut has_unresolvable);
    let var = c.variable.as_ref().map(|v| strip_dollar(bytes_to_string(v.name)));
    CatchClause { classes, var, has_unresolvable }
}

/// Flatten a catch type hint (a plain class or a `|`-union of them) into class
/// [`NameRef`]s; any non-identifier member sets `unresolvable`.
fn lower_catch_hint(hint: &Hint<'_>, classes: &mut Vec<NameRef>, unresolvable: &mut bool) {
    match hint {
        Hint::Identifier(id) => classes.push(name_ref(id)),
        Hint::Union(u) => {
            lower_catch_hint(u.left, classes, unresolvable);
            lower_catch_hint(u.right, classes, unresolvable);
        }
        Hint::Parenthesized(p) => lower_catch_hint(p.hint, classes, unresolvable),
        _ => *unresolvable = true,
    }
}

/// Lower a type hint to a [`NativeType`] (single scalar, `?T`, or a union of the
/// four scalars + `false`/`true`/`null`), or `None` for unsupported types. A single
/// non-scalar member anywhere (`array`, `mixed`, `iterable`, `callable`, `object`,
/// an intersection, `self`/`static`/`parent`, `void`/`never`) collapses the whole
/// hint to `None` (silent; zero-FP).
pub(crate) fn lower_hint(hint: &Hint<'_>, rc: &RefResolver) -> Option<NativeType> {
    let mut members = Vec::new();
    let mut nullable = false;
    lower_hint_into(hint, rc, &mut members, &mut nullable)?;
    // A hint with no non-null members (standalone `null`) is not modeled.
    if members.is_empty() {
        return None;
    }
    Some(NativeType { members, nullable })
}

/// Accumulate a hint's members into `members`, recording `null` in `nullable`.
/// Returns `None` (propagated up) the moment any part is a type Steins does not
/// model, collapsing the whole hint to silence.
fn lower_hint_into(
    hint: &Hint<'_>,
    rc: &RefResolver,
    members: &mut Vec<TypeMember>,
    nullable: &mut bool,
) -> Option<()> {
    match hint {
        Hint::Integer(_) => members.push(TypeMember::Scalar(ScalarType::Int)),
        Hint::Float(_) => members.push(TypeMember::Scalar(ScalarType::Float)),
        Hint::String(_) => members.push(TypeMember::Scalar(ScalarType::String)),
        Hint::Bool(_) => members.push(TypeMember::Scalar(ScalarType::Bool)),
        Hint::False(_) => members.push(TypeMember::BoolLiteral(false)),
        Hint::True(_) => members.push(TypeMember::BoolLiteral(true)),
        Hint::Null(_) => *nullable = true,
        // A class / interface / enum name (ADR-0043): resolve to its FQN and join
        // the union as an `Instance` member — lowercase-normalized for matching,
        // source-cased for diagnostics. `self`/`static`/`parent` are their own hint
        // variants, not `Hint::Identifier`, and stay in the silence arm below
        // because late-static binding is unsupported (ADR-0043).
        Hint::Identifier(id) => {
            let display = rc.class_display_fqn(&name_ref(id));
            members.push(TypeMember::Instance { fqn: display.to_ascii_lowercase(), display });
        }
        Hint::Nullable(n) => {
            *nullable = true;
            lower_hint_into(n.hint, rc, members, nullable)?;
        }
        Hint::Union(u) => {
            lower_hint_into(u.left, rc, members, nullable)?;
            lower_hint_into(u.right, rc, members, nullable)?;
        }
        Hint::Parenthesized(p) => lower_hint_into(p.hint, rc, members, nullable)?,
        // An intersection of object types (`A&B&…`, ADR-0043): collect every
        // conjunct's resolved class into one conjunctive `InstanceInter` member.
        // Any non-class conjunct collapses the whole hint to silence via the `?`.
        Hint::Intersection(_) => {
            let mut classes = Vec::new();
            collect_intersection_classes(hint, rc, &mut classes)?;
            members.push(TypeMember::InstanceInter(classes));
        }
        // `array`, `mixed`, `iterable`, `callable`, `object`, `self`/`static`/
        // `parent`, `void`/`never` → silence.
        _ => return None,
    }
    Some(())
}

/// Accumulate the resolved classes of an intersection hint into `out`. Recurses
/// through nested `Intersection`/`Parenthesized` nodes; each leaf must be a
/// class/interface identifier (PHP forbids scalar/`null` intersection members).
/// Returns `None` — collapsing the whole hint to silence — the moment a leaf is
/// anything other than a class name.
fn collect_intersection_classes(
    hint: &Hint<'_>,
    rc: &RefResolver,
    out: &mut Vec<ClassRef>,
) -> Option<()> {
    match hint {
        Hint::Intersection(i) => {
            collect_intersection_classes(i.left, rc, out)?;
            collect_intersection_classes(i.right, rc, out)?;
        }
        Hint::Parenthesized(p) => collect_intersection_classes(p.hint, rc, out)?,
        Hint::Identifier(id) => {
            let display = rc.class_display_fqn(&name_ref(id));
            out.push(ClassRef { fqn: display.to_ascii_lowercase(), display });
        }
        _ => return None,
    }
    Some(())
}
