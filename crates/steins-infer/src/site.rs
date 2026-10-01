//! One resolver for what runs at a site (ADR-0099 §2).
//!
//! The effect lane (ADR-0005) and the throw lane (ADR-0040) ask the same
//! question at every call-like and construct-like site of a body: *what code can
//! run here?* [`resolve_site`] answers it once, for the lane's [`Knowledge`]:
//! the [`Target`]s the site runs (edges to project bodies, engine functions,
//! methods and constructors with the lane's own rows attached, a thrown class, a
//! construct's label), what the engine may run implicitly on the site's operands
//! ([`Reach`]), and the [`GapKind`]s that make the answer incomplete.
//!
//! **A lane never scans a tree, never resolves a name and never reads a catalog
//! row.** It iterates a declaration's [`SiteOrigin`]s, hands each to
//! [`resolve_site`], and folds the [`ResolvedSite`] into its own row. The rows
//! it sees were read by [`engine`], on its axis only, and a row the catalog does
//! not have arrives as a gap rather than as an absence.
//!
//! This slice (#863) is a refactor and must not move an answer, so the
//! [`Knowledge`] per lane reproduces what each lane knew before: the effect lane's
//! predicates exactly, and the throw lane's, unsafe default included. #864 makes
//! the knowledge one.

mod contract;
pub(crate) mod engine;
pub(crate) mod method;
pub(crate) mod reach;
mod resolve;

use std::collections::BTreeSet;

use steins_db::PluginFacts;
use steins_syntax::{ConstructKind, SiteKind, SiteOrigin};

use crate::Sym;
pub(crate) use method::{NewTarget, engine_exit, new_origin, resolve_new};
pub(crate) use resolve::resolve_site;

/// Why a site's answer is **incomplete** (ADR-0099 §5): the reason a body's
/// effect or throw set may be missing something, where it used to be a bare
/// `exhaustive: false`. An own row carries the set its sites produced and is
/// exhaustive exactly when that set is empty; propagation joins the sets' emptiness
/// over the edges a body reaches, as it joined the bit.
///
/// Every place the bit was cleared maps to one kind below. The variants are
/// numbered by the facts payload's codec, so a new one is **appended**, never
/// inserted (the payload is decoded past the analyzer gate, so no
/// `SCHEMA_VERSION` bump follows).
///
/// Nothing surfaces the kinds yet (#864 does); they are recorded so that every
/// coverage change in an A/B can name its cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum GapKind {
    /// The callee is computed: a `$f()` call that is not a body-local callback, a
    /// `$o->$m()` or `$c::m()` call, `static::m()`. Source: [`SiteKind::Dynamic`]
    /// (both lanes' `Opaque` / `Taint`).
    DynamicCallee,
    /// The class cannot be named: a `new $c()`, an anonymous class that may bring
    /// its own constructor, or a receiver or `new` whose class chain leaves the
    /// analysed universe or holds no such member. Source: [`SiteKind::Dynamic`]
    /// `New` / `AnonymousClass`, `resolve_new`'s `Unknown`, and a method call whose
    /// chain resolves nowhere.
    UnknownClass,
    /// A statically named function that nothing resolves: no project body, no
    /// catalog knowledge, an ambiguous or shadowed name. Includes a call whose
    /// named or spread arguments defeat the arity a certification needs, which the
    /// code does not tell apart from an uncatalogued name today. Source:
    /// `FnResolution::Unknown` at a call (either lane).
    UnknownFunction,
    /// A method call whose receiver names a class but whose body a subclass may
    /// replace (a non-final `$this->m()`, `self::m()`), or that is a private method
    /// of another class. Source: the open arms of the method edge.
    OpenMethod,
    /// An ADR-0067 declared receiver (`f(Repo $r) { $r->find(); }`,
    /// `$this->repo->find()`) with no checked bound to import. The throw lane
    /// reads every such receiver as this, whatever the interface declares.
    DeclaredReceiver,
    /// An interop envelope (ADR-0082) answered the call: its labels enter the
    /// declared lane, and nothing has checked them, so the answer stays open.
    InteropEnvelope,
    /// A callback the site passes that no body or catalog row resolves: a slot
    /// filled by an unresolvable value, or a named function that resolves nowhere.
    UnresolvedCallback,
    /// Code the analysis never sees: `eval`, an `include` or `require`
    /// (ADR-0046's amendment).
    UnseenCode,
    /// An argument may reach user code the call site does not rule out (issue
    /// #856): a coerced `string` parameter that holds an object, a loose
    /// comparison, a builtin handed over as a callback. Source: [`Reach::Possible`].
    UserCodeReach,
    /// A state construct whose label is not inferred yet (ADR-0055's amendment of
    /// 2026-09-26).
    StateConstruct,
    /// A thrown or rethrown class the scan cannot name: `throw <expr>`, or a
    /// rethrow of a catch parameter whose clause named a type that did not resolve.
    UnresolvedThrow,
    /// A known engine class has no row on the **effect** axis (`new SomeEngine`,
    /// `SomeEngine::m()`), or one only a final method can use.
    NoEffectRow,
    /// A known engine class has no row on the **throw** axis.
    NoThrowRow,
}

impl GapKind {
    /// Every kind, in the order the facts payload's codec numbers them.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 13] = [
        Self::DynamicCallee,
        Self::UnknownClass,
        Self::UnknownFunction,
        Self::OpenMethod,
        Self::DeclaredReceiver,
        Self::InteropEnvelope,
        Self::UnresolvedCallback,
        Self::UnseenCode,
        Self::UserCodeReach,
        Self::StateConstruct,
        Self::UnresolvedThrow,
        Self::NoEffectRow,
        Self::NoThrowRow,
    ];
}

/// What a lane knows, and so how [`resolve_site`] reads a site for it
/// (ADR-0099 §3). In this slice each lane keeps its own knowledge, reproduced
/// exactly; #864 replaces both with one.
pub(crate) enum Knowledge<'a> {
    /// The effect lane: a builtin is known by an effect color or an out-parameter
    /// row, a call may be certified pure at its arity or its operands, a declared
    /// receiver imports its interface's envelope, the plugin channel colours
    /// what nothing else resolves (ADR-0068), and the sites that prove a label of
    /// their own (output, exit, `eval`, inclusion) do.
    Effects { plugins: &'a PluginFacts },
    /// The throw lane as it stood before the sites were shared: a builtin is known
    /// by an effect color alone, a known name with no throw row is **throwless**
    /// (the legacy default, see [`engine::legacy_throws`]), a declared receiver is
    /// unresolvable, the call-site reach rule is not asked, and the constructs the
    /// effect lane owns (output, exit, `eval`, inclusion, state) are not its sites.
    ThrowsLegacy,
}

/// One resolved site: what runs, what it may reach, and why the answer is open.
///
/// A site the lane does not record resolves to an empty value.
#[derive(Debug, Default)]
pub(crate) struct ResolvedSite {
    /// What runs at the site, in the order a report names it: a call's edge
    /// before the callbacks it hands over, a contract's own labels last.
    pub(crate) targets: Vec<Target>,
    /// Whether an operand may reach user code through the engine.
    pub(crate) reach: Reach,
    /// Why the answer is incomplete; the site is covered exactly when empty.
    pub(crate) gaps: BTreeSet<GapKind>,
}

/// One thing that runs at a site.
#[derive(Debug)]
pub(crate) enum Target {
    /// A project body: a call edge in the lane's graph.
    Edge(Edge),
    /// An engine function, method or constructor, with the lane's rows.
    Engine(Hit),
    /// A class a `throw` raises (throw lane): its FQN and how a fact displays it.
    Thrown { class: String, display: String },
    /// Declared-lane effect labels (ADR-0067, ADR-0068): a checked or interop
    /// bound at a call, or the plugin channel's colouring of an unresolved name.
    Declared(Vec<String>),
    /// A language construct proving a label of its own: `(label, spelling)`.
    Construct { label: &'static str, spelling: &'static str },
}

/// A call edge to a project body.
#[derive(Debug)]
pub(crate) struct Edge {
    pub(crate) sym: Sym,
    /// The callee's `…?` does not propagate across this edge: an ADR-0063
    /// contract the call site decided in full. Its proven findings still do.
    pub(crate) untainting: bool,
}

impl Edge {
    pub(crate) const fn call(sym: Sym) -> Target {
        Target::Edge(Self { sym, untainting: false })
    }
}

/// What kind of callee a [`Hit`] is, for how a lane names it and attributes it.
#[derive(Debug)]
pub(crate) enum HitKind {
    /// A builtin function, attributed as `policy.function_attribution(callee)`.
    Function,
    /// A project function's `@pure-unless-parameter-passed` row, produced at the
    /// call site but the callee's contract: attributed as `callee`, the project
    /// function's FQN, and named in a report by its declared name.
    Contract { declared: String },
    /// An engine class's method, attributed as `method_attribution(callee, method)`.
    Method,
    /// An engine class's constructor, reached by `new` or `parent::__construct`.
    Constructor,
}

/// An engine function, method or constructor a site runs, with the rows the
/// catalog states for it on the lane's axis. The other axis is empty.
#[derive(Debug)]
pub(crate) struct Hit {
    pub(crate) kind: HitKind,
    /// The catalog name: the builtin function (lowercase), or the engine class
    /// FQN. For a [`HitKind::Contract`], the project function's FQN.
    pub(crate) callee: String,
    /// The method's name; empty for a function.
    pub(crate) method: String,
    /// How a finding's provenance names the callee: the builtin's catalog name,
    /// or the source spelling of a method or constructor (`$this->m`, `new Foo`).
    pub(crate) origin: String,
    /// The callee as the call spelled it (`name.simple()`) for a function; the
    /// same as [`Self::origin`] for a method or constructor.
    pub(crate) spelled: String,
    /// The effect labels of the callee's row (effect lane).
    pub(crate) labels: Vec<&'static str>,
    /// The thrown classes of the callee's row (throw lane).
    pub(crate) throws: &'static [&'static str],
}

impl Hit {
    /// How a report names the callee: `strlen()`, `$this->m()`, `new Foo`.
    pub(crate) fn shown(&self) -> String {
        match &self.kind {
            HitKind::Function => format!("{}()", self.spelled),
            HitKind::Contract { declared } => format!("{declared}()"),
            HitKind::Method => format!("{}()", self.origin),
            HitKind::Constructor => self.origin.clone(),
        }
    }

    /// How a throw fact names its origin: `strlen()`, `new Foo`,
    /// `parent::__construct`.
    pub(crate) fn throw_origin(&self) -> String {
        match &self.kind {
            HitKind::Function => format!("{}()", self.spelled),
            _ => self.origin.clone(),
        }
    }
}

/// Whether the engine may run user code through a site's operands (ADR-0099 §4,
/// issue #856).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reach {
    /// The lane did not ask: the throw lane reads no operand shapes yet (#864).
    #[default]
    Unasked,
    /// Every operand is ruled out, or the callee reaches nothing.
    RuledOut,
    /// Some operand may reach user code: the site carries [`GapKind::UserCodeReach`].
    Possible,
}

/// Whether the throw lane records `site` (it has a [`steins_syntax::ThrowOrigin`]
/// view): everything but the constructs the effect lane owns. A `match` without a
/// `default` is the one construct both record.
pub(crate) fn records_throw(site: &SiteOrigin) -> bool {
    !matches!(&site.kind, SiteKind::Construct(k) if *k != ConstructKind::MatchNoDefault)
}
