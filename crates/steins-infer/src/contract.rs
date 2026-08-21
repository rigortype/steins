//! PHPDoc declared-contract acceptance (ADR-0029 / ADR-0030 relation #1): pure set
//! semantics, no coercion, trinary judgment where only a definite `No` is ever
//! reported. Hosts the contract value (`CVal`), the declared envelopes and their
//! template shadow, the generic carry, and the project-wide is-a oracle.

use std::collections::{HashMap, HashSet};

use steins_contract::{ContractTy, normalize};
use steins_domain::{Certainty, PhpStr, Val};
use steins_phpdoc::{AssertKind, Type as PType, TagKind, Variance, parse_type, scan_docblock};
use steins_phpdoc::ast::{ConditionalSubject, ConstExpr, TypeKind as PKind, StringLit};
use steins_syntax::{
    ArgValue, NameRef, NativeType, NormKey, RefKind, ScalarType, StaticClass, TypeMember,
    normalize_array,
};

use crate::{Cx, Folder, Known, Store, contract_touches_class, val_of};
use crate::builtin_returns::store_holds_resource;
use crate::generics::{
    accepts_carried_ty, accepts_shape, carry_for_owner, check_arraylike, domain_key,
    template_variances,
};
use crate::untyped::is_template_type;

// ---------------------------------------------------------------------------
// PHPDoc declared-contract acceptance (ADR-0029/0030 relation #1).
//
// A separate acceptance relation from the runtime one above: **pure set
// semantics, NO coercion** (a numeric string `"5"` does NOT satisfy `int`). The
// judgment is trinary — `Yes`/`No`/`Maybe` — and only a definite `No` (proven
// non-membership) is ever reported; `Maybe` is silent (the zero-FP side).
// ---------------------------------------------------------------------------

/// Intersection-style combine: `No` dominates, then `Maybe`, else `Yes`. Used
/// when *every* sub-obligation must hold (element/key membership, shape items).
/// This is exactly [`Certainty::and`], kept as a free function for the existing
/// call sites.
pub(crate) fn combine(a: Certainty, b: Certainty) -> Certainty {
    a.and(b)
}

/// A convenience alias inside this module: the phpdoc contract acceptance code
/// (ADR-0030) was written against a local `Tri`; it now shares the one project-wide
/// [`Certainty`] type (ADR-0031 — one trinary, never parallel ones).
use Certainty as Tri;

/// A proven value in contract terms: a scalar literal, an array of proven values
/// (normalized keys), or an object of an exact class (a `New` fact).
///
/// An object additionally carries its **class-level generic type arguments**
/// (ADR-0032 tier 3, issue #10, extended by issue #294): a set of
/// [`GenericCarry`] edges, each naming the class that *declares* the templates
/// its arguments align to. Empty when the class is non-generic or nothing could
/// be proven — the honest floor (acceptance answers `Maybe`, never a manufactured
/// `No`). Lives in the contract lane, not the object-free value lattice
/// (ADR-0035/0043 §4).
///
/// `Clone`/`PartialEq` exist because the carry survives a variable binding on
/// the heap ([`HeapObj::targs`], issue #295): cloned out at every use, compared
/// for identity at a branch join.
///
/// [`HeapObj::targs`]: crate::HeapObj::targs
#[derive(Clone, PartialEq)]
pub(crate) enum CVal {
    Scalar(ArgValue),
    Array(Vec<(NormKey, CVal)>),
    Object(String, Vec<GenericCarry>),
    /// A legacy PHP **resource** handle (ADR-0056 §8). Carries nothing: there is
    /// no resource hierarchy to name and the open/closed state is not modeled, so
    /// the kind IS the whole fact — which is also why it needs no exactness flag
    /// where [`CVal::Object`] does. Being a resource is never a lower bound.
    Resource,
}

/// One class-level generic parameterization an object carries: the FQN of the class
/// that **declares** the templates, plus one argument per declared template in
/// declaration order (issue #294).
///
/// Naming the owner is what makes the carry survive inheritance. Stage 1's carry
/// was a bare positional vector implicitly aligned to the object's *own* class;
/// that breaks for `final class IntBox extends Box` with `@extends Box<int>`,
/// where the object is `IntBox` but the templates are `Box`'s. Acceptance looks
/// up the edge whose owner **is** the class the contract names, silent when none
/// matches — never comparing one class's arguments against another's parameters.
#[derive(Clone, PartialEq)]
pub(crate) struct GenericCarry {
    /// The class declaring the `@template`s `args` aligns to (resolved FQN).
    pub(crate) owner: String,
    /// One argument per declared template, in declaration order.
    pub(crate) args: Vec<CArg>,
    /// The file whose namespace/`use` scope the argument *types* were written in,
    /// with the offset that picks it — `None` for a value carry, which needs no
    /// resolution context. Class names inside a [`CArg::Ty`] are resolved here.
    pub(crate) site: Option<(usize, u32)>,
}

impl GenericCarry {
    /// Whether every argument is a **declared** type ([`CArg::Ty`]) — an
    /// inheritance edge's `@extends Box<int>`, or a declared parameter's
    /// `@param Box<int> $b` (issue #388). Such a carry states what the author wrote
    /// about the class, which no method call changes and no lack of exactness
    /// weakens, so it survives a sweep and reads off a lower-bound receiver.
    ///
    /// A mixed carry can't occur (all-`Val` from a `new` site, or all-`Ty` from a
    /// declaration), but the predicate is written over the args to stay correct if
    /// one ever does.
    pub(crate) fn is_declared(&self) -> bool {
        self.args.iter().all(|a| matches!(a, CArg::Ty(_)))
    }
}

/// One parameterized inheritance edge as written — [`Cx::inheritance_edge_types`]'s
/// element, the pre-lowering half of a [`GenericCarry`] (issue #361).
///
/// Same gates, same owner keying; only the arguments differ, and they differ
/// because the two readers ask different questions of them. `site` is the class
/// docblock's own `(file, offset)`, which the arguments' class names were written
/// against — carrying it is what lets a reader lift an argument out of this
/// declaration without changing what its names mean.
pub(crate) struct InheritanceEdge {
    pub(crate) owner: String,
    pub(crate) args: Vec<PType>,
    pub(crate) site: (usize, u32),
}

/// One carried type argument. Two provenances: a `new` site proves a **value**
/// flowed in (tier-1 propagation feeding tier 3), an inheritance edge states a
/// **type** the author wrote.
///
/// The distinction is also what the binding-carry sweep reads (issue #295): a
/// `Val` is a fact about the values one object holds, invalidated by a mutating
/// receiver call; a `Ty` is declared and sweep-immune.
#[derive(Clone, PartialEq)]
pub(crate) enum CArg {
    /// A proven value from the `new` site (`new Box('x')` → `'x'`).
    Val(CVal),
    /// A type written on an inheritance edge (`@extends Box<int>` → `int`), lowered
    /// but with its class names still spelled as written (resolved against
    /// [`GenericCarry::site`] at the point of use).
    Ty(steins_contract::ContractTy),
}

/// The `@param`/`@return` phpdoc envelopes parsed off one declaration's docblock.
pub(crate) struct Envelopes {
    /// Parameter name (no `$`) → declared phpdoc type.
    pub(crate) params: Vec<(String, PType)>,
    pub(crate) ret: Option<PType>,
    /// Parameter names (no `$`) that an assertion tag (`@phpstan-assert` &c.)
    /// targets on this declaration — the function is an **assertion helper** for
    /// them (see [`check_phpdoc_param`]). Property/`$this->…` targets excluded
    /// (say nothing about a call-site argument).
    ///
    /// [`check_phpdoc_param`]: crate::generics::check_phpdoc_param
    assert_params: HashSet<String>,
    /// Full assertion specs on this declaration (Feature D): asserted type applied
    /// to the caller's env after a call (`Always`), or in guard position
    /// (`IfTrue`/`IfFalse`). Property/`$this` targets excluded (as above).
    pub(crate) asserts: Vec<AssertSpec>,
}

/// One `@phpstan-assert[-if-true|-if-false] [!]<type> $param` spec (Feature D).
pub(crate) struct AssertSpec {
    /// Target parameter name (no `$`).
    pub(crate) param: String,
    /// The asserted phpdoc type.
    pub(crate) ty: PType,
    /// Unconditional / conditional-on-true / conditional-on-false.
    pub(crate) kind: AssertKind,
    /// The negated form (`@phpstan-assert !T $x`): asserts NOT `T`.
    pub(crate) negated: bool,
}

impl Envelopes {
    pub(crate) fn param(&self, name: &str) -> Option<&PType> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Whether `name` is an assertion target on this declaration, in which case its
    /// `@param` states a **post**-condition and checking arguments against it is a
    /// category error (see [`check_phpdoc_param`]).
    ///
    /// [`check_phpdoc_param`]: crate::generics::check_phpdoc_param
    pub(crate) fn is_assert_target(&self, name: &str) -> bool {
        self.assert_params.contains(name)
    }

    /// Neutralize every envelope identifier naming a template from `shadow` (issue
    /// #5): a `@template`-declared name is a template parameter, not a class, and
    /// must not lower to a class contract that would reject real arguments. Applied
    /// in two idempotent stages — [`parse_envelopes`] applies this declaration's own
    /// `@template` names, a member-check site applies the enclosing class-like's.
    pub(crate) fn shadow_templates(&mut self, shadow: &TemplateShadow) {
        if shadow.is_empty() {
            return;
        }
        for (_, t) in &mut self.params {
            neutralize_templates(t, shadow);
        }
        if let Some(t) = &mut self.ret {
            neutralize_templates(t, shadow);
        }
        for s in &mut self.asserts {
            neutralize_templates(&mut s.ty, shadow);
        }
    }

    /// Resolve every `template-type<…>` node in every envelope to the type it
    /// denotes (issue #361) — the declaration-side half of [`Cx::envelopes_of`],
    /// applied to the same three places [`Self::shadow_templates`] rewrites.
    ///
    /// Runs **after** this declaration's own `@template` shadow and **before** the
    /// enclosing class-like's, which is what makes the two template levels come out
    /// right without a second pass: a function-level `T` has already become its
    /// bound or an opaque node, so `template-type<Box<T>, Box, 'T'>` projects
    /// exactly what `@return T` would; a class-level `T` is still an identifier
    /// here, so the projection yields the name and the member site's shadow
    /// neutralizes it afterwards, as it does for any other class-level template.
    pub(crate) fn resolve_template_types(&mut self, cx: &Cx, file: usize, off: u32) {
        for (_, t) in &mut self.params {
            cx.resolve_template_types(t, file, off);
        }
        if let Some(t) = &mut self.ret {
            cx.resolve_template_types(t, file, off);
        }
        for s in &mut self.asserts {
            cx.resolve_template_types(&mut s.ty, file, off);
        }
    }
}

/// The lowercased set of `@template` names a docblock declares — the *shadow set*
/// (issue #5). A name here is a template parameter, not a class, inside the
/// docblock's own `@param`/`@return`/`@var` types (and, when this is a class-like's
/// docblock, inside every member docblock).
///
/// **Case-insensitive by decision.** PHPStan treats template names as
/// case-sensitive, so strictly `@template Model` would not shadow `@param model`.
/// Steins folds case instead: over-shadowing only ever *silences* a diagnostic;
/// the identifier pipeline already normalizes to lowercase; and the only
/// divergence from PHPStan is staying silent where it would still resolve the
/// class, the safe side (ADR-0029).
///
/// Each name also carries its declared **bound**, when that bound is one Steins can
/// stand behind — see [`TemplateShadow`] and [`vocabulary_bound`].
pub(crate) fn template_names_of(docblock: Option<&str>) -> TemplateShadow {
    let Some(text) = docblock else { return TemplateShadow::default() };
    let mut shadow = TemplateShadow::default();
    for decl in steins_phpdoc::scan_template_decls(text) {
        let key = decl.name.to_ascii_lowercase();
        if let Some(bound) = decl.bound.as_deref().and_then(vocabulary_bound) {
            shadow.bounds.insert(key.clone(), bound);
        }
        shadow.names.insert(key);
        // `decl.variance` is scanned (issue #293) but not consumed: a bound is
        // judged the same way whatever the variance. Issue #294's inheritance
        // edges are what read it.
    }
    shadow
}

/// The `@template` names in force over a docblock, each with the *upper bound* it
/// was declared with where Steins reads that bound (issue #293).
///
/// Two things ride together because they are two answers to the same question —
/// "what does this bare identifier mean here?". A name with no usable bound means
/// *opaque*; a name with one means *at most that bound*, which is what ADR-0032
/// tier 1 calls the declared bound "participating as an upper-bound contract".
#[derive(Debug, Clone, Default)]
pub(crate) struct TemplateShadow {
    /// Every declared template name, lowercased.
    names: HashSet<String>,
    /// The subset whose declared bound Steins reads, keyed by the same lowercased
    /// name. Always a subset of `names`.
    bounds: HashMap<String, PType>,
}

impl TemplateShadow {
    /// Whether this docblock declares no templates at all — the overwhelmingly
    /// common case, and the one every shadow stage short-circuits on.
    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Whether `name` (already lowercased) is a declared template name.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Fold another docblock's declarations in — the class-level stage extended by
    /// a member's own `@template` names. A member redeclaring a class-level name
    /// wins, bound and all, which matches PHP shadowing.
    pub(crate) fn extend(&mut self, other: Self) {
        for name in other.names {
            self.bounds.remove(&name);
            self.names.insert(name);
        }
        self.bounds.extend(other.bounds);
    }
}

/// A `@template` bound Steins is willing to substitute for the template: one whose
/// text parses whole and lowers to a **vocabulary** contract — `array`, `int`,
/// `string`, `int|list<int>`, and their kin.
///
/// Deliberately narrow (issue #293). A **class** bound (`@template T of HasName`)
/// declines rather than being half-checked — admitting it would put a class
/// contract on every templated parameter in the corpus, an unmeasured surface
/// beyond the slice that closes `generics_template_bound_array`. `Opaque`,
/// `mixed`, and `object` decline too: none is information a check can act on.
///
/// Soundness is one-directional: whatever binds to `T` inhabits `T`, and `T` is
/// *at most* its bound, so judging against the bound can only miss violations,
/// never manufacture one.
fn vocabulary_bound(text: &str) -> Option<PType> {
    use steins_contract::ContractTy as C;
    let parsed = steins_phpdoc::parse_type(text).ok()?;
    if !parsed.at_end {
        return None; // trailing garbage — the no-envelope outcome (ADR-0029).
    }
    let lowered = steins_contract::lower(&parsed.ty);
    if matches!(lowered, C::Opaque | C::Mixed | C::ObjectAny) || contract_touches_class(&lowered) {
        return None;
    }
    Some(parsed.ty)
}

/// Hand every **child type** of one phpdoc node to `f`, once — the single place
/// the set of positions a phpdoc-type walk descends into is decided (issue #374).
///
/// Six walks over a [`PType`] live in this crate: the `@template` shadow
/// ([`neutralize_templates`]), the bare-identifier collection
/// ([`collect_bare_identifiers`]), the opaque-node test
/// ([`type_has_unsupported`]), the `template-type` rewrite
/// ([`Cx::resolve_template_types`]), the edge-argument qualification
/// ([`Cx::qualify_class_names`]) and the template-mention scan
/// ([`mentioned_templates`]). Written out by hand they disagreed about where they
/// went — the shadow stopped at a `Callable` and a `Conditional`, so a template
/// name written inside `\Closure(): T` was never shadowed and lowered as a class
/// named `T`, the issue #5 false positive one level down. Each walk now does its
/// own work at the node and recurses through here, so a position is either
/// descended into by all of them or by none.
///
/// **Every position holding a type**, and nothing else: the nullable/array
/// element, the union and intersection members, the generic arguments, an
/// offset access's base and offset, an array shape's values and its unsealed
/// tail's value and key, an object shape's values, a callable's parameter and
/// return types, and a conditional's subject (when the subject is a type rather
/// than a `$param` name), target and both branches.
///
/// **Not** the strings that merely *name* something: a [`PKind::Generic`]'s base
/// and a callable's identifier are references written as text, not child nodes, so
/// each walk decides about them itself — the mention scan counts a generic base,
/// the qualification re-spells it, the shadow leaves both alone. Nor a callable's
/// own `<T>` template list, which *declares* names instead of using them.
///
/// [`mentioned_templates`]: crate::mentioned_templates
pub(crate) fn for_each_child_type(ty: &PType, f: &mut dyn FnMut(&PType)) {
    match &ty.kind {
        PKind::Nullable(inner) | PKind::Array(inner) => f(inner),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            for t in types {
                f(t);
            }
        }
        PKind::Generic { args, .. } => {
            for a in args {
                f(&a.ty);
            }
        }
        PKind::OffsetAccess { base, offset } => {
            f(base);
            f(offset);
        }
        PKind::ArrayShape(s) => {
            for it in &s.items {
                f(&it.value);
            }
            if let Some(tail) = &s.unsealed {
                f(&tail.value);
                if let Some(k) = &tail.key {
                    f(k);
                }
            }
        }
        PKind::ObjectShape(items) => {
            for it in items {
                f(&it.value);
            }
        }
        PKind::Callable(c) => {
            for p in &c.params {
                f(&p.ty);
            }
            f(&c.return_type);
        }
        PKind::Conditional(c) => {
            if let ConditionalSubject::Type(t) = &c.subject {
                f(t);
            }
            f(&c.target);
            f(&c.if_type);
            f(&c.else_type);
        }
        PKind::Identifier(_) | PKind::This | PKind::Const(_) | PKind::Unsupported(_) => {}
    }
}

/// [`for_each_child_type`] for the walks that rewrite. The two enumerate the same
/// positions in the same order, and are written adjacently so they stay that way.
fn for_each_child_type_mut(ty: &mut PType, f: &mut dyn FnMut(&mut PType)) {
    match &mut ty.kind {
        PKind::Nullable(inner) | PKind::Array(inner) => f(inner),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            for t in types {
                f(t);
            }
        }
        PKind::Generic { args, .. } => {
            for a in args {
                f(&mut a.ty);
            }
        }
        PKind::OffsetAccess { base, offset } => {
            f(base);
            f(offset);
        }
        PKind::ArrayShape(s) => {
            for it in &mut s.items {
                f(&mut it.value);
            }
            if let Some(tail) = &mut s.unsealed {
                f(&mut tail.value);
                if let Some(k) = &mut tail.key {
                    f(k);
                }
            }
        }
        PKind::ObjectShape(items) => {
            for it in items {
                f(&mut it.value);
            }
        }
        PKind::Callable(c) => {
            for p in &mut c.params {
                f(&mut p.ty);
            }
            f(&mut c.return_type);
        }
        PKind::Conditional(c) => {
            if let ConditionalSubject::Type(t) = &mut c.subject {
                f(t);
            }
            f(&mut c.target);
            f(&mut c.if_type);
            f(&mut c.else_type);
        }
        PKind::Identifier(_) | PKind::This | PKind::Const(_) | PKind::Unsupported(_) => {}
    }
}

/// Rewrite every **bare, unqualified** identifier naming a template from `shadow`
/// to its declared bound, or to an opaque node when it has none (issue #5,
/// extended by #293). The neutral node is [`PKind::Unsupported`], which lowers
/// to `ContractTy::Opaque` and rides `accepts` as `Maybe` — the same silence a
/// template gets today when it names no existing class. A bounded template
/// becomes its bound instead (`T` under `@template T of array` reads as
/// `array`), keeping the template's own span so a diagnostic still points at
/// the `@param`. A `\`-qualified or namespaced reference is **never** shadowed.
/// Idempotent; recurses through every composite [`for_each_child_type_mut`]
/// enumerates — a callable's signature and a conditional's branches included
/// (issue #374), which is where the shadow used to stop and leak.
///
/// A substituted bound is **not** re-walked: it is a type this docblock did not
/// write at this position, and the shadow's subject is what the author wrote.
pub(crate) fn neutralize_templates(ty: &mut PType, shadow: &TemplateShadow) {
    if let PKind::Identifier(name) = &mut ty.kind {
        if name.contains('\\') {
            return;
        }
        let key = name.to_ascii_lowercase();
        if let Some(bound) = shadow.bounds.get(&key) {
            ty.kind = bound.kind.clone();
        } else if shadow.contains(&key) {
            let raw = std::mem::take(name);
            ty.kind = PKind::Unsupported(raw);
        }
        return;
    }
    for_each_child_type_mut(ty, &mut |child| neutralize_templates(child, shadow));
}

/// Parse the `@param`/`@return` envelopes from a raw docblock, or `None` when the
/// declaration carries no docblock or no envelope-bearing tag. A tag whose type
/// fails to parse (or carries an `Unsupported` node) contributes no envelope; the
/// other tags are unaffected (ADR-0029). `@var`/`@throws` are out of scope.
///
/// The context-free half of [`Cx::envelopes_of`], and since issue #374 its only
/// caller: every consumer now has a declaration context to read the docblock in.
pub(crate) fn parse_envelopes(docblock: Option<&str>) -> Option<Envelopes> {
    let text = docblock?;
    // A `@phpstan-`/`@psalm-` prefixed tag overrides the plain one for the same
    // target (PHPStan precedence; ADR-0029): a later prefixed tag wins, a plain
    // one never displaces a prefixed one.
    let mut params: Vec<(String, PType)> = Vec::new();
    let mut param_prefixed: HashSet<String> = HashSet::new();
    let mut ret: Option<PType> = None;
    let mut ret_prefixed = false;
    let mut assert_params: HashSet<String> = HashSet::new();
    let mut asserts: Vec<AssertSpec> = Vec::new();
    for tag in scan_docblock(text) {
        // An assertion tag targeting a parameter marks it an assert-helper param
        // (its `@param` is a post-condition; ADR-0030). Property targets are inert.
        // All three kinds (Always/IfTrue/IfFalse) and the negated form exempt alike
        // since the parameter is not *constrained* on entry. Also recorded for
        // post-call application (Feature D).
        if let TagKind::Assert { kind: akind, negated } = tag.kind
            && !tag.property_target
            && let Some(var) = &tag.var_name
        {
            let name = var.trim_start_matches('$').to_owned();
            assert_params.insert(name.clone());
            if let Some(ty) = parse_tag_type(&tag.type_text) {
                asserts.push(AssertSpec { param: name, ty, kind: akind, negated });
            }
            continue;
        }
        match tag.kind {
            TagKind::Param => {
                let Some(var) = &tag.var_name else { continue };
                let name = var.trim_start_matches('$').to_owned();
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                match params.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => {
                        // Replace only if we are not downgrading precedence.
                        if tag.prefixed || !param_prefixed.contains(&name) {
                            slot.1 = ty;
                        }
                    }
                    None => params.push((name.clone(), ty)),
                }
                if tag.prefixed {
                    param_prefixed.insert(name);
                }
            }
            TagKind::Return => {
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                if tag.prefixed || (ret.is_none() && !ret_prefixed) {
                    ret = Some(ty);
                    ret_prefixed = tag.prefixed;
                }
            }
            TagKind::Var | TagKind::Throws => {}
            // Conditional purity is an effect contract (ADR-0063), not a type
            // envelope; `conditional_purity` is its consumer.
            TagKind::ConditionalPurity(_) => {}
            // Assertion tags are consumed above (collected into `assert_params`);
            // they never contribute a `@param`/`@return` envelope.
            TagKind::Assert { .. } => {}
            // The trace annotation (ADR-0074) is a statement-level introspection
            // trigger, never a declaration envelope; its consumer is the
            // statement-docblock emitter, not this seam.
            TagKind::TraceTag => {}
            // Interop envelopes: consumed in Slice 2 (#303)
            TagKind::InteropEnvelope(_) => {}
        }
    }
    // Return an envelope set whenever there is anything to check *or* any assertion
    // to remember: an assert-only docblock still carries the exemption fact.
    if !(!params.is_empty() || ret.is_some() || !assert_params.is_empty()) {
        return None;
    }
    let mut env = Envelopes { params, ret, assert_params, asserts };
    // Shadow this declaration's own `@template` names (issue #5); a member-check
    // site additionally applies the enclosing class-like's class-level templates
    // (idempotent second stage).
    env.shadow_templates(&template_names_of(Some(text)));
    Some(env)
}

/// Parse one tag's type text into a phpdoc [`PType`], or `None` on a parse error
/// or an `Unsupported` node (no envelope — silence is safe).
///
/// **Order matters, and it is fixed by [`parse_envelopes`]:** this runs on the tag
/// text as written, *before* the `@template` shadow rewrites anything. The opaque
/// nodes the shadow (and the `template-type` rewrite) plant are therefore never
/// seen here — were the order the other way round, extending the shadow into a
/// callable signature (issue #374) would have started dropping every
/// `\Closure(): T` envelope instead of merely silencing the class named `T`.
pub(crate) fn parse_tag_type(text: &str) -> Option<PType> {
    let parsed = parse_type(text).ok()?;
    (!type_has_unsupported(&parsed.ty)).then_some(parsed.ty)
}

/// Whether a phpdoc type subtree contains an `Unsupported` node anywhere — a
/// grammar construct kept as raw text, in any position [`for_each_child_type`]
/// reaches. The parser plants none today (it reports an error instead); the nodes
/// this finds are the ones the shadow and the `template-type` decline write, both
/// of which run after its one caller.
pub(crate) fn type_has_unsupported(ty: &PType) -> bool {
    if matches!(ty.kind, PKind::Unsupported(_)) {
        return true;
    }
    let mut found = false;
    for_each_child_type(ty, &mut |child| found = found || type_has_unsupported(child));
    found
}

/// What the declared-side rewrite concluded about one `template-type<…>` node
/// (issue #361). Three outcomes, because two different things are *not* an answer
/// and they must not be confused.
enum Projection {
    /// The named template argument, decided from declarations — the node becomes it.
    Resolved(PKind),
    /// Nothing here is decidable from declarations, and nothing later will decide
    /// it either: the node becomes `Unsupported`, lowering to `Opaque` and legible
    /// as declined.
    Declined,
    /// The subject is a **template name**, so the answer lives at a call site, not
    /// in a declaration. The node is left exactly as written — it already floors to
    /// `Opaque` (issue #360), and the carry readers (#362/#363) need to see the
    /// spelling that a rewrite would have erased.
    Deferred,
}

impl<'a> Cx<'a> {
    /// [`parse_envelopes`] plus the declared-side `template-type` rewrite (issue
    /// #361) — the one constructor every envelope consumer with a declaration
    /// context in hand should use.
    ///
    /// `file`/`off` locate the docblock's namespace and `use` scope: the owner
    /// argument is a class *reference*, and which class it names is exactly the
    /// question those two answer. Every consumer supplies them — the last site that
    /// could not, an inherited constructor's `@param`, was reached by teaching
    /// [`Cx::find_ctor`] to report the file that declared it (issue #374).
    pub(crate) fn envelopes_of(&self, docblock: Option<&str>, file: usize, off: u32) -> Option<Envelopes> {
        let mut env = parse_envelopes(docblock)?;
        env.resolve_template_types(self, file, off);
        Some(env)
    }

    /// Rewrite every `template-type<Subject, Owner, 'TName'>` node in one phpdoc
    /// type to the type it denotes (issue #361), in place. Idempotent, and applied
    /// where envelopes are built rather than where they are lowered: finding
    /// `'TName'`'s position needs the owner's `@template` list out of the project
    /// index, which the contract lane has no access to (ADR-0030's one-relation
    /// discipline — this is a rewrite, not a second evaluator).
    ///
    /// Three subject shapes resolve, and each is a *declaration* reading:
    ///
    /// - **A spelled parameterization of the owner** — `template-type<Box<int>,
    ///   Box, 'T'>` is `int`, positionally, by the owner's own template order.
    /// - **A one-level inheritance edge to the owner** — `IntBox` declaring
    ///   `@extends Box<int>` gives `int`. One level, no walk: a subject that
    ///   reaches the owner through a generic intermediate is a *substitution*
    ///   problem, and a class declaring its own `@template`s is the case ADR-0032's
    ///   amendment already settles the other way ("own templates win").
    /// - **The owner parameterized by a template name** — `template-type<Box<T>,
    ///   Box, 'T'>` is `T`, whatever `T` has become by the time this runs.
    ///
    /// Everything else declines to `Unsupported`, never to a manufactured class:
    /// an unknown owner, a template name the owner does not declare, an arity
    /// disagreement between the spelled arguments and that list, an unrelated
    /// subject, a union/shape/callable subject. Arguments are rewritten before the
    /// node itself, so a nested utility resolves inside-out.
    ///
    /// Variance does **not** gate a projection. `@template-covariant T` states what
    /// the author expects of *substitution*, which is why #294 gates acceptance on
    /// it; reading an argument out by position asks nothing about substitution at
    /// all.
    pub(crate) fn resolve_template_types(&self, ty: &mut PType, file: usize, off: u32) {
        for_each_child_type_mut(ty, &mut |child| self.resolve_template_types(child, file, off));
        // The node itself, after its arguments (inside-out).
        let PKind::Generic { base, args } = &ty.kind else { return };
        if !is_template_type(base, args.len()) {
            return;
        }
        match self.project_template_type(args, file, off) {
            Projection::Resolved(kind) => ty.kind = kind,
            Projection::Declined => {
                // The written spelling is kept as the opaque node's text, so the
                // dump surface and every renderer still say what was declined.
                let raw = ty.to_string();
                ty.kind = PKind::Unsupported(raw);
            }
            Projection::Deferred => {}
        }
    }

    /// The verdict for one `template-type<…>` node, given its three arguments —
    /// the whole decision procedure of [`Self::resolve_template_types`].
    fn project_template_type(
        &self,
        args: &[steins_phpdoc::ast::GenericArg],
        file: usize,
        off: u32,
    ) -> Projection {
        // The owner: a class reference, and one that must declare templates for a
        // positional index into them to mean anything.
        let PKind::Identifier(owner_name) = &args[1].ty.kind else { return Projection::Declined };
        let owner_fqn = self.resolve_pclass(file, off, owner_name);
        let Some((_, od)) = self.find_class(&owner_fqn) else { return Projection::Declined };
        let names = od
            .docblock
            .as_deref()
            .map(steins_phpdoc::scan_template_names)
            .unwrap_or_default();
        if names.is_empty() {
            return Projection::Declined;
        }
        // The template name: a quoted literal, matched exactly first. The
        // case-insensitive retry is the same concession `template_names_of` makes —
        // PHPStan's names are case-sensitive, and folding case can only ever pick a
        // template the author plainly meant.
        let PKind::Const(ConstExpr::Str(StringLit::Single(want) | StringLit::Double(want))) =
            &args[2].ty.kind
        else {
            return Projection::Declined;
        };
        let Some(i) = names
            .iter()
            .position(|n| n == want)
            .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(want)))
        else {
            return Projection::Declined;
        };
        match &args[0].ty.kind {
            // (a) The owner, parameterized right here. Same docblock, same
            // namespace scope — the argument needs no re-spelling to travel.
            PKind::Generic { base, args: spelled } => {
                if class_key(&self.resolve_pclass(file, off, base)) != class_key(&owner_fqn)
                    || spelled.len() != names.len()
                {
                    return Projection::Declined;
                }
                Projection::Resolved(spelled[i].ty.kind.clone())
            }
            // (b) A bare class name, or (c) a template name.
            PKind::Identifier(subject) => {
                let subject_fqn = self.resolve_pclass(file, off, subject);
                if !self.is_known_class(&subject_fqn) {
                    // Not a class at all: a function- or class-level template, whose
                    // argument is only known where a value flowed in. Left as
                    // written for #362/#363.
                    return Projection::Deferred;
                }
                if class_key(&subject_fqn) == class_key(&owner_fqn) {
                    // The owner itself, unparameterized: it has no argument to give.
                    return Projection::Declined;
                }
                if self.declares_templates(&subject_fqn) {
                    // A generic subject reaching the owner through its own templates
                    // is substitution, not lookup (ADR-0032: own templates win, and
                    // the walk does not recurse).
                    return Projection::Declined;
                }
                let mut matching = self
                    .inheritance_edge_types(&subject_fqn)
                    .into_iter()
                    .filter(|e| class_key(&e.owner) == class_key(&owner_fqn));
                let Some(edge) = matching.next() else { return Projection::Declined };
                if matching.next().is_some() {
                    return Projection::Declined; // two edges to one owner: say nothing.
                }
                let (efile, eoff) = edge.site;
                let mut arg = edge.args[i].clone();
                self.qualify_class_names(&mut arg, efile, eoff);
                Projection::Resolved(arg.kind)
            }
            // (c) again, in the spelling the shadow leaves behind. By the time this
            // runs, [`parse_envelopes`] has already neutralized the declaration's own
            // `@template` names, so a function-level `T` subject is an `Unsupported`
            // node rather than an identifier — the *same* case as a bare template
            // name, and it must be left alone for the same reason. Declining here
            // would rewrite the node and erase the spelling #363 matches on.
            PKind::Unsupported(_) => Projection::Deferred,
            // (d) A union, an intersection, a shape, a callable, `$this`, a
            // literal — no class whose templates could be indexed. PHPStan unions
            // over a union subject's class names; Steins declines in this slice.
            _ => Projection::Declined,
        }
    }

    /// Re-spell every class name in a type lifted out of *another* declaration's
    /// docblock so that it still names the same class where it lands (issue #361).
    ///
    /// An inheritance edge is written in the subclass's file, against that file's
    /// namespace and `use` scope; the envelope receiving the projected argument may
    /// be written anywhere. The carry solves this by remembering the edge's site
    /// ([`GenericCarry::site`]) and resolving late — a spliced AST node has nowhere
    /// to keep one, so the name is made fully qualified instead, which resolves the
    /// same everywhere.
    ///
    /// Only identifiers that name a **known class** in the edge's own context are
    /// touched: `int` and its kin must stay bare or they would stop being keywords,
    /// and an unresolvable name is left alone because qualifying a guess would turn
    /// a silence into a claim.
    fn qualify_class_names(&self, ty: &mut PType, efile: usize, eoff: u32) {
        let qualify = |name: &mut String| {
            if name.starts_with('\\') {
                return;
            }
            let fqn = self.resolve_pclass(efile, eoff, name);
            if self.is_known_class(&fqn) {
                *name = format!("\\{}", fqn.trim_start_matches('\\'));
            }
        };
        // The names this node itself carries: an identifier, and a generic's base.
        // A callable's identifier (`Closure`) is deliberately left alone — it names
        // the callable vocabulary the contract lane matches on, not a class the
        // edge's file could re-spell.
        match &mut ty.kind {
            PKind::Identifier(name) => qualify(name),
            PKind::Generic { base, .. } => qualify(base),
            _ => {}
        }
        for_each_child_type_mut(ty, &mut |child| self.qualify_class_names(child, efile, eoff));
    }

    /// Resolve a call/return value to a proven [`CVal`] (scalars, arrays of proven
    /// values, or a `New` exact-class object), or `None` when not provable.
    pub(crate) fn resolve_cval(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        store: &Store,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<CVal> {
        match value {
            v if v.is_literal() => Some(CVal::Scalar(v.clone())),
            // `new Class(args)` — a proven object of exactly `Class`, carrying the
            // generic parameterizations it can prove: type-argument values flowing
            // in (ADR-0032 tier 3, issue #10) or, for a template-free class, the
            // ancestor parameterization its `@extends`/`@implements` documents
            // (issue #294). Empty carry when non-generic / unprovable (FP-safe).
            ArgValue::New(class_ref, args, _) if !poisoned => {
                let class = self.class_fqn(class_ref);
                let carry = self.infer_generic_carry(&class, args, env, store, poisoned, folder);
                Some(CVal::Object(class, carry))
            }
            // ADR-0043 stage 4: an enum case is an object value of its enum class,
            // so it can ride the is-a oracle against enum/interface phpdoc contracts.
            // An enum is never `@template`-parameterized → no generic carry.
            ArgValue::EnumCase(fqn, _) => Some(CVal::Object(fqn.clone(), Vec::new())),
            // A class-const access (`Foo::BAR`, `Suit::Hearts`) resolves env-free:
            // an enum case becomes an object, a literal const its value. `self`/
            // `parent` need an enclosing class, absent here → unresolved (silent).
            ArgValue::ClassConst(sc, name) => match self.resolve_class_const(sc, name, None)? {
                ArgValue::EnumCase(fqn, _) => Some(CVal::Object(fqn, Vec::new())),
                lit => self.resolve_cval(&lit, env, store, poisoned, folder),
            },
            ArgValue::Array(items) => {
                // ADR-0049 A12: the walk knows the project's own PHP minor, so a
                // negative-key literal resolves exactly; only an unreported minor
                // over such a literal declines.
                let normalized = normalize_array(items, self.php_minor)?;
                let mut out = Vec::with_capacity(normalized.len());
                for (k, v) in normalized {
                    out.push((k, self.resolve_cval(&v, env, store, poisoned, folder)?));
                }
                Some(CVal::Array(out))
            }
            ArgValue::Var(name) if !poisoned => {
                if let Some(k) = env.get(name) {
                    // A `OneOf` fact is not one proven value → not a `CVal`.
                    let v = k.singleton()?;
                    self.resolve_cval(&v, env, store, poisoned, folder)
                } else if store_holds_resource(store, name) {
                    Some(CVal::Resource)
                } else if store.is_exact(name) {
                    // Only an EXACT object becomes a `CVal::Object` (audit G1): the
                    // phpdoc-acceptance consumer draws a No-side `is_a` conclusion,
                    // which a lower-bound `$this` would make unsound. An inexact
                    // object stays unresolved (silent).
                    //
                    // Generic type arguments DO travel through a variable binding
                    // (issue #295): the allocation records them, so
                    // `$x = new Box('x'); f($x)` judges both halves. A receiver
                    // method call has already swept the value half
                    // (`Store::sweep_targs`), keeping a post-mutation `f($x)` silent
                    // rather than convicting it on a stale argument.
                    store
                        .class_of(name)
                        .map(|c| CVal::Object(c.to_owned(), store.targs_of(name).to_vec()))
                } else {
                    None
                }
            }
            // The resolved value goes back through this function rather than
            // straight into `CVal::Scalar`, exactly as the `Var` arm above sends
            // its singleton back (issue #329). Previously `.map(CVal::Scalar)`
            // wrapped a resolved `Val::Array` in the scalar carrier, so
            // `take(array_values(['x']))` was convicted where the identical
            // `take(['x'])` was silent — same value, different provenance.
            ArgValue::Call(..) => {
                let lit = self.resolve_literal(value, env, poisoned, folder)?;
                self.resolve_cval(&lit, env, store, poisoned, folder)
            }
            _ => None,
        }
    }

    /// Resolve a phpdoc class name to its FQN in the callee file `cfile`'s context
    /// (offset `coff` picks the namespace/use scope where the docblock was written).
    pub(crate) fn resolve_pclass(&self, cfile: usize, coff: u32, name: &str) -> String {
        let raw = name.trim_start_matches('\\').to_owned();
        let kind = if name.starts_with('\\') {
            RefKind::FullyQualified
        } else if raw.contains('\\') {
            RefKind::Qualified
        } else {
            RefKind::Unqualified
        };
        self.units[cfile].tree.resolve_class_fqn(&NameRef { raw, kind, offset: coff })
    }

    /// Whether `fqn` names a **known class** — a Unique project class or a
    /// catalogued builtin (ADR-0043 stage 4), via the same closure predicate the
    /// is-a oracle uses ([`Self::ancestors_of`] returns `Some`). Only a known
    /// class may make a proven scalar a definite non-member; an unresolved bare
    /// identifier stays silent.
    pub(crate) fn is_known_class(&self, fqn: &str) -> bool {
        self.ancestors_of(fqn.trim_start_matches('\\')).is_some()
    }

    // -----------------------------------------------------------------------
    // ADR-0043 stage 3 — native object acceptance (definite-No opening).
    // -----------------------------------------------------------------------

    /// The proven exact class (namespace-resolved FQN) of an object-valued
    /// [`ArgValue`], or `None` when it is not a proven object. `New` resolves its
    /// written class reference in this file's context (matching the ADR-0036 heap
    /// `class_of`); an `EnumCase` already carries the resolved enum FQN.
    pub(crate) fn proven_object_class(&self, v: &ArgValue) -> Option<String> {
        match v {
            ArgValue::New(r, _, _) => Some(self.class_fqn(r)),
            ArgValue::EnumCase(fqn, _) => Some(fqn.clone()),
            _ => None,
        }
    }

    /// ADR-0043 stage 3 — does an object of exact class `class_fqn` **provably
    /// violate** the native type `ty`? A definite-No: `true` only when *every*
    /// union member definitively rejects an object of that class (any `Unknown`
    /// or accepting member makes the whole verdict silent). `nullable` is
    /// irrelevant to an object value — an object is never `null`.
    pub(crate) fn object_is_type_error(&self, ty: &NativeType, class_fqn: &str) -> bool {
        ty.members.iter().all(|m| self.member_rejects_object(m, class_fqn))
    }

    /// Whether the native type `ty` **definitively rejects** a resource
    /// (ADR-0056 §8) — every union member does, and `ty.nullable` is irrelevant
    /// because no resource is null.
    ///
    /// Stronger than [`Self::object_is_type_error`]: that version demotes `string`
    /// to a strict-mode-only reject, since a `__toString` object coerces into
    /// `string` in coercive mode. **There is no `__toResource`.** PHP offers a
    /// resource no coercion path into any scalar, in either mode — probed at 8.5.9:
    ///
    /// ```text
    /// function b(bool $x){} … b($h);  → must be of type bool, resource given
    /// function i(int $x){}  … i($h);  → must be of type int, resource given
    /// function s(string $x){} … s($h);→ must be of type string, resource given
    /// ```
    ///
    /// (all from a file with NO `declare(strict_types=1)`), so this predicate
    /// never consults [`Self::strict`] — the finding is mode-independent.
    ///
    /// An object member rejects too — a resource is not an instance of anything.
    /// This is the opposite of the `Maybe` the resource *contract* gives an
    /// object value (`unrepresentable_verdict`): there the docblock is the
    /// suspect (PHP 8 left `@param resource $ch` behind on params that now take
    /// a `CurlHandle`), here the *value* is proven, and a native `\CurlHandle`
    /// parameter handed a real `fopen()` stream is a genuine TypeError.
    pub(crate) fn resource_is_type_error(&self, ty: &NativeType) -> bool {
        ty.members.iter().all(|m| match m {
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => true,
            TypeMember::Instance { .. } | TypeMember::InstanceInter(_) => true,
        })
    }

    /// Whether a single union `member` **definitively rejects** an object of exact
    /// class `class_fqn`.
    ///
    /// Verified against PHP 8.5.8 (`php -r`):
    /// - `int`/`float`/`bool` (and `false`/`true` literals): no object — **not
    ///   even one with `__toString`** — coerces into these in either mode; passing
    ///   any object `TypeError`s → an unconditional definite reject.
    /// - `string`: a `__toString` object *does* coerce to a `string` parameter in
    ///   **coercive** mode (no error), while a plain object and **any** object in
    ///   **strict** mode `TypeError`. Steins does not (yet) prove the *absence* of
    ///   `__toString` across a class hierarchy, so a `string` member is a definite
    ///   reject only in **strict** mode; in coercive mode it stays silent
    ///   (Unknown), the FP-safe choice.
    /// - `Instance { fqn, .. }`: rejects iff the trinary is-a oracle proves non-membership
    ///   (`IsA::No`); `Yes` accepts and `Unknown` (incomplete hierarchy) is silent.
    fn member_rejects_object(&self, m: &TypeMember, class_fqn: &str) -> bool {
        match m {
            TypeMember::Instance { fqn, .. } => self.is_a(class_fqn, fqn) == IsA::No,
            // An intersection (`A&B&…`) demands membership in **every** conjunct,
            // so it definitively rejects the moment the is-a oracle proves
            // non-membership in **any** one — an incomplete hierarchy on the rest
            // stays silent.
            TypeMember::InstanceInter(cs) => {
                cs.iter().any(|c| self.is_a(class_fqn, &c.fqn) == IsA::No)
            }
            TypeMember::Scalar(ScalarType::String) => self.strict(),
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => true,
        }
    }

    /// Resolve a [`StaticClass`] class-expression to its FQN (ADR-0043). `Named`
    /// resolves in this file's namespace context (source-cased); `self`/`parent`
    /// need the enclosing class; `static` (late static binding) stays unproven.
    fn resolve_static_class_fqn(&self, sc: &StaticClass, enclosing: Option<&str>) -> Option<String> {
        match sc {
            StaticClass::Named(r) => Some(self.class_fqn(r)),
            StaticClass::SelfKw => enclosing.map(str::to_owned),
            StaticClass::Parent => self.parent_fqn(enclosing?),
            StaticClass::Static => None,
        }
    }

    /// Resolve a class-constant / enum-case access `Class::NAME` to a proven value
    /// (ADR-0043 §2), or `None` when unresolvable/non-literal (→ silent).
    ///
    /// - `Class::class` → the FQN **string** literal. Only a written name
    ///   (`Named`) is resolved, preserving declared source casing (verified
    ///   against PHP 8.5.8 — `::class` yields the `use`-target's declared casing).
    ///   `self`/`parent`/`static::class` resolve only to the lowercase-normalized
    ///   index FQN, so emitting them risks a wrong-case string — left unproven.
    /// - An enum case → an [`ArgValue::EnumCase`] **object** value of the enum
    ///   class (never its backing scalar — an enum case is an object).
    /// - A class constant with a literal initializer → that literal, resolved
    ///   through the class/interface hierarchy (child overrides parent).
    fn resolve_class_const(&self, sc: &StaticClass, name: &str, enclosing: Option<&str>) -> Option<ArgValue> {
        if name.eq_ignore_ascii_case("class") {
            return match sc {
                StaticClass::Named(r) => {
                    Some(ArgValue::Str(PhpStr::from(self.class_fqn(r).trim_start_matches('\\'))))
                }
                _ => None,
            };
        }
        let fqn = self.resolve_static_class_fqn(sc, enclosing)?;
        if let Some((_, cd)) = self.find_class(&fqn)
            && cd.is_enum
            && cd.enum_cases.iter().any(|c| c.name == name)
        {
            return Some(ArgValue::EnumCase(cd.fqn.clone(), name.to_owned()));
        }
        self.resolve_const_literal(&fqn, name)
    }

    /// The **normalized enum FQN** `sc::case` names, when `case` is one of a
    /// completely-known case set (issue #429). `None` for everything else: a
    /// class constant that is not a case, an enum whose declaration
    /// [`Cx::enum_case_names`] refuses to complete, an unresolvable `static::`.
    ///
    /// Asking through `enum_case_names` rather than the decl directly is what
    /// keeps the guard and the seed on one gate: a lane that was never expanded
    /// must not be subtracted from as though it had been.
    pub(crate) fn resolve_enum_case(
        &self,
        sc: &StaticClass,
        case: &str,
        enclosing: Option<&str>,
    ) -> Option<String> {
        let fqn = self.resolve_static_class_fqn(sc, enclosing)?;
        let (_, cd) = self.find_class(&fqn)?;
        self.enum_case_names(&fqn)?.iter().any(|c| c == case).then(|| cd.fqn.clone())
    }

    /// Resolve a class constant `fqn::name` to its literal value by walking the
    /// class's own consts, its directly-implemented interfaces' consts, then its
    /// parent chain (most-derived first, matching PHP constant override). Returns
    /// `None` on an unresolvable node or a name with no proven literal.
    fn resolve_const_literal(&self, fqn: &str, name: &str) -> Option<ArgValue> {
        let mut cur = fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(cur.to_ascii_lowercase()) {
                return None;
            }
            let (file, cd) = self.find_class(&cur)?;
            if let Some((_, v)) = cd.consts.iter().find(|(n, _)| n == name) {
                return Some(v.clone());
            }
            for iref in &cd.implements {
                let ifqn = self.units[file].tree.resolve_class_fqn(iref);
                if let Some((_, icd)) = self.find_class(&ifqn)
                    && let Some((_, v)) = icd.consts.iter().find(|(n, _)| n == name)
                {
                    return Some(v.clone());
                }
            }
            let pref = cd.parent.as_ref()?;
            cur = self.units[file].tree.resolve_class_fqn(pref);
        }
    }

    /// Resolve an [`ArgValue`] to a proven value **without an environment** — a
    /// self-evident literal, a proven object (`new` / enum case), or a resolved
    /// class constant (ADR-0043). Feeds the native definite-No checks at the
    /// call/return sites. `enclosing` supplies `self`/`parent` for class-const
    /// resolution inside a method body (`None` at file scope).
    pub(crate) fn resolve_static_value(&self, v: &ArgValue, enclosing: Option<&str>) -> Option<ArgValue> {
        match v {
            _ if v.is_literal() => Some(v.clone()),
            ArgValue::New(..) | ArgValue::EnumCase(..) => Some(v.clone()),
            ArgValue::ClassConst(sc, name) => self.resolve_class_const(sc, name, enclosing),
            _ => None,
        }
    }

    /// The **trinary is-a oracle** (ADR-0043 §3): is a value of exact class
    /// `sub_fqn` an instance of `super_fqn`?
    ///
    /// - **`Yes`** — a supertype path exists: parent chain plus transitive
    ///   `implements` closure. Reflexive (`sub == super` is `Yes`).
    /// - **`No`** — only under a **completely enumerated hierarchy**: every
    ///   ancestor edge reachable from `sub` resolved (in-project or catalog
    ///   builtin), and `super` is absent from that closed set — the Certainty
    ///   discipline applied to subtyping.
    /// - **`Unknown`** — the enumeration is incomplete: an ancestor is
    ///   unresolvable, the chain leaves the project into an uncatalogued
    ///   builtin, or `sub`/`super` is itself unknown.
    ///
    /// Enums (ADR-0043): a lowered enum is-a its explicit `implements` plus the
    /// implicit `UnitEnum` interface, and a *backed* enum additionally is-a
    /// `BackedEnum` (which the catalog records as extending `UnitEnum`).
    ///
    /// A `use`d trait does **not** force `Unknown`: a trait adds methods, never
    /// types, so it cannot change the is-a relation — [`Self::ancestors_of`]
    /// ignores trait use.
    pub(crate) fn is_a(&self, sub_fqn: &str, super_fqn: &str) -> IsA {
        self.is_a_tracked(sub_fqn, super_fqn).0
    }

    /// [`Self::is_a`], additionally reporting whether the verdict was **catalog-
    /// backed** — any ancestor edge resolved through the builtin catalog
    /// ([`steins_catalog::builtin_class_supers`]) rather than in-project source.
    /// ADR-0052 A11 reads this: a catalog-backed verdict used for arm deletion is
    /// demoted to `Unknown` on a PHP-minor skew. A purely in-project verdict is
    /// never catalog-backed, so a project's own `A|B` union narrows unaffected.
    fn is_a_tracked(&self, sub_fqn: &str, super_fqn: &str) -> (IsA, bool) {
        let target = super_fqn.trim_start_matches('\\');
        // `Stringable` is implicitly implemented by any class with a `__toString`
        // method (PHP 8.0+), invisible to the explicit parent/`implements` closure.
        // For this target only: a proven `__toString` is a definite `Yes`, and a
        // trait-using class (merged methods unmodeled — might declare
        // `__toString`) forces `Unknown` rather than an unsound `No`.
        let stringable_target = target.eq_ignore_ascii_case("Stringable");
        let mut queue: Vec<String> = vec![sub_fqn.trim_start_matches('\\').to_owned()];
        let mut seen: HashSet<String> = HashSet::new();
        // Whether every ancestor edge inspected so far resolved — the closure
        // condition for a sound `No`. A single unresolvable node taints it.
        let mut complete = true;
        // Whether a visited class may implicitly gain `Stringable` via a trait.
        let mut maybe_stringable = false;
        // Whether any traversed ancestor edge came from the builtin catalog (A11).
        let mut catalog = false;
        while let Some(cur) = queue.pop() {
            if cur.eq_ignore_ascii_case(target) {
                return (IsA::Yes, catalog);
            }
            if !seen.insert(cur.to_ascii_lowercase()) {
                continue;
            }
            if stringable_target
                && let Some((_, cd)) = self.find_class(&cur)
            {
                if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case("__toString")) {
                    return (IsA::Yes, catalog);
                }
                if cd.uses_traits {
                    maybe_stringable = true;
                }
            }
            // An edge resolved through the catalog (not an in-project class) marks
            // the whole verdict catalog-backed.
            let in_project = self.find_class(&cur).is_some();
            match self.ancestors_of(&cur) {
                Some(supers) => {
                    if !in_project {
                        catalog = true;
                    }
                    queue.extend(supers);
                }
                None => complete = false,
            }
        }
        if stringable_target && maybe_stringable {
            return (IsA::Unknown, catalog);
        }
        (if complete { IsA::No } else { IsA::Unknown }, catalog)
    }

    /// The **direct** supertypes (parent + `implements`, plus an enum's implicit
    /// interfaces) of `fqn`, or `None` when `fqn` is an unknown external — which
    /// makes the is-a enumeration incomplete. A resolvable class with no
    /// supertypes returns an empty vector (fully enumerated, a root).
    pub(crate) fn ancestors_of(&self, fqn: &str) -> Option<Vec<String>> {
        if let Some((file, cd)) = self.find_class(fqn) {
            let tree = &self.units[file].tree;
            let mut supers = Vec::new();
            if let Some(pref) = &cd.parent {
                supers.push(tree.resolve_class_fqn(pref));
            }
            for imp in &cd.implements {
                supers.push(tree.resolve_class_fqn(imp));
            }
            if cd.is_enum {
                supers.push("UnitEnum".to_owned());
                if cd.enum_backing.is_some() {
                    supers.push("BackedEnum".to_owned());
                }
            }
            Some(supers)
        } else {
            steins_catalog::builtin_class_supers(fqn)
                .map(|s| s.into_iter().map(str::to_owned).collect())
        }
    }
}

/// The verdict of the trinary is-a oracle ([`Cx::is_a`], ADR-0043 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsA {
    /// A supertype path exists (membership is proven).
    Yes,
    /// The hierarchy is completely enumerated and the target is absent from it
    /// (non-membership is proven under closure).
    No,
    /// The hierarchy is incomplete — no verdict (the FP-safe silence).
    Unknown,
}

/// The **project** is-a oracle for contract-arm subtraction (ADR-0052 N4): the
/// steins-infer implementor of steins-contract's [`normalize::IsaOracle`] seam.
/// Wraps the real trinary hierarchy ([`Cx::is_a_tracked`]) and applies the A11
/// version-skew demotion, keeping steins-contract free of any steins-infer/
/// catalog dependency.
pub(crate) struct ProjectIsa<'c, 'a> {
    pub(crate) cx: &'c Cx<'a>,
    /// Whether a catalog-backed verdict must demote to `Unknown` (A11 skew).
    pub(crate) demote_catalog: bool,
}

impl normalize::IsaOracle for ProjectIsa<'_, '_> {
    fn is_a(&self, sub: &str, sup: &str) -> Certainty {
        let (verdict, catalog) = self.cx.is_a_tracked(sub, sup);
        let c = match verdict {
            IsA::Yes => Certainty::Yes,
            IsA::No => Certainty::No,
            IsA::Unknown => Certainty::Maybe,
        };
        // A11: a decisive but catalog-backed verdict falls to `Unknown` on a minor
        // skew — the arm is kept in both polarities (FP-safe).
        if self.demote_catalog && catalog && c != Certainty::Maybe { Certainty::Maybe } else { c }
    }

    fn is_final(&self, fqn: &str) -> bool {
        // Only an in-project `final` class or enum is provably closed; a builtin
        // (finality untracked) stays open — the positive branch keeps its arm.
        self.cx.find_class(fqn).is_some_and(|(_, cd)| cd.is_final || cd.is_enum)
    }
}

/// Contract acceptance (ADR-0030): does the proven value `v` inhabit the phpdoc
/// type `ty`? Class names in `ty` resolve in the callee file `cfile` at `coff`.
pub(crate) fn accepts(cx: &Cx, cfile: usize, coff: u32, ty: &PType, v: &CVal) -> Tri {
    match &ty.kind {
        PKind::Identifier(name) => accepts_identifier(cx, cfile, coff, name, v),
        // `$this` is intentionally undecided here.
        PKind::This => Tri::Maybe,
        PKind::Nullable(inner) => match v {
            CVal::Scalar(ArgValue::Null) => Tri::Yes,
            _ => accepts(cx, cfile, coff, inner, v),
        },
        // Union: `Yes` if any member accepts, `No` only if all definitely reject.
        //
        // An `unset` member is skipped, not folded (ADR-0087 §5): it states nothing
        // about a value, so its `Maybe` would swallow every sibling's `No` and
        // delete the finding `@param \DateTime $d` reports on the same argument.
        // The member is inert in this position — the value arms of `\DateTime|unset`
        // are `\DateTime`'s, which is §2.1 — and a union of nothing else keeps the
        // bare-`unset` floor below.
        PKind::Union { types, .. } => {
            let (mut any_yes, mut any_maybe) = (false, false);
            let value_arms: Vec<&PType> = types.iter().filter(|t| !is_unset_atom(t)).collect();
            if value_arms.is_empty() {
                return Tri::Maybe;
            }
            for t in value_arms {
                match accepts(cx, cfile, coff, t, v) {
                    Tri::Yes => any_yes = true,
                    Tri::Maybe => any_maybe = true,
                    Tri::No => {}
                }
            }
            if any_yes {
                Tri::Yes
            } else if any_maybe {
                Tri::Maybe
            } else {
                Tri::No
            }
        }
        PKind::Intersection(_) => Tri::Maybe, // class intersections — silent
        // `T[]` — an array (any keys) whose values inhabit `T`.
        PKind::Array(inner) => match v {
            CVal::Array(entries) => {
                let mut r = Tri::Yes;
                for (_, cv) in entries {
                    r = combine(r, accepts(cx, cfile, coff, inner, cv));
                    if r == Tri::No {
                        return Tri::No;
                    }
                }
                r
            }
            _ => Tri::No,
        },
        PKind::Generic { base, args } => accepts_generic(cx, cfile, coff, base, args, v),
        PKind::ArrayShape(shape) => accepts_shape(cx, cfile, coff, shape, v),
        PKind::Const(c) => accepts_const(c, v),
        // Callables, offset-access, conditionals, object-shapes → silent.
        PKind::Callable(_) | PKind::OffsetAccess { .. } | PKind::Conditional(_)
        | PKind::ObjectShape(_) | PKind::Unsupported(_) => Tri::Maybe,
    }
}

/// Whether a phpdoc union member is the `unset` pseudo-type (ADR-0087 §2). Read
/// through `lower_identifier` rather than by spelling, so the case-blindness and
/// the leading-backslash handling are the one table's, not a second one's.
fn is_unset_atom(ty: &PType) -> bool {
    matches!(&ty.kind, PKind::Identifier(name) if steins_contract::lower_identifier(name).is_unset())
}

/// Acceptance for a bare identifier type.
///
/// **One identifier table** (ADR-0030's no-second-relation discipline, ADR-0062 §5
/// — the same convergence [`accepts_shape`] performs for shapes). The keyword
/// vocabulary is *not* restated here: the name is lowered by
/// [`steins_contract::lower_identifier`] — the table the abstract-fact lane in
/// [`check_phpdoc_param`] already lowers through — and the lowered contract is
/// judged against the proven value by [`steins_contract::admits_val`].
///
/// This converges a formerly hand-maintained sibling match on the raw phpdoc AST
/// that had drifted: `non-positive-int`, `numeric`, `int-range<…>`, and the
/// `boolean`/`integer`/`double` synonyms were enforced against an abstract fact
/// but silent against a *proven* value.
///
/// Two judgments stay lane-local — the value domain has no inhabitant for them
/// (ADR-0035/0038 — there is no `Val::Object`):
///
/// * a **class name** — which the one table reports as its `Class` catch-all —
///   rides this crate's trinary is-a oracle and its `is_known_class` gate;
/// * a value the domain cannot express (an **object**, or an array holding one) is
///   judged by [`unrepresentable_verdict`], which reads the *lowered* contract, not
///   the keyword — so it is a leaf judge, not a second table.
///
/// [`check_phpdoc_param`]: crate::generics::check_phpdoc_param
fn accepts_identifier(cx: &Cx, cfile: usize, coff: u32, name: &str, v: &CVal) -> Tri {
    let cty = steins_contract::lower_identifier(name);
    // Not a keyword → a class name, whose judgment this crate owns.
    if matches!(cty, steins_contract::ContractTy::Class(_)) {
        return accepts_class_name(cx, cfile, coff, name, v);
    }
    // Pseudo-type/class precedence (PHPStan's `tryResolvePseudoTypeClassType`): a
    // keyword PHP does not *reserve* — `integer`, `boolean`, `double`, `number`,
    // `closure`, … — is a legal class name, and a class of that name in scope wins
    // over the keyword. `steins-contract` answers the vocabulary half; the
    // registry half is necessarily ours, hence the delegation instead of a
    // straight table replacement.
    //
    // The gate is **in-project declaration**, not `is_known_class` — deliberately
    // narrower than PHPStan's rule: the seeded catalog carries global class-likes
    // colliding with pseudo-types (`number` implements `Stringable`), and letting
    // those shadow would silently turn `@param number` (int|float) into a class
    // contract in every non-namespaced file, an FP from a collision the author
    // never saw. A project declaring its own `class Integer` and writing
    // `@param Integer` means that class — the case the rule exists for. Same
    // in-project/catalog cut as `ProjectIsa::is_final` (ADR-0043 A11).
    if steins_contract::is_shadowable_pseudo_type(name)
        && cx.find_class(&cx.resolve_pclass(cfile, coff, name)).is_some()
    {
        return accepts_class_name(cx, cfile, coff, name, v);
    }
    match cval_as_val(v) {
        Some(val) => steins_contract::admits_val(&cty, &val),
        None => unrepresentable_verdict(&cty, v),
    }
}

/// The conversion seam from the contract lane's proven value into the domain's
/// [`Val`], so the shared acceptance relation can judge it. `None` for a value the
/// domain has no inhabitant for: an object, or an array holding one (ADR-0035/0038
/// — the value lattice is object-free).
fn cval_as_val(v: &CVal) -> Option<Val> {
    match v {
        // The array minor-version question is already settled: a `CVal`'s keys are
        // normalized (`NormKey`), so no next-int guess is needed here.
        CVal::Scalar(s) => val_of(s, None),
        CVal::Array(entries) => entries
            .iter()
            .map(|(k, cv)| cval_as_val(cv).map(|val| (domain_key(k), val)))
            .collect::<Option<Vec<_>>>()
            .map(Val::Array),
        CVal::Object(..) | CVal::Resource => None,
    }
}

/// The lane-local leaf judge for a value the domain cannot represent — an **object**
/// (ADR-0043's world, which the object-free value lattice has no inhabitant for) or
/// an array holding one.
///
/// Reads the **lowered** contract rather than the keyword, so it states only what
/// is true of *every* object, or of an array whose members are unknown — no keyword
/// knowledge duplicated here. `steins-contract` cannot host this: doing so would
/// mean giving the value domain an object inhabitant.
fn unrepresentable_verdict(cty: &steins_contract::ContractTy, v: &CVal) -> Tri {
    use steins_contract::ContractTy as C;
    use steins_contract::MixedCut;
    match v {
        CVal::Object(..) => match cty {
            C::Mixed | C::ObjectAny => Tri::Yes,
            // Both cuts of `mixed` keep every object: not null, and every object
            // truthy since PHP 7 — the arm that keeps `f(new stdClass())` against
            // `@param non-empty-mixed` from being a manufactured `No`.
            C::MixedMinus(_) => Tri::Yes,
            // An object may be `Traversable`, may have `__invoke` — none of it
            // provable from the class name alone.
            C::Opaque | C::IterableOf { .. } | C::CallableTy { .. } | C::StrOpaque => Tri::Maybe,
            // `@param resource $ch` handed an object — ADR-0056 §8.5's named FP
            // channel. An object genuinely is not a resource, so `No` would be
            // true, but overwhelmingly this is a stale docblock from PHP 8's own
            // migration (`curl_init()` returned a resource for twenty years, a
            // `CurlHandle` now) on code that works. The other direction — a
            // proven RESOURCE against a native class parameter — does convict
            // (`resource_is_type_error`): there the value is proven, not the doc.
            C::Resource => Tri::Maybe,
            // Every other lowered form denotes scalars, null, or arrays, of which no
            // object is a member (pure set membership, no coercion — ADR-0030).
            _ => Tri::No,
        },
        // An array with an unrepresentable member: its array-ness is decided, its
        // contents are not, so only the contract's own array-ness answers.
        CVal::Array(entries) => match cty {
            C::Mixed | C::ArrayAny { non_empty: false } => Tri::Yes,
            // An array's falsiness is its emptiness alone, decided here however
            // unrepresentable its members are — no reference to contents needed.
            C::MixedMinus(MixedCut::Null) => Tri::Yes,
            C::MixedMinus(MixedCut::Falsy) => {
                if entries.is_empty() { Tri::No } else { Tri::Yes }
            }
            C::ArrayAny { .. }
            | C::ListOf { .. }
            | C::MapOf { .. }
            | C::IterableOf { .. }
            | C::Shape { .. }
            | C::CallableTy { .. }
            | C::Opaque => Tri::Maybe,
            _ => Tri::No,
        },
        // A resource (ADR-0056 §8). Exact almost everywhere — a leaf with no
        // hierarchy, so only two `Maybe`s and the object arm (FP channel) need care.
        CVal::Resource => match cty {
            C::Mixed | C::Resource => Tri::Yes,
            // Both cuts keep every resource: none is null, and every resource is
            // truthy — a CLOSED one included (`fclose($h); (bool) $h === true` at
            // 8.5.9).
            C::MixedMinus(_) => Tri::Yes,
            // `object` and a named class are where PHP 8's migration left its
            // wreckage. A resource is *not* an object, so `No` would be honest,
            // but the code this fires on is overwhelmingly a stale
            // `@param resource $ch` / `@return CurlHandle` pair straddling the
            // migration. `Maybe` (ADR-0056 §8's named FP channel).
            C::ObjectAny | C::Class(_) => Tri::Maybe,
            // `Opaque` is unknown by definition; `callable` admits a resource in
            // no PHP (`is_callable($h) === false`) — the first stays `Maybe`, the
            // second decides.
            C::Opaque => Tri::Maybe,
            // Every other lowered form denotes scalars, null, arrays or callables,
            // and no resource is a member. PHP rejects a resource at any scalar
            // boundary in both modes (probed at 8.5.9).
            _ => Tri::No,
        },
        // Unreachable in practice: `resolve_cval` yields only literal scalars here.
        // The honest floor, not a verdict.
        CVal::Scalar(_) => Tri::Maybe,
    }
}

/// A class-name type (ADR-0043 stage 4) — the identifier judgment the one table
/// cannot host. Rides the trinary is-a oracle for a proven object value
/// (`Yes`→Yes, `No`→No, `Unknown`→Maybe) and rejects a proven scalar against a
/// *known* class — phpdoc acceptance is pure set membership (ADR-0030 registry 1,
/// no coercion). The `is_known_class` gate is the safety valve: an unresolved bare
/// identifier may be a `@template`/`@phpstan-type` alias denoting a scalar, so it
/// stays silent.
fn accepts_class_name(cx: &Cx, cfile: usize, coff: u32, name: &str, v: &CVal) -> Tri {
    let target = cx.resolve_pclass(cfile, coff, name);
    match v {
        CVal::Object(obj, _) => match cx.is_a(obj, &target) {
            IsA::Yes => Tri::Yes,
            // A definite `No` requires a *known* target: an unresolved name may
            // be a `@template`/`@phpstan-type` alias the object *does* satisfy.
            IsA::No if cx.is_known_class(&target) => Tri::No,
            IsA::No | IsA::Unknown => Tri::Maybe,
        },
        // A resource is a non-instance for the same reason a scalar is, gated the
        // same way (ADR-0056 §8.5). Without this arm the contract layer would be
        // quieter than the proof layer about the same pairing
        // (`resource_is_type_error` convicts on a native `\CurlHandle` param).
        CVal::Scalar(_) | CVal::Resource if cx.is_known_class(&target) => Tri::No,
        // An array is likewise never a class instance, but it is left
        // intentionally undecided here (out of the stage-4 scope).
        _ => Tri::Maybe,
    }
}

/// Acceptance for a literal constant type (`'foo'`, `123`, `1.5`, `true`, …) by
/// value equality; a const-fetch (`Foo::BAR`) is unresolved → silent.
fn accepts_const(c: &ConstExpr, v: &CVal) -> Tri {
    // A const-fetch type (`Foo::BAR`, `self::CONST`, `Suit::Hearts`) is unresolved
    // here, so it must stay silent for *every* value (ADR-0043 stage 4): a
    // returned/passed value that *is* that very constant must never be
    // manufactured into a `No` — guards against firing on
    // `@return self::CONST { return self::CONST; }` tautologies.
    if matches!(c, ConstExpr::Fetch { .. }) {
        return Tri::Maybe;
    }
    let scalar = match v {
        CVal::Scalar(s) => s,
        _ => return Tri::No,
    };
    let yes_no = |b: bool| if b { Tri::Yes } else { Tri::No };
    match c {
        ConstExpr::Int(s) => match (s.parse::<i64>().ok(), scalar) {
            (Some(n), ArgValue::Int(i)) => yes_no(*i == n),
            _ => Tri::No,
        },
        ConstExpr::Float(s) => match (s.parse::<f64>().ok(), scalar) {
            (Some(n), ArgValue::Float(f)) => yes_no(*f == n),
            _ => Tri::No,
        },
        ConstExpr::Str(lit) => match scalar {
            ArgValue::Str(s) => yes_no(s == string_lit_value(lit)),
            _ => Tri::No,
        },
        ConstExpr::True => yes_no(matches!(scalar, ArgValue::Bool(true))),
        ConstExpr::False => yes_no(matches!(scalar, ArgValue::Bool(false))),
        ConstExpr::Null => yes_no(matches!(scalar, ArgValue::Null)),
        ConstExpr::Fetch { .. } => Tri::Maybe,
    }
}

fn string_lit_value(lit: &StringLit) -> &str {
    match lit {
        StringLit::Single(s) | StringLit::Double(s) => s,
    }
}

/// Resolve a `key-of<Foo::MAP>` / `value-of<Foo::MAP>` operand — a class constant
/// holding an array literal — to the sealed [`ContractTy::Shape`] the shared
/// projection reads (census bucket vi, const tier).
///
/// This is an **operand resolver, not a second projection**: it answers only "what
/// array does this reference denote", handing the answer to the same
/// `project_key_of`/`project_value_of` the inline tier uses. Lives here (not in
/// `steins-contract`) because it needs the project index.
///
/// `None` — the honest floor, turned into silence by the caller — whenever any
/// step is unproven: a non-const-fetch operand, an unresolvable class, a constant
/// with no *literal* initializer, a non-array constant, version-dependent runtime
/// keys, or a non-scalar-literal element.
///
/// Deliberately **not** covered: a backed **enum** operand (`value-of<Suit>`). Its
/// backing values are recorded but never read — the const resolver returns an
/// enum case as an *object* by design (ADR-0043 §2), so projecting backing
/// scalars would be new semantics, a named ceiling.
fn const_operand_shape(cx: &Cx, cfile: usize, coff: u32, ty: &PType) -> Option<ContractTy> {
    let PKind::Const(ConstExpr::Fetch { class, name }) = &ty.kind else { return None };
    let fqn = cx.resolve_pclass(cfile, coff, class);
    let ArgValue::Array(items) = cx.resolve_const_literal(&fqn, name)? else { return None };
    let normalized = normalize_array(&items, cx.php_minor)?;
    let mut fields = Vec::with_capacity(normalized.len());
    for (k, v) in normalized {
        let key = match k {
            NormKey::Int(i) => steins_contract::CKey::Int(i),
            NormKey::Str(s) => steins_contract::CKey::Str(s),
        };
        fields.push(steins_contract::CField { key, optional: false, ty: literal_contract(&v)? });
    }
    let non_empty = !fields.is_empty();
    Some(ContractTy::Shape { list: false, fields, sealed: true, non_empty, unsealed: None })
}

/// The literal contract one *proven* array element denotes. `None` for anything
/// that is not a scalar literal (a nested array, an object, an unresolved
/// reference) — which drops the whole operand to silence rather than projecting a
/// partial key/value set.
pub(crate) fn literal_contract(v: &ArgValue) -> Option<ContractTy> {
    Some(match v {
        ArgValue::Int(i) => ContractTy::LitInt(*i),
        ArgValue::Float(f) => ContractTy::LitFloat(*f),
        ArgValue::Str(s) => ContractTy::LitStr(s.clone()),
        ArgValue::Bool(b) => ContractTy::LitBool(*b),
        ArgValue::Null => ContractTy::Null,
        _ => return None,
    })
}

/// Acceptance for a generic type: `array<…>`/`list<…>`/`non-empty-*<…>` (per the
/// phpstan#14939 list semantics), simple `int<lo, hi>` ranges; everything else
/// (`Collection<…>`, `iterable<…>`, template generics) is silent.
fn accepts_generic(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    base: &str,
    args: &[steins_phpdoc::ast::GenericArg],
    v: &CVal,
) -> Tri {
    let base_lc = base.to_ascii_lowercase();
    match base_lc.as_str() {
        // `array`/`non-empty-array`/`list`/`non-empty-list`, per phpstan#14939's
        // list semantics.
        //
        // NOT converged onto `lower_generic` + `admits_val` (unlike the
        // `associative-array`/`int`/`key-of` arms below): that convergence
        // regressed `nested_generic_fires_on_inner_mismatch`
        // (`steins-infer/tests/generics_carry.rs`) — `list<Box<int>>` with a
        // `Box<string>` element must still fire `No`, but a `Box` element is an
        // **object**, which `cval_as_val` cannot represent, so the array collapses
        // to `Maybe`, losing inner-mismatch detection. This leg is reached for
        // arrays of PROJECT OBJECTS, so `check_arraylike`'s per-element recursion
        // through `accepts()` (dispatching to `accepts_class_generic`/
        // `accepts_class_name`) is load-bearing — one relation only where the
        // value domain can host it (ADR-0062 §5).
        "array" | "non-empty-array" | "list" | "non-empty-list" => {
            let CVal::Array(entries) = v else { return Tri::No };
            let non_empty = base_lc.starts_with("non-empty");
            let require_list = base_lc.ends_with("list");
            // list<V> / non-empty-list<V>: 1 arg (value). array<V>: 1 arg (value);
            // array<K, V>: 2 args (key, value).
            let (key_ty, val_ty) = match (require_list, args) {
                (_, [v1]) => (None, &v1.ty),
                (false, [k, v2]) => (Some(&k.ty), &v2.ty),
                (true, [_, v2]) => (None, &v2.ty), // list<int, V> is unusual; ignore key
                _ => return Tri::Maybe,
            };
            check_arraylike(cx, cfile, coff, entries, key_ty, val_ty, require_list, non_empty)
        }
        // `int<lo, hi>` and Phan's `int-range<lo, hi>` — the same bounded range
        // under two base names. The one generic table lowers it and the shared
        // relation judges it, exactly as the identifier path does (ADR-0062 §5).
        "int" | "int-range" if args.len() == 2 => match cval_as_val(v) {
            Some(val) => {
                steins_contract::admits_val(&steins_contract::lower_generic(base, args), &val)
            }
            None => Tri::Maybe,
        },
        // Phan's `associative-array<K, V>` / `non-empty-associative-array<K, V>`
        // (census bucket ix): unlike the plain-array arm above, `lower_generic`
        // already carries the not-a-list refusal (`MapOf.not_list`), so the
        // shared relation judges it directly.
        "associative-array" | "non-empty-associative-array" if matches!(args.len(), 1 | 2) => {
            match cval_as_val(v) {
                Some(val) => {
                    steins_contract::admits_val(&steins_contract::lower_generic(base, args), &val)
                }
                None => Tri::Maybe,
            }
        }
        // `key-of<T>` / `value-of<T>` (census bucket vi, inline tier): the one
        // generic table projects the key/value set out of the lowered operand and
        // the shared relation judges the result. An operand the projection cannot
        // read lowers to `Opaque`, `Maybe` for every value.
        "key-of" | "value-of" if args.len() == 1 => {
            let Some(val) = cval_as_val(v) else { return Tri::Maybe };
            // Two resolvers, one projection rule (ADR-0030): `lower_generic`
            // resolves context-free operands (inline shape, `array<K, V>`,
            // `list<T>`); the const-fetch resolver below supplies the one operand
            // only this lane can see — a class constant holding an array.
            let projected = const_operand_shape(cx, cfile, coff, &args[0].ty).map(|shape| {
                if base_lc == "key-of" {
                    steins_contract::project_key_of(&shape)
                } else {
                    steins_contract::project_value_of(&shape)
                }
            });
            match projected {
                Some(ty) => steins_contract::admits_val(&ty, &val),
                None => {
                    steins_contract::admits_val(&steins_contract::lower_generic(base, args), &val)
                }
            }
        }
        // A class-level generic `Class<A, …>` (ADR-0032 tier 3, issue #10).
        _ => accepts_class_generic(cx, cfile, coff, base, args, v),
    }
}

/// Acceptance of a value against a class-level generic contract `Class<A, …>`
/// (ADR-0032 tier 3, issue #10; inheritance edges via issue #294). The class half
/// rides the trinary is-a oracle exactly as the bare-class identifier path; the
/// argument half judges ONLY through the carried edge whose **owner is the class
/// the contract names** — the object's own class when it declares the templates,
/// an ancestor when `@extends Box<int>` does.
///
/// Honesty bounds (zero-FP):
/// - A **non-object** value is silent (`Maybe`): the bare-class identifier path
///   owns scalar-vs-class `No`.
/// - The class half only **gates**: a `No`/`Unknown` is-a answers `Maybe`, never a
///   manufactured `No` — the sole `No` here comes from a provable **argument-half**
///   violation on an object that **is** the required class.
/// - **No matching edge** or an **arity mismatch** answers `Maybe`.
/// - A **non-invariant** template position answers `Maybe` regardless of its
///   argument — Steins models neither variance direction, and reading a
///   `@template-covariant` position invariantly would convict correct code. See
///   [`template_variances`].
pub(crate) fn accepts_class_generic(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    base: &str,
    args: &[steins_phpdoc::ast::GenericArg],
    v: &CVal,
) -> Tri {
    let CVal::Object(obj_class, carries) = v else { return Tri::Maybe };
    let target = cx.resolve_pclass(cfile, coff, base);
    // Class half: proceed only on a proven is-a; otherwise stay silent.
    if cx.is_a(obj_class, &target) != IsA::Yes {
        return Tri::Maybe;
    }
    // Argument half: the edge that speaks about THIS class's templates, if any.
    let Some(carry) = carry_for_owner(carries, &target) else {
        return Tri::Maybe;
    };
    if carry.args.len() != args.len() {
        return Tri::Maybe;
    }
    let variances = template_variances(cx, &carry.owner);
    let mut r = Tri::Yes;
    for (i, (declared, actual)) in args.iter().zip(carry.args.iter()).enumerate() {
        // Variance gates before the comparison, not after it.
        if variances.get(i).copied().unwrap_or_default() != Variance::Invariant {
            r = combine(r, Tri::Maybe);
            continue;
        }
        let one = match actual {
            CArg::Val(cv) => accepts(cx, cfile, coff, &declared.ty, cv),
            CArg::Ty(cty) => accepts_carried_ty(cx, carry.site, &declared.ty, cty),
        };
        r = combine(r, one);
        if r == Tri::No {
            return Tri::No;
        }
    }
    r
}

/// The class and **declared** carries an argument denotes where it is bound to a
/// NON-exact heap object — a declared parameter's seed above all (ADR-0032's
/// 2026-08-16 amendment, issue #388).
///
/// [`Cx::resolve_cval`] declines such an object deliberately: its `CVal::Object`
/// licenses the No-side `is_a` conclusion the bare-class acceptance path draws, and
/// a lower bound would make that unsound (audit G1). That left the two readers which
/// only ever index a carry **positionally** — declared-argument acceptance
/// ([`accepts_class_generic`]) and the call-site template binder
/// ([`bind_call_templates`]) — with nothing to read on a declared parameter, though
/// neither needs the licence `resolve_cval` is withholding. Both read through here
/// instead, and the class half stays silent.
///
/// `None` for an exact object (`resolve_cval` already speaks for it), for anything
/// that is not a heap-bound variable, for a poisoned scope, and for an object
/// carrying nothing declared — there being no position a reader could then index.
///
/// [`bind_call_templates`]: crate::bind_call_templates
pub(crate) fn declared_carrier(
    value: &ArgValue,
    store: &Store,
    poisoned: bool,
) -> Option<(String, Vec<GenericCarry>)> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let obj = store.obj_of(name)?;
    if obj.class_exact {
        return None;
    }
    let carries = obj.declared_targs();
    if carries.is_empty() {
        return None;
    }
    Some((obj.class.clone(), carries))
}

/// The FQN comparison key for a class name: case-insensitive (PHP class names are),
/// leading `\` insignificant.
pub(crate) fn class_key(fqn: &str) -> String {
    fqn.trim_start_matches('\\').to_ascii_lowercase()
}
