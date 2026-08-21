//! Declared-return arms at a call: the contract arm list a call seeds into the
//! assignment, with template binding across the call (ADR-0032) and the receiver's
//! carried template types.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::Base;
use steins_phpdoc::Type as PType;
use steins_phpdoc::ast::{ConstExpr, TypeKind as PKind, StringLit};
use steins_syntax::{ArgValue, CallExpr, Callee, NativeType, Param, ScalarType, TypeMember};

use crate::Folder;
use crate::contract::{
    CArg, CVal, Envelopes, TemplateShadow, declared_carrier, for_each_child_type, template_names_of,
};
use crate::cx::Cx;
use crate::descent::value_lane_fn_site;
use crate::dispatch::{CallTarget, resolve_call_target};
use crate::env::{ContractArm, Known, Store};
use crate::generics::{
    CarriedArg, carg_contract_ty, get_template_type, names_unknown_class, template_arg_carries,
};
use crate::method_call::nullsafe_call;
use crate::project::Site;
use crate::refine::{
    expand_enum_case_arms, flatten_arms, refine_contract_arms, refine_declared_arms,
};
use crate::untyped::is_template_type;

/// The declared-return arm list to seed the assigned variable of `$x = f(...)` /
/// `$x = $o->m(...)` at a call site (ADR-0052 §9, the return direction; ADR-0075
/// for methods). For a uniquely-resolved user function target, or a resolved
/// method/static target via [`resolve_call_target`], the native return type seeds
/// `Verified` arms and the `@return` phpdoc refines `Asserted` within it.
///
/// Verified membership is never exactness: a `: Foo` native return seeds an
/// Instance-membership arm, not an exact-class object — the runtime class may be
/// any subclass. This arm feeds the S6-style declared-receiver lane and the
/// `eval_instanceof` Yes-side only; it must never satisfy an exactness-requiring
/// proof leg (ADR-0052 §3 NOT-fed list). The arms are the floor below values,
/// seeded only after every proven-value path has declined. `None` for a
/// builtin/unknown/dynamic target, a constructor, or no declared return type.
pub(crate) fn call_return_arms(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    if let Some(site) = cx.resolve_user_fn_any(call) {
        return fn_return_arms(cx, site);
    }
    // Constructors are the ADR-0036 exactness lane, not a value-return floor.
    if matches!(call.receiver, Callee::Construct { .. }) {
        return None;
    }
    // `$x = $b?->m()` may be `null` whatever `m` declares (ADR-0075 §3.1). Declined
    // here as well as at the outcome, since this is `apply_assign`'s own fallback
    // and would otherwise re-seed the floor the outcome just refused.
    if nullsafe_call(&call.receiver) {
        return None;
    }
    let target = resolve_call_target(cx, &call.receiver, store, this_exact, enclosing_class, poisoned)?;
    method_return_arms(cx, &target)
}

/// [`call_return_arms`] for a call known only by its **simple name** (issue #60):
/// the declared-return floor of a call in value position, where no [`CallExpr`]
/// exists. Resolution is [`value_lane_fn_site`], the same hardened rule as the
/// value lane's [`project_call_summary`] — the two must agree on the target or the
/// floor could name a different function than the summary descended into.
///
/// `args` is the lowered argument list [`ArgValue::Call`] carries, which the
/// call-site template read binds from (issue #363). It needs no positional gate of
/// its own: a named or spread call never lowers to an [`ArgValue::Call`] at all, so
/// what arrives here is positional by construction.
///
/// [`project_call_summary`]: crate::descent::project_call_summary
pub(crate) fn call_return_arms_by_name(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    let site = value_lane_fn_site(cx, folder, name)?;
    let bindable: Vec<&ArgValue> = args.iter().collect();
    fn_return_arms_at_call(cx, folder, site, &bindable, env, store, poisoned)
}

/// [`call_return_arms_by_name`]'s **method** twin (issue #386): the declared-return
/// floor of a method or static call in value position, resolved through the same
/// `resolve_call_target` [`project_method_summary`] uses — the two must agree on the
/// target, or the floor could name a different method than the summary descended
/// into.
///
/// `this_exact`/`enclosing_class` are `None` at a frame-less entry
/// ([`best_dump_phpdoc_type`]), which declines `$this->`/`self::`/`parent::` there
/// by `resolve_call_target`'s own arms. A **nullsafe** call declines outright: the
/// result may be `null` and the arms do not say so (ADR-0075 §3.1).
///
/// A `Receiver::New`'s **carries** are not read here, unlike at the summary rung:
/// filling them means minting the receiver object, which means walking its
/// constructor, and a declared floor is not worth a walk that the rung above it has
/// already made when it could. Strictly less knowledge, at the rung that already is
/// the floor.
///
/// [`project_method_summary`]: crate::descent::project_method_summary
/// [`best_dump_phpdoc_type`]: crate::dump::best_dump_phpdoc_type
#[allow(clippy::too_many_arguments)]
pub(crate) fn method_return_arms_by_callee(
    cx: &Cx,
    folder: &mut dyn Folder,
    callee: &Callee,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    if nullsafe_call(callee) {
        return None;
    }
    let target = resolve_call_target(cx, callee, store, this_exact, enclosing_class, poisoned)?;
    let bindable: Vec<&ArgValue> = args.iter().collect();
    method_return_arms_at_call(cx, folder, &target, &bindable, env, store, poisoned)
}

/// The declared-return contract arms of the project function at `site` — the shared
/// body of [`call_return_arms`] (a resolved [`CallExpr`]) and
/// [`call_return_arms_by_name`] (a value-position simple name).
pub(crate) fn fn_return_arms(cx: &Cx, site: Site) -> Option<Vec<ContractArm>> {
    let decl = cx.fn_decl(site);
    let native: Vec<ContractTy> = decl.ret.as_ref().map(native_arms).unwrap_or_default();
    // The callee's own `@return` envelope (with its function-level `@template` names
    // already shadowed to `Opaque` by `parse_envelopes`, issue #5). Class arms resolve
    // in the CALLEE's file/namespace (where the return type is written), matching how
    // the native return member list's FQNs were resolved at lowering.
    let off = decl.span.start;
    let phpdoc = cx.envelopes_of(decl.docblock.as_deref(), site.file, off).and_then(|e| e.ret);
    let resolve = |n: &str| {
        cx.resolve_pclass(site.file, off, n).trim_start_matches('\\').to_ascii_lowercase()
    };
    let mut arms = refine_contract_arms(&native, phpdoc.as_ref(), &resolve)?;
    // The finite enum domain travels the return direction too (issue #429): a
    // `: Suit` declaration states the same enforced case set on either side of
    // the boundary.
    expand_enum_case_arms(cx, &mut arms);
    Some(arms)
}

/// [`fn_return_arms`] with the call's own **arguments** in hand (ADR-0032's second
/// 2026-08-15 amendment, issue #363): one rung above the argument-blind floor,
/// where a function-level `@template T` bound from an argument's generics carry
/// lets the callee's `@return T` name a type instead of flooring to `Opaque`.
///
/// Tried first, falling straight through: everything the binding rule does not
/// reach — every non-binding parameter spelling, every argument that carries
/// nothing, every disagreement between two occurrences — is exactly
/// [`fn_return_arms`], which is also what a caller with no arguments in hand
/// keeps calling.
///
/// The rung ABOVE this one is the body summary, and it stays above: this is the
/// declared floor's seam, which the summary already outranks at both the
/// assignment and the value position. That ordering is the amendment's answer to
/// the dual-inference hazard tier 1 refuses a solver over.
pub(crate) fn fn_return_arms_at_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    site: Site,
    args: &[&ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    let decl = cx.fn_decl(site);
    let native: Vec<ContractTy> = decl.ret.as_ref().map(native_arms).unwrap_or_default();
    template_arg_return_arms(
        cx,
        folder,
        TemplateReadSite {
            docblock: decl.docblock.as_deref(),
            params: &decl.params,
            native: &native,
            file: site.file,
            off: decl.span.start,
        },
        args,
        env,
        store,
        poisoned,
    )
    .or_else(|| fn_return_arms(cx, site))
}

/// [`method_return_arms`] with the call's own arguments in hand — the method twin
/// of [`fn_return_arms_at_call`] (issue #363), binding a **method-level**
/// `@template` name.
///
/// The two carry readers on a method are orthogonal and both run: this one indexes
/// an *argument's* carry for a method-level subject, [`receiver_template_type_arms`]
/// indexes the *receiver's* for a class-level one, and the two shadow stages keep
/// their name spaces apart — a method-level name is already an opaque node when
/// this runs, a class-level one is still an identifier. The receiver's carries are
/// untouched here.
pub(crate) fn method_return_arms_at_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    target: &CallTarget<'_>,
    args: &[&ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    let method = target.method;
    let native: Vec<ContractTy> = method.ret.as_ref().map(native_arms).unwrap_or_default();
    template_arg_return_arms(
        cx,
        folder,
        TemplateReadSite {
            docblock: method.docblock.as_deref(),
            params: &method.params,
            native: &native,
            file: target.class_file,
            off: method.span.start,
        },
        args,
        env,
        store,
        poisoned,
    )
    .or_else(|| method_return_arms(cx, target))
}

/// The declaration half of a call-site template read — the four things
/// [`template_arg_return_arms`] needs about the callee, bundled so the free-function
/// and method entries hand over the same shape (their declarations are different
/// types; everything the read asks of them is not).
struct TemplateReadSite<'a> {
    docblock: Option<&'a str>,
    params: &'a [Param],
    /// The callee's native return arms — the envelope the read must refine within.
    native: &'a [ContractTy],
    /// The file and offset the callee's docblock was written at, which is what
    /// resolves the class names inside it.
    file: usize,
    off: u32,
}

/// The positional argument values a call-site template read may bind from —
/// **empty** for any call whose argument list breaks the position-to-parameter map.
///
/// A named or spread argument list declines the whole call rather than binding from
/// the positional prefix, and it declines by carrying no arguments at all, so every
/// caller gets the decline for free. Same list, same reason as the carry sweep's
/// gate (issue #295): position is what the read is built on, and a call that does
/// not have one has nothing to read.
pub(crate) fn bindable_args(call: &CallExpr) -> Vec<&ArgValue> {
    if !call.positional_only || call.has_spread {
        return Vec::new();
    }
    call.args.iter().map(|a| &a.value).collect()
}

/// One binding of a function-/method-level `@template` name to what flowed in at a
/// call site (issue #363) — a carry argument plus the context its class names were
/// written in, the owned twin of [`CarriedArg`].
///
/// Equality is structural and includes the site, which is what makes the
/// all-or-nothing rule safe for a [`CArg::Ty`]: the same spelling written in two
/// files can name two classes, and two bindings that might not be the same thing
/// are not treated as agreeing.
#[derive(Clone, PartialEq)]
struct BoundTemplate {
    arg: CArg,
    site: Option<(usize, u32)>,
}

/// The return arms of a callee whose `@return` names one of its **own**
/// `@template`s, bound from the generics carry of the argument that flowed into a
/// `@param Owner<…, T, …>` (ADR-0032's second 2026-08-15 amendment, issue #363).
///
/// A projection, not a solver: one positional read out of tier-3 state, no
/// constraint generation, no unification, no reverse flow into the argument, no
/// fixpoint. What it produces is what tier 1 already calls `T` — whatever flowed in
/// — made legible at the call site because the carry recorded it.
///
/// The `@return` shapes that read, both as [`Cx::envelopes_of`] leaves them:
///
/// - `T` itself (an opaque node carrying the raw name, since the declaration's own
///   shadow has run) — which covers `@return template-type<Box<T>, Box, 'T'>` too,
///   because issue #361 rewrote that to exactly this node;
/// - `template-type<T, Owner, 'TName'>` — the subject issue #361 deferred, whose
///   answer is one hop past the binding: the carried argument's own carries,
///   indexed by `'TName'` on `Owner`. The receiver-less twin of
///   [`receiver_template_type_arms`].
///
/// The result enters through [`refine_declared_arms`], so it comes out `Asserted`
/// for the same structural reason a hand-written `@return` does and no consumer can
/// tell which spelling produced it. `None` everywhere the read does not land, and
/// every `None` leaves the caller's existing floor exactly as it was.
fn template_arg_return_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    at: TemplateReadSite<'_>,
    args: &[&ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<Vec<ContractArm>> {
    // The overwhelmingly common short-circuit: a declaration with no `@template` of
    // its own has nothing here to bind, and pays one docblock scan to say so.
    let shadow = template_names_of(at.docblock);
    if shadow.is_empty() || args.is_empty() {
        return None;
    }
    // A by-ref or variadic parameter list declines the whole call for the same
    // reason a named or spread argument list does (already declined by an empty
    // `args`): the read is positional, and these are the shapes where a position
    // stops naming one parameter.
    if args.len() > at.params.len() || at.params.iter().any(|p| p.variadic || p.by_ref) {
        return None;
    }
    let envelopes = cx.envelopes_of(at.docblock, at.file, at.off)?;
    let ret = envelopes.ret.as_ref()?;
    let bound = bind_call_templates(cx, folder, &envelopes, &at, &shadow, args, env, store, poisoned);
    read_bound_template(cx, &bound, ret, at.native, at.file, at.off)
}

/// Bind every one of the declaration's own `@template` names the call's arguments
/// decide — the binding half of [`template_arg_return_arms`].
///
/// **All-or-nothing, over every occurrence and not just the readable ones.** A
/// name binds only when *every* place the parameter envelopes mention it is a
/// binding position this function actually read, and all of those reads agree.
/// Anything else maps it to `None` — contested — and the read declines.
///
/// The strictness is the whole soundness argument, and the direction of the error
/// is why. A `@template T` witnessed at two parameters is the docblock stating
/// that one type stands at both; reading the one position Steins understands and
/// ignoring the other would answer **narrower than the declaration supports** —
/// `@param \Closure():T $t1, @param T $t2, @return T` handed a `Closure(): A1` and
/// an `A2` would come back `A2` where the truth is `A1|A2`. Narrower-than-true is
/// the direction this family has refused everywhere else (a stale carry, a
/// covariant position), and being `Asserted` does not excuse it: contract arms feed
/// narrowing and the dump surface. Widening instead would mean *modelling* the
/// positions the rule declines, which is the solver ADR-0032 refuses. So the read
/// declines, and says why.
///
/// Contesting occurrences, exhaustively: a parameter whose declared type is
/// neither of the two binding shapes; a `@param Owner<…>` whose owner is not a
/// class declaring templates, or whose arity disagrees with that list; any slot of
/// an otherwise-aligned `Owner<…>` that mentions the name below its top level; a
/// `@param` on a parameter the call supplied no argument for; and a `@param` naming
/// no declared parameter at all.
#[allow(clippy::too_many_arguments)]
fn bind_call_templates(
    cx: &Cx,
    folder: &mut dyn Folder,
    envelopes: &Envelopes,
    at: &TemplateReadSite<'_>,
    shadow: &TemplateShadow,
    args: &[&ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> HashMap<String, Option<BoundTemplate>> {
    let mut bound: HashMap<String, Option<BoundTemplate>> = HashMap::new();
    // Every `@param` envelope, not every parameter: a tag naming no declared
    // parameter still witnesses its templates, and no argument was ever matched to
    // it, so it contests them.
    for (pname, ty) in &envelopes.params {
        let value = at.params.iter().position(|p| p.name == *pname).and_then(|i| args.get(i));
        let Some(value) = value else {
            contest_mentions(&mut bound, ty, shadow);
            continue;
        };
        match &ty.kind {
            // `@param T $p` — the whole parameter IS the template, so what binds is
            // the argument's own proven value, and there is no sub-node left to
            // contest. A bounded template never reaches this arm: the shadow already
            // replaced it with its bound, which is what the author promised and what
            // `@return T` therefore reads.
            PKind::Unsupported(name) if shadow.contains(&name.to_ascii_lowercase()) => {
                let carried = cx
                    .resolve_cval(value, env, store, poisoned, folder)
                    .map(|cv| BoundTemplate { arg: CArg::Val(cv), site: None });
                record_binding(&mut bound, name, carried);
            }
            // `@param Owner<…, T, …> $p` — TOP level only, and only where the
            // spelling aligns with the owner's own `@template` list position for
            // position. A nested (`list<Box<T>>`) or nullable (`Box<T>|null`)
            // spelling is a different node kind and falls to the contesting arm
            // below, which is the decline the amendment states.
            PKind::Generic { base, args: spelled } => {
                let owner = cx.resolve_pclass(at.file, at.off, base);
                let names = class_template_names(cx, &owner);
                if names.is_empty() || names.len() != spelled.len() {
                    // The owner is not a class whose templates these arguments align
                    // to, so no slot of it is a binding position — including the ones
                    // that look like one.
                    contest_mentions(&mut bound, ty, shadow);
                    continue;
                }
                // The argument's carries, through the same resolution acceptance
                // uses — so a direct `new` in argument position and a heap-bound
                // variable (issue #295) both reach here, and a swept carry reaches
                // here empty.
                let carries = match cx.resolve_cval(value, env, store, poisoned, folder) {
                    Some(CVal::Object(_, carries)) => carries,
                    // A declared parameter's object is a lower bound and therefore
                    // never a `CVal` (audit G1), but its declared carries index
                    // positionally exactly as a proven object's do (issue #388).
                    _ => declared_carrier(value, store, poisoned)
                        .map(|(_, carries)| carries)
                        .unwrap_or_default(),
                };
                for (j, slot) in spelled.iter().enumerate() {
                    match &slot.ty.kind {
                        PKind::Unsupported(name)
                            if shadow.contains(&name.to_ascii_lowercase()) =>
                        {
                            let carried = get_template_type(cx, &carries, &owner, &names[j])
                                .map(|c| BoundTemplate { arg: c.arg.clone(), site: c.site });
                            record_binding(&mut bound, name, carried);
                        }
                        // A slot that is not itself the template — `Box<list<T>>` —
                        // is a mention the read cannot index, so it contests.
                        _ => contest_mentions(&mut bound, &slot.ty, shadow),
                    }
                }
            }
            _ => contest_mentions(&mut bound, ty, shadow),
        }
    }
    bound
}

/// Record one occurrence's verdict about `name`, applying the all-or-nothing rule:
/// a first binding stands, a second one that agrees changes nothing, and anything
/// else — a disagreement, or an occurrence that carried nothing — contests the name
/// permanently.
fn record_binding(
    bound: &mut HashMap<String, Option<BoundTemplate>>,
    name: &str,
    carried: Option<BoundTemplate>,
) {
    match bound.entry(name.to_ascii_lowercase()) {
        std::collections::hash_map::Entry::Vacant(v) => {
            v.insert(carried);
        }
        std::collections::hash_map::Entry::Occupied(mut o) => {
            let agrees = matches!((o.get(), &carried), (Some(a), Some(b)) if a == b);
            if !agrees {
                o.insert(None);
            }
        }
    }
}

/// Contest every template name `ty` mentions anywhere — the non-binding half of
/// [`bind_call_templates`]' all-or-nothing rule.
fn contest_mentions(
    bound: &mut HashMap<String, Option<BoundTemplate>>,
    ty: &PType,
    shadow: &TemplateShadow,
) {
    let mut names = Vec::new();
    mentioned_templates(ty, shadow, &mut names);
    for name in names {
        record_binding(bound, &name, None);
    }
}

/// Every mention of one of `shadow`'s template names anywhere inside `ty`.
///
/// **Two spellings still count, though no longer for the original reason.** This
/// walk was written to match both the neutralized [`PKind::Unsupported`] form and
/// the raw [`PKind::Identifier`] because [`neutralize_templates`] stopped at a
/// `Callable` and a `Conditional`, leaving the `T` in `\Closure():T` unshadowed.
/// Issue #374 closed that gap: its one caller reads envelopes the shadow has
/// already rewritten everywhere, so the identifier arm no longer catches anything
/// the opaque arm misses. It stays anyway, because the question asked here is
/// about a *type* and not about a stage — a caller reading a docblock type before
/// the shadow would otherwise silently under-count — and it costs one lookup on a
/// name already in hand. A `\`-qualified name is never a template, matching the
/// shadow's own rule.
///
/// A `Generic`'s **base** counts too (`T<int>`): nothing neutralizes a base string,
/// and a template used as a generic base is a mention the read cannot index.
///
/// [`neutralize_templates`]: crate::contract::neutralize_templates
pub(crate) fn mentioned_templates(ty: &PType, shadow: &TemplateShadow, out: &mut Vec<String>) {
    let note = |name: &String, out: &mut Vec<String>| {
        if !name.contains('\\') && shadow.contains(&name.to_ascii_lowercase()) {
            out.push(name.clone());
        }
    };
    match &ty.kind {
        PKind::Identifier(name) | PKind::Unsupported(name) | PKind::Generic { base: name, .. } => {
            note(name, out);
        }
        _ => {}
    }
    for_each_child_type(ty, &mut |child| mentioned_templates(child, shadow, out));
}

/// The class-level `@template` names `class_fqn` declares, in declaration order —
/// the positional list every carry aligns to.
///
/// Empty for an unresolvable class or one declaring none, which declines every
/// read that would have indexed it.
pub(crate) fn class_template_names(cx: &Cx, class_fqn: &str) -> Vec<String> {
    cx.find_class(class_fqn)
        .and_then(|(_, cd)| cd.docblock.as_deref())
        .map(steins_phpdoc::scan_template_names)
        .unwrap_or_default()
}

/// The arms a callee's `@return` denotes once its own `@template` names are bound —
/// the reading half of [`template_arg_return_arms`].
fn read_bound_template(
    cx: &Cx,
    bound: &HashMap<String, Option<BoundTemplate>>,
    ret: &PType,
    native: &[ContractTy],
    file: usize,
    off: u32,
) -> Option<Vec<ContractArm>> {
    let binding = |name: &str| bound.get(&name.to_ascii_lowercase())?.as_ref();
    let (ty, site) = match &ret.kind {
        // `@return T`, and — since issue #361 rewrote it to this very node —
        // `@return template-type<Box<T>, Box, 'T'>` with it.
        PKind::Unsupported(name) => {
            let b = binding(name)?;
            (carg_contract_ty(&b.arg)?, b.site)
        }
        // `@return template-type<T, Owner, 'TName'>` — the Deferred subject, one hop
        // past the binding. `getTemplateType` on a function-level template.
        PKind::Generic { base, args } if is_template_type(base, args.len()) => {
            let PKind::Unsupported(name) = &args[0].ty.kind else { return None };
            let b = binding(name)?;
            let PKind::Identifier(owner_name) = &args[1].ty.kind else { return None };
            let PKind::Const(ConstExpr::Str(StringLit::Single(want) | StringLit::Double(want))) =
                &args[2].ty.kind
            else {
                return None;
            };
            let owner_fqn = cx.resolve_pclass(file, off, owner_name);
            let hop = template_arg_carries(cx, &CarriedArg { arg: &b.arg, site: b.site });
            let named = get_template_type(cx, &hop, &owner_fqn, want)?;
            (carg_contract_ty(named.arg)?, named.site)
        }
        // Every other `@return` — a class, a scalar, a union mentioning `T`, a
        // shape — is not this read. The argument-blind floor already says whatever
        // there is to say about it.
        _ => return None,
    };
    // Class names inside a carried type are resolved where they were WRITTEN
    // (issue #294), and an unresolvable one stays silent — the same valve
    // [`receiver_template_type_arms`] applies for the same reason.
    let (rfile, roff) = site.unwrap_or((file, off));
    if names_unknown_class(cx, rfile, roff, &ty) {
        return None;
    }
    let resolve =
        |n: &str| cx.resolve_pclass(rfile, roff, n).trim_start_matches('\\').to_ascii_lowercase();
    refine_declared_arms(native, flatten_arms(ty), &resolve)
}

/// The declared-return contract arms of a resolved method/static target (ADR-0075
/// floor parity with free functions). Same native + `@return` refinement as
/// [`fn_return_arms`]; class-level `@template` names shadow in the method docblock
/// (issue #5), matching [`scope_return_phpdoc`]'s method leg.
fn method_return_arms(cx: &Cx, target: &CallTarget<'_>) -> Option<Vec<ContractArm>> {
    let method = target.method;
    let native: Vec<ContractTy> = method.ret.as_ref().map(native_arms).unwrap_or_default();
    let off = method.span.start;
    let file = target.class_file;
    let mut envelopes = cx.envelopes_of(method.docblock.as_deref(), file, off);
    // The receiver-carry read runs HERE, between the two shadow stages (issue
    // #362): `envelopes_of` has already resolved everything declarations decide
    // and deliberately left a template subject as written (issue #361), while the
    // class-level shadow below is about to neutralize that subject to an opaque
    // node — after which there is no spelling left to intercept. Declining falls
    // through to exactly the floor this function had before.
    if let Some(ret) = envelopes.as_ref().and_then(|e| e.ret.as_ref())
        && let Some(arms) = receiver_template_type_arms(cx, target, &native, ret)
    {
        return Some(arms);
    }
    if let Some(e) = &mut envelopes {
        e.shadow_templates(&template_names_of(target.declaring_class.docblock.as_deref()));
    }
    let phpdoc = envelopes.and_then(|e| e.ret);
    let resolve = |n: &str| {
        cx.resolve_pclass(file, off, n).trim_start_matches('\\').to_ascii_lowercase()
    };
    let mut arms = refine_contract_arms(&native, phpdoc.as_ref(), &resolve)?;
    // As in [`fn_return_arms`] (issue #429).
    expand_enum_case_arms(cx, &mut arms);
    Some(arms)
}

/// The return arms of a `@return template-type<T, Owner, 'TName'>` whose subject
/// `T` is a **class-level template of the receiver's class**, read off the
/// receiver's generics carry (ADR-0032's 2026-08-15 amendment, issue #362) — the
/// shape phpstan/phpstan#9053 was opened for.
///
/// Two lookups over the carry, each one level:
///
/// 1. `T`'s position in the declaring class's own `@template` list picks an
///    argument out of the carry edge owned by that class — for
///    `new Helper(new Model())`, the proven `Model` object.
/// 2. That argument's own carries are indexed again, by `'TName'` on `Owner` —
///    `Model`'s `@implements ModelInterface<Child>` edge gives `Child`.
///
/// The result enters through [`refine_declared_arms`], the same refinement a
/// hand-written `@return Child` goes through, so it comes out at the same stratum
/// and a reader cannot tell which spelling produced it. `Asserted`, never
/// laundered: the carry is proven, but what it resolves is still the docblock's
/// claim about a return.
///
/// `None` at every step that does not land, and each `None` is a floor rather
/// than a gap: the subject is not one of the declaring class's templates (a
/// **method**-level template subject is issue #363, and declines here); no carry
/// edge is owned by the declaring class, because the receiver had none, because a
/// `$this`/non-exact/static/`new` receiver contributes none, or because an
/// earlier receiver call swept the value carry (issue #295); the owner declares
/// no such template; the hop carries no edge owned by the owner; the argument is
/// a value the contract lane cannot state. PHPStan falls back to an unresolved
/// template's declared bound, which Steins declines on tier 1's own terms
/// (issue #293), so that path floors here too.
fn receiver_template_type_arms(
    cx: &Cx,
    target: &CallTarget<'_>,
    native: &[ContractTy],
    ret: &PType,
) -> Option<Vec<ContractArm>> {
    let PKind::Generic { base, args } = &ret.kind else { return None };
    if !is_template_type(base, args.len()) {
        return None;
    }
    // The subject: a bare name that must be one of the DECLARING class's own
    // `@template`s. Anything else — a spelled parameterization (#361 already
    // resolved it), a method-level template (#363), a union — is not this read.
    let PKind::Identifier(subject) = &args[0].ty.kind else { return None };
    let declaring = target.declaring_class;
    let declares_subject = declaring
        .docblock
        .as_deref()
        .map(steins_phpdoc::scan_template_names)
        .unwrap_or_default()
        .iter()
        .any(|n| n == subject || n.eq_ignore_ascii_case(subject));
    if !declares_subject {
        return None;
    }
    let (file, off) = (target.class_file, target.method.span.start);
    // The owner is a class *reference*, resolved in the file the `@return` was
    // written in; the template name is a quoted literal, not a type.
    let PKind::Identifier(owner_name) = &args[1].ty.kind else { return None };
    let PKind::Const(ConstExpr::Str(StringLit::Single(want) | StringLit::Double(want))) =
        &args[2].ty.kind
    else {
        return None;
    };
    let owner_fqn = cx.resolve_pclass(file, off, owner_name);

    let subject_arg = get_template_type(cx, &target.receiver_carries, &declaring.fqn, subject)?;
    let hop = template_arg_carries(cx, &subject_arg);
    let named = get_template_type(cx, &hop, &owner_fqn, want)?;
    let ty = carg_contract_ty(named.arg)?;
    // Class names inside a carried type are resolved where they were WRITTEN, not
    // where they are read — the carry keeps that context precisely so a lifted
    // argument keeps naming the class it named (issue #294). An unresolvable name
    // stays silent, the same safety valve `accepts_carried_ty` applies.
    let (rfile, roff) = named.site.unwrap_or((file, off));
    if names_unknown_class(cx, rfile, roff, &ty) {
        return None;
    }
    let resolve =
        |n: &str| cx.resolve_pclass(rfile, roff, n).trim_start_matches('\\').to_ascii_lowercase();
    refine_declared_arms(native, flatten_arms(ty), &resolve)
}

/// Lower a native scalar/union type to contract arms (declaration order, then a
/// `null` arm when nullable). Every native member is representable: the four
/// scalars, `false`/`true` literals, and object `Instance` members (the lowercase
/// FQN, matching [`ContractTy::Class`]'s normalization).
pub(crate) fn native_arms(ty: &NativeType) -> Vec<ContractTy> {
    let mut arms: Vec<ContractTy> = ty
        .members
        .iter()
        .map(|m| match m {
            TypeMember::Scalar(ScalarType::Int) => ContractTy::Base(Base::Int),
            TypeMember::Scalar(ScalarType::Float) => ContractTy::Base(Base::Float),
            TypeMember::Scalar(ScalarType::String) => ContractTy::Base(Base::String),
            TypeMember::Scalar(ScalarType::Bool) => ContractTy::Base(Base::Bool),
            TypeMember::BoolLiteral(b) => ContractTy::LitBool(*b),
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
            TypeMember::InstanceInter(cs) => {
                ContractTy::Inter(cs.iter().map(|c| ContractTy::Class(c.fqn.clone())).collect())
            }
        })
        .collect();
    if ty.nullable {
        arms.push(ContractTy::Null);
    }
    arms
}
