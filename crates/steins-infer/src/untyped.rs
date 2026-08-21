//! The untyped surface (ADR-0078 / issue #200): the contract-layer `untyped.*`
//! family, P9 of the rule-port map — declarations whose parameter, return, property,
//! class-constant, iterable-value or generics type is missing where one is claimable.

use steins_phpdoc::{Type as PType, TagKind, scan_docblock};
use steins_phpdoc::ast::TypeKind as PKind;
use steins_syntax::{Param, Span};

use crate::contract::{TemplateShadow, for_each_child_type, parse_tag_type, template_names_of};
use crate::{
    Cx, Diagnostic, UNTYPED_CLASS_CONSTANT_ID, UNTYPED_GENERICS_ID, UNTYPED_ITERABLE_VALUE_ID,
    UNTYPED_PARAMETER_ID, UNTYPED_PROPERTY_ID, UNTYPED_RETURN_ID,
};
use crate::docblock_hygiene::param_subject;
use crate::throws::resolve_class_name;

// ---------------------------------------------------------------------------
// The untyped surface (ADR-0078 / issue #200): the contract-layer `untyped.*`
// family, P9 of the rule-port map.
//
// **Declaration reading only.** Every premise below is a fact about what the
// declaration and its own docblock spell — never a value, a receiver, or
// another file's behaviour. The one cross-file question is "does this class
// declare `@template` parameters?", answered off the resident class index
// (`Cx::find_class`), itself a declaration read.
//
// The typed/untyped boundary is ADR-0078's, and it is *presence*, not agreement:
// a native type, OR a docblock claim of any provenance (`@param`, `@phpstan-param`,
// `@psalm-param`, and the `@return`/`@var` equivalents) makes the declaration
// typed. A claim that disagrees with the code is `phpdoc.*`'s finding; this
// family's subject is the claim that was never made.
//
// What the lowering leaves invisible:
//
// * **Traits** lower to a NAME only (ADR-0049 §5) — no members reach here.
// * **Enum methods** are not lowered (`ClassDecl::methods` is empty for an
//   enum), so method signatures are not measured; enum CONSTANTS are.
// * **Class-body hooked properties** are dropped at lowering (only names
//   survive), so they never reach the property arm. A PROMOTED hooked property
//   does reach it, and is skipped there like every other promoted property,
//   since the parameter arm already covers it.
// * **Closures and arrow functions** are out of scope: PHPStan ports them as
//   its own `MissingClosure*TypehintRule` pair, which issue #200 does not list.
// ---------------------------------------------------------------------------

/// The file's untyped-surface findings, run once per file from `check_units`.
///
/// Walks the file's own declarations: every named function, and every class-like's
/// methods, properties and constants.
pub(crate) fn untyped_surface(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for f in cx.tree().functions() {
        let shadow = template_names_of(f.docblock.as_deref());
        untyped_function_like(
            cx,
            f.docblock.as_deref(),
            &f.params,
            f.ret_span,
            f.span.start,
            &format!("{}()", f.name),
            false,
            &shadow,
            out,
        );
    }
    for c in cx.tree().classes() {
        // A class-level `@template` shadows the same name in EVERY member docblock
        // (issue #5), so the member walk inherits the class's shadow set.
        let class_shadow = template_names_of(c.docblock.as_deref());
        for m in &c.methods {
            let mut shadow = class_shadow.clone();
            shadow.extend(template_names_of(m.docblock.as_deref()));
            untyped_function_like(
                cx,
                m.docblock.as_deref(),
                &m.params,
                m.ret_span,
                m.span.start,
                &format!("{}::{}()", c.name, m.name),
                m.is_constructor || m.name.eq_ignore_ascii_case("__destruct"),
                &shadow,
                out,
            );
        }
        for p in &c.properties {
            // One declaration, one finding: a promoted constructor parameter is the
            // parameter arm's subject and must not be reported again here.
            if p.promoted {
                continue;
            }
            untyped_member(
                cx,
                p.docblock.as_deref(),
                p.hint_span,
                p.span.start,
                UNTYPED_PROPERTY_ID,
                &format!("property {}::${}", c.name, p.name),
                &class_shadow,
                out,
            );
        }
        for k in &c.const_decls {
            untyped_member(
                cx,
                k.docblock.as_deref(),
                k.hint_span,
                k.span.start,
                UNTYPED_CLASS_CONSTANT_ID,
                &format!("class constant {}::{}", c.name, k.name),
                &class_shadow,
                out,
            );
        }
    }
}

/// The parameter + return arms for one function-like. `no_return_type_possible` is
/// `true` for `__construct`/`__destruct`, on which PHP forbids a return type
/// outright — their silence is a language rule, not withheld information.
#[allow(clippy::too_many_arguments)]
fn untyped_function_like(
    cx: &Cx,
    docblock: Option<&str>,
    params: &[Param],
    ret_span: Option<Span>,
    anchor: u32,
    display: &str,
    no_return_type_possible: bool,
    shadow: &TemplateShadow,
    out: &mut Vec<Diagnostic>,
) {
    let doc = DeclaredTags::of(docblock);

    for p in params {
        // `doc.params_opaque` is the conservative leg: a `@param` whose subject the
        // scanner cannot attribute might be the very one covering this parameter, so
        // every parameter the docblock does NOT visibly claim declines rather than
        // guess. A parameter with its own readable `@param` is unaffected — the
        // unattributable tag cannot also be that one.
        let mut claim = doc.param(&p.name);
        if !claim.present && doc.params_opaque {
            claim = Claim::UNREADABLE;
        }
        if p.hint_span.is_none() && !claim.present {
            out.push(untyped_diag(
                cx,
                UNTYPED_PARAMETER_ID,
                p.span.start,
                format!("parameter ${} of {display} has no type — no native type and no `@param`", p.name),
            ));
        }
        untyped_iterable_and_generics(
            cx,
            p.hint_span,
            claim,
            p.span.start,
            &format!("parameter ${} of {display}", p.name),
            shadow,
            out,
        );
    }

    if no_return_type_possible {
        return;
    }
    let ret_claim = doc.ret();
    if ret_span.is_none() && !ret_claim.present {
        out.push(untyped_diag(
            cx,
            UNTYPED_RETURN_ID,
            anchor,
            format!("{display} has no return type — no native return type and no `@return`"),
        ));
    }
    untyped_iterable_and_generics(
        cx,
        ret_span,
        ret_claim,
        anchor,
        &format!("the return of {display}"),
        shadow,
        out,
    );
}

/// The property / class-constant arm: one subject, one `@var` claim. Both member
/// kinds spell their docblock claim `@var`, so one arm serves both.
#[allow(clippy::too_many_arguments)]
fn untyped_member(
    cx: &Cx,
    docblock: Option<&str>,
    hint_span: Option<Span>,
    anchor: u32,
    id: &'static str,
    display: &str,
    shadow: &TemplateShadow,
    out: &mut Vec<Diagnostic>,
) {
    let doc = DeclaredTags::of(docblock);
    let claim = doc.var();
    if hint_span.is_none() && !claim.present {
        out.push(untyped_diag(
            cx,
            id,
            anchor,
            format!("{display} has no type — no native type and no `@var`"),
        ));
    }
    untyped_iterable_and_generics(cx, hint_span, claim, anchor, display, shadow, out);
}

/// The two content-carrying arms, which apply to every subject kind alike.
fn untyped_iterable_and_generics(
    cx: &Cx,
    hint_span: Option<Span>,
    claim: Claim<'_>,
    anchor: u32,
    display: &str,
    shadow: &TemplateShadow,
    out: &mut Vec<Diagnostic>,
) {
    // `untyped.iterable-value`. The subject is the declaration's EFFECTIVE type: a
    // docblock claim replaces the native one where it exists (PHPStan's own
    // precedence), so a `@param int[] $a` narrows a native `array` and a bare
    // `@param array $a` fails to narrow whatever the native side says. With no
    // claim at all the native spelling decides, which is what makes a plain
    // `function f(array $a)` this id's subject and not `untyped.parameter`'s.
    //
    // A claim that is *present but unreadable* (unparsable, or carrying a
    // construct the type AST keeps opaque) answers `false` here: whether it
    // narrows is unknown, and unknown is silence. Its unreadability is
    // `phpdoc.unparsable`'s finding, not this one's.
    let native_iterable = hint_span
        .and_then(|s| cx.tree().source_slice(s))
        .is_some_and(native_hint_is_iterable);
    let unstated = if claim.present {
        claim.ty.is_some_and(leaves_value_type_unstated)
    } else {
        native_iterable
    };
    if unstated {
        out.push(untyped_diag(
            cx,
            UNTYPED_ITERABLE_VALUE_ID,
            anchor,
            format!("{display} is an iterable with no value type — write `array<T>`, `T[]`, `list<T>` or an array shape"),
        ));
    }
    // `untyped.generics`: a docblock type naming a `@template`-carrying class
    // without type arguments.
    let Some(ty) = claim.ty else { return };
    let mut bare: Vec<String> = Vec::new();
    collect_bare_identifiers(ty, &mut bare);
    for name in bare {
        // A template parameter of the enclosing declaration is not a class at all.
        if !name.contains('\\') && shadow.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        let fqn = resolve_class_name(cx, anchor, &name);
        let Some((_, cd)) = cx.find_class(&fqn) else { continue };
        let templates = cd.docblock.as_deref().map(steins_phpdoc::scan_template_names).unwrap_or_default();
        if templates.is_empty() {
            continue;
        }
        out.push(untyped_diag(
            cx,
            UNTYPED_GENERICS_ID,
            anchor,
            format!(
                "{display} names the generic class {name} without type arguments — it declares `@template {}`",
                templates.join(", ")
            ),
        ));
    }
}

/// One untyped-surface diagnostic at a file offset.
fn untyped_diag(cx: &Cx, id: &'static str, offset: u32, message: String) -> Diagnostic {
    let pos = cx.tree().position(offset);
    Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    }
}

/// What a docblock claims about one subject: whether it made a claim at all, and
/// the parsed type when the claim is readable.
///
/// The split is the whole family's hinge. A tag whose payload does not parse
/// contributes no type but **still counts as a claim** — the code said something,
/// and that it said it unreadably is `phpdoc.unparsable`'s finding, not this
/// family's. So `present` is what the plain parameter/return/property/constant arms
/// read, and `ty` is what only the two content-carrying arms read.
#[derive(Clone, Copy)]
struct Claim<'a> {
    /// Whether the docblock makes a claim about this subject at all.
    present: bool,
    /// The parsed claim, when it is readable.
    ty: Option<&'a PType>,
}

impl<'a> Claim<'a> {
    /// No claim at all.
    const ABSENT: Claim<'a> = Claim { present: false, ty: None };
    /// A claim exists but cannot be attributed or read — every arm declines.
    const UNREADABLE: Claim<'a> = Claim { present: true, ty: None };

    fn of(slot: Option<&'a Option<PType>>) -> Claim<'a> {
        match slot {
            None => Claim::ABSENT,
            Some(parsed) => Claim { present: true, ty: parsed.as_ref() },
        }
    }
}

/// The `@param` / `@return` / `@var` claims one docblock makes, scanned once.
#[derive(Default)]
struct DeclaredTags {
    /// `(name, parsed type)` per attributable `@param`; the inner `None` is
    /// present-but-unreadable.
    params: Vec<(String, Option<PType>)>,
    /// A `@param` tag whose subject the scanner could not attribute at all. It
    /// might name any parameter, so the parameter arms decline for the whole
    /// signature.
    params_opaque: bool,
    /// The `@return` claim, when the docblock makes one.
    ret: Option<Option<PType>>,
    /// The `@var` claim, when the docblock makes one (the first, if several).
    var: Option<Option<PType>>,
}

impl DeclaredTags {
    fn of(docblock: Option<&str>) -> Self {
        let mut out = DeclaredTags::default();
        let Some(text) = docblock else { return out };
        for tag in scan_docblock(text) {
            match tag.kind {
                // `param_subject` is the attribution rule `phpdoc.stale-param`
                // already settled (issue #186): the variable token past the type
                // expression's extent, so a `$name` inside a `callable(…)` type is
                // never mistaken for the subject.
                TagKind::Param => match param_subject(text, &tag) {
                    Some(name) if name != "this" => {
                        out.params.push((name, parse_tag_type(&tag.type_text)));
                    }
                    Some(_) => {}
                    None => out.params_opaque = true,
                },
                TagKind::Return => {
                    out.ret.get_or_insert_with(|| parse_tag_type(&tag.type_text));
                }
                TagKind::Var => {
                    out.var.get_or_insert_with(|| parse_tag_type(&tag.type_text));
                }
                _ => {}
            }
        }
        out
    }

    fn param(&self, name: &str) -> Claim<'_> {
        Claim::of(self.params.iter().find(|(n, _)| n == name).map(|(_, t)| t))
    }

    fn ret(&self) -> Claim<'_> {
        Claim::of(self.ret.as_ref())
    }

    fn var(&self) -> Claim<'_> {
        Claim::of(self.var.as_ref())
    }
}

/// Whether a native type hint, **as written**, includes an `array` or `iterable`
/// member. Mechanically exact and deliberately shallow: the hint text is split on
/// the union/intersection separators, each part stripped of `?` and whitespace and
/// case-folded, and compared against the two keywords. `Traversable`, `Generator`
/// and every userland `IteratorAggregate` are NOT this id's subject — PHPStan's
/// `missingType.iterableValue` fires on the native `array`/`iterable` keywords.
fn native_hint_is_iterable(hint: &str) -> bool {
    hint.split(['|', '&', '(', ')'])
        .map(|p| p.trim().trim_start_matches('?').trim().to_ascii_lowercase())
        .any(|p| p == "array" || p == "iterable")
}

/// Whether a phpdoc type still leaves an array/iterable **value type** unstated —
/// i.e. some part of it is a bare `array`/`iterable`/`list` rather than one of the
/// narrowing spellings.
///
/// Narrowing spellings, all of which answer `false` here: `array<T>` / `array<K,V>`
/// / `iterable<T>` / `list<T>` and their `non-empty-` kin ([`PKind::Generic`] with
/// arguments), `T[]` ([`PKind::Array`]), and every array shape
/// ([`PKind::ArrayShape`]). A union answers `true` if ANY member does — the bare
/// arm is the one that carries no value type.
fn leaves_value_type_unstated(ty: &PType) -> bool {
    match &ty.kind {
        PKind::Identifier(name) => matches!(
            name.trim_start_matches('\\').to_ascii_lowercase().as_str(),
            "array" | "iterable" | "non-empty-array" | "list" | "non-empty-list"
        ),
        PKind::Nullable(inner) => leaves_value_type_unstated(inner),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            types.iter().any(leaves_value_type_unstated)
        }
        PKind::Generic { args, .. } => args.is_empty(),
        _ => false,
    }
}

/// Collect every **bare class-name identifier** a phpdoc type names — the
/// occurrences `untyped.generics` judges. An identifier that already carries type
/// arguments is a [`PKind::Generic`], not an [`PKind::Identifier`], so it is not
/// collected; its arguments are recursed into, because `array<Collection>` names
/// `Collection` bare just as surely as a top-level occurrence does.
///
/// The one exception is a **class-reference position**, where a name without
/// type arguments is the spelling and not an omission — see
/// [`template_type_owner_arg`]. That exemption is why a generic node is
/// enumerated by hand here rather than through [`for_each_child_type`]: the
/// position is skipped by *index*, which a child walk that hands out types has no
/// way to say. Every other node descends through the shared walk, so a bare
/// generic class named inside a callable signature, a conditional branch or a
/// shape value is collected exactly as one named at the top level is (issue #374).
fn collect_bare_identifiers(ty: &PType, out: &mut Vec<String>) {
    match &ty.kind {
        PKind::Identifier(name) => out.push(name.clone()),
        PKind::Generic { base, args } => {
            let skip = template_type_owner_arg(base, args.len());
            for (i, a) in args.iter().enumerate() {
                if Some(i) == skip {
                    continue;
                }
                collect_bare_identifiers(&a.ty, out);
            }
        }
        _ => for_each_child_type(ty, &mut |child| collect_bare_identifiers(child, out)),
    }
}

/// Which argument of a generic spelling is a class **reference** rather than a
/// type — the position `untyped.generics` must not look into (issue #360).
///
/// One spelling has one today: `template-type<Subject, Owner, 'TName'>` names
/// the owner whose `@template` list is being indexed, and PHPStan reads that
/// argument as a class name, never as a parameterized type. Writing
/// `template-type<Box<T>, Box<T>, 'T'>` there would be the wrong docblock, so
/// asking for type arguments would be asking for a mistake.
///
/// The *subject* (argument 0) is an ordinary type position and keeps reporting:
/// `template-type<Box, Box, 'T'>` names `Box` bare where a `Box<T>` belongs.
/// The template name (argument 2) is a quoted literal, a [`PKind::Const`] that
/// yields nothing anyway. Only the exact three-argument shape is exempt —
/// any other arity is not this utility type, whatever it is spelled like.
fn template_type_owner_arg(base: &str, arity: usize) -> Option<usize> {
    is_template_type(base, arity).then_some(1)
}

/// Whether a [`PKind::Generic`] node is PHPStan's `template-type<Subject, Owner,
/// 'TName'>` utility written at the arity that means anything — the one spelling
/// [`Cx::resolve_template_types`] rewrites and [`template_type_owner_arg`] exempts.
///
/// Case-insensitive and `\`-blind, matching how the contract lane's
/// `KNOWN_UNENFORCED` floor recognizes the same name (issue #360). Any other arity
/// is not this utility type, whatever it is spelled like: it keeps that floor and
/// no rewrite touches it.
pub(crate) fn is_template_type(base: &str, arity: usize) -> bool {
    arity == 3 && base.trim_start_matches('\\').eq_ignore_ascii_case("template-type")
}
