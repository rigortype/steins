//! The Coerce row of ADR-0099 §4.3 (issue #868): a user type declaration that admits
//! `string` converts an object handed to it through `__toString`, in a coercive file.
//!
//! Three boundaries, one rule. An argument handed to a project function's, method's or
//! constructor's parameter, a `return <expr>` and a constructor's write to a typed
//! property each run the conversion when the **declared type admits `string` and does
//! not admit the object as it is**. The operand is then resolved by the ToString family
//! ([`Operator::class`]): an exact class with `__toString` is an edge to it, one proven
//! not `Stringable` runs nothing (the engine raises `TypeError`), a bound is an edge only
//! where the method is final, and an operand nothing is known of is the gap.
//!
//! **Which file's mode governs** differs by boundary, and is the file whose code the
//! engine is running when it checks: a parameter follows the *calling* file (the site's
//! own `strict_types`), a return the file that *declares* the function (the frame's), a
//! property write the file that *writes* it (also the frame's). A callee's file decides
//! only where the parameter's type is spelled, which is read from that file's source.
//!
//! **Which types never convert.** The hint's text is read, not [`steins_syntax::Param::ty`],
//! which lowers `string|array`, `string|iterable` and `string|callable` to nothing though
//! all of them convert. A type with no `string` member converts nothing (`int`, `float`
//! and `bool` raise `TypeError` for an object); `object`, `mixed` and `Stringable` admit
//! the object as it is, so the conversion never runs (`string|Stringable` takes an object
//! that is `Stringable` unconverted and raises for one that is not). Every other member
//! (`int`, `array`, `iterable`, `callable`, a class) admits only what it is: an object
//! that is an instance of a class member, `Traversable` for `iterable` or invokable for
//! `callable` is taken as it is, and the others are converted.

use steins_syntax::{
    ArgShape, CallArg, EffectRecv, NameRef, OperatorConstruct as C, OperatorFamily as F, Param,
    RefKind, SiteOrigin, SourceTree,
};

use super::chain::{Chain, Lookup, lookup};
use super::{Bound, Operator, Subject, gap_kind};
use crate::Sym;
use crate::contract::IsA;
use crate::cx::Cx;
use crate::site::reach::{Frame, Held};
use crate::site::ResolvedSite;

/// The parameters of the project callee a call runs, and the file that declares them (the
/// file whose source spells each parameter's type).
pub(in crate::site) struct Signature<'a> {
    pub(in crate::site) file: usize,
    pub(in crate::site) params: &'a [Param],
    /// The class declaring a method, for the `self` and `parent` its types name.
    pub(in crate::site) class: Option<String>,
}

/// Where a declared type is spelled: the source its names resolve in, at a byte offset, and
/// the class whose `self` and `parent` it may name.
pub(super) struct Spelled<'a> {
    tree: &'a SourceTree,
    offset: u32,
    class: Option<&'a str>,
    parent: Option<String>,
}

impl<'a> Spelled<'a> {
    pub(super) fn new(
        cx: &Cx<'_>,
        (tree, offset): (&'a SourceTree, u32),
        class: Option<&'a str>,
    ) -> Self {
        let parent = class.and_then(|c| cx.parent_fqn(c));
        Self { tree, offset, class, parent }
    }
}

/// A member of a declared type that admits an object as it is, when the object is an
/// instance of it.
enum Member {
    /// `iterable`: a `Traversable`.
    Iterable,
    /// `callable`: a `Closure` or an object whose class declares `__invoke`.
    Callable,
    /// A class or interface, by its resolved name.
    Class(String),
}

/// A declared type read for the one question it answers here: does it convert an object?
pub(super) struct Declared {
    /// The type has a `string` member.
    string: bool,
    /// The type admits any object (`object`, `mixed`) or every `Stringable` one.
    open: bool,
    members: Vec<Member>,
}

impl Declared {
    /// Read the type spelled `text` at `spelled`.
    pub(super) fn parse(text: &str, spelled: &Spelled<'_>) -> Self {
        let mut declared = Self { string: false, open: false, members: Vec::new() };
        for member in members_of(text) {
            declared.add(member, spelled);
        }
        declared
    }

    fn add(&mut self, member: &str, spelled: &Spelled<'_>) {
        match member.to_ascii_lowercase().as_str() {
            "string" => self.string = true,
            "object" | "mixed" => self.open = true,
            "iterable" => self.members.push(Member::Iterable),
            "callable" => self.members.push(Member::Callable),
            "self" | "parent" => {
                let class = if member.eq_ignore_ascii_case("self") {
                    spelled.class.map(str::to_owned)
                } else {
                    spelled.parent.clone()
                };
                self.members.extend(class.map(Member::Class));
            }
            // A scalar, an array, `null` or a keyword type admits no object. `static` admits
            // an instance of the class the method was called on, which is not read here: such
            // a type reads as converting, the over-approximation. An intersection member is
            // no one class.
            "" | "int" | "float" | "bool" | "array" | "null" | "false" | "true" | "void"
            | "never" | "static" => {}
            _ if member.contains('&') => {}
            _ => {
                let fqn = spelled.tree.resolve_class_fqn(&name_ref(member, spelled.offset));
                if fqn.eq_ignore_ascii_case("Stringable") {
                    self.open = true;
                } else {
                    self.members.push(Member::Class(fqn));
                }
            }
        }
    }

    /// Whether an object handed to this type may be converted to a string.
    pub(super) fn converts(&self) -> bool {
        self.string && !self.open
    }
}

/// The members of a declared type spelled `text`: split at the top-level `|`, the `?`
/// prefix and enclosing parentheses removed (`null` is no member that matters).
fn members_of(text: &str) -> Vec<&str> {
    let mut members = Vec::new();
    let mut depth = 0u32;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => {
                members.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    members.push(&text[start..]);
    members
        .into_iter()
        .map(|m| m.trim().trim_start_matches('?').trim().trim_matches(['(', ')']).trim())
        .collect()
}

/// The reference a class name spelled `raw` in a type declaration is, at `offset`.
fn name_ref(raw: &str, offset: u32) -> NameRef {
    if let Some(rest) = raw.strip_prefix('\\') {
        return NameRef { raw: rest.to_owned(), kind: RefKind::FullyQualified, offset };
    }
    let first = raw.split('\\').next().unwrap_or(raw);
    if raw.contains('\\') && first.eq_ignore_ascii_case("namespace") {
        let rest = raw.get(first.len() + 1..).unwrap_or("");
        return NameRef { raw: rest.to_owned(), kind: RefKind::Relative, offset };
    }
    let kind = if raw.contains('\\') { RefKind::Qualified } else { RefKind::Unqualified };
    NameRef { raw: raw.to_owned(), kind, offset }
}

impl<'a> Operator<'a, '_> {
    /// One operand `shape` (its class named by `receiver`) handed to a type `declared`.
    fn coerce(&mut self, declared: &Declared, shape: &ArgShape, receiver: Option<&EffectRecv>) {
        if !declared.converts() {
            return;
        }
        // A parameter the frame never rewrites with an object holds what it was declared
        // with, though the syntax layer cannot name it as a receiver: passing it to a call
        // is a write there, which only callee resolution answers (`Frame::rebound_by_call`).
        let declared_param = match shape {
            ArgShape::Param { name, .. } if receiver.is_none() => {
                (!self.frame.rebound_by_call(self.cx, name)).then(|| EffectRecv::Var(name.clone()))
            }
            _ => None,
        };
        match self.subject(shape, receiver.or(declared_param.as_ref())) {
            Subject::NoObject => {}
            Subject::Opaque => self.gap(),
            Subject::Classes(bounds) => {
                let converted: Vec<&Bound> =
                    bounds.iter().filter(|bound| !self.admitted(declared, bound)).collect();
                for bound in converted {
                    self.class(bound, 0);
                }
            }
        }
    }

    /// Whether every object of `bound` is taken by `declared` as it is: an instance of one
    /// of its members, subclasses included.
    fn admitted(&self, declared: &Declared, bound: &Bound) -> bool {
        declared.members.iter().any(|member| match member {
            Member::Class(fqn) => self.is_a(&bound.fqn, fqn) == IsA::Yes,
            Member::Iterable => self.is_a(&bound.fqn, "Traversable") == IsA::Yes,
            Member::Callable => {
                self.is_a(&bound.fqn, "Closure") == IsA::Yes
                    || matches!(
                        lookup(self.cx, None, &bound.fqn, "__invoke"),
                        Lookup::Found { .. }
                    )
            }
        })
    }

    /// A spread argument unpacks into every position from `from` on, and each element may be
    /// an object: only an operand shown to hold none rules the conversion out.
    fn coerce_spread(&mut self, converting: bool, shape: &ArgShape) {
        if converting && self.frame.held(self.cx, shape) != Held::ObjectFree {
            self.gap();
        }
    }
}

/// The conversions the arguments of one call run on the parameters of its callee `sig`
/// (`None` where no project declaration was found: any argument that may hold an object may
/// be converted).
pub(in crate::site) fn arguments<'a>(
    cx: &Cx<'a>,
    frame: &Frame<'a>,
    args: &[CallArg],
    sig: Option<&Signature<'a>>,
) -> ResolvedSite {
    let mut op = Operator {
        cx,
        frame,
        gap: gap_kind(F::ToString),
        family: F::ToString,
        // Any form that is not a comparison: the operand is read for the conversion alone.
        construct: C::Return,
        member: None,
        out: ResolvedSite::default(),
    };
    if cx.strict() || args.is_empty() {
        return op.out;
    }
    let Some(sig) = sig else {
        op.gap();
        return op.out;
    };
    let tree = cx.units[sig.file].tree;
    let declared: Vec<Option<Declared>> = sig
        .params
        .iter()
        .map(|p| {
            let span = p.hint_span?;
            let spelled = Spelled::new(cx, (tree, span.start), sig.class.as_deref());
            let declared = Declared::parse(tree.source_slice(span)?, &spelled);
            declared.converts().then_some(declared)
        })
        .collect();
    if declared.iter().all(Option::is_none) {
        return op.out;
    }
    let variadic = sig.params.iter().position(|p| p.variadic);
    let mut position = 0;
    for arg in args {
        if arg.spread {
            let converting = declared.iter().skip(position).any(Option::is_some);
            op.coerce_spread(converting, &arg.shape);
            continue;
        }
        let at = match &arg.name {
            Some(name) => sig.params.iter().position(|p| &p.name == name).or(variadic),
            None => {
                position += 1;
                Some(position - 1).filter(|&i| i < declared.len()).or(variadic)
            }
        };
        if let Some(Some(declared)) = at.map(|i| &declared[i]) {
            op.coerce(declared, &arg.shape, arg.receiver.as_ref());
        }
    }
    op.out
}

/// The conversion of a `return <expr>` (`member` is the return type as written) or of a
/// constructor's `$this->p = <expr>` (`member` is the property) at an operator site.
pub(in crate::site) fn value(
    cx: &Cx<'_>,
    frame: &Frame<'_>,
    site: &SiteOrigin,
    (construct, receivers, member): (C, &[Option<EffectRecv>], Option<&str>),
) -> ResolvedSite {
    let mut op = Operator {
        cx,
        frame,
        gap: gap_kind(F::ToString),
        family: F::ToString,
        construct,
        member,
        out: ResolvedSite::default(),
    };
    let (Some(member), Some(shape)) = (member, site.operands.as_deref().and_then(<[_]>::first))
    else {
        return op.out;
    };
    if cx.strict() {
        return op.out;
    }
    let receiver = receivers.first().and_then(Option::as_ref);
    let declared = match construct {
        C::PropertyValue => property_type(&op, member),
        _ => {
            let spelled = Spelled::new(cx, (cx.tree(), site.span.start), frame.class_fqn);
            Some(Declared::parse(member, &spelled))
        }
    };
    if let Some(declared) = declared {
        op.coerce(&declared, shape, receiver);
    }
    op.out
}

/// The declared type of the property `$this->member` a constructor of the frame's class
/// writes: the nearest declaration on a closed chain that the scope sees, not hooked. A
/// chain that is not closed, a hooked property, an undeclared one and an untyped one read
/// nothing: the first two are gaps of the property write already (§4.3's MagicProp row), the
/// others are properties the engine does not convert for.
fn property_type(op: &Operator<'_, '_>, member: &str) -> Option<Declared> {
    let class = op.frame.class_fqn?;
    let chain = Chain::of(op.cx, class);
    if !chain.closed || chain.hooks(Some(member)) || !op.visible_property(&chain, member) {
        return None;
    }
    let (prop, owner) = chain.property(member)?;
    let (file, _) = op.cx.find_class(&owner.fqn)?;
    let tree = op.cx.units[file].tree;
    let span = prop.hint_span?;
    let spelled = Spelled::new(op.cx, (tree, span.start), Some(&owner.fqn));
    Some(Declared::parse(tree.source_slice(span)?, &spelled))
}

/// The parameters of the project method or constructor `sym` names, and its file.
pub(in crate::site) fn method_signature<'a>(cx: &Cx<'a>, sym: &Sym) -> Option<Signature<'a>> {
    let Sym::Method(class, method) = sym else { return None };
    let (file, class) = cx.find_class(class)?;
    let decl = class.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
    Some(Signature { file, params: &decl.params, class: Some(class.fqn.clone()) })
}
