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
//! Both lanes resolve with one [`Knowledge`] (ADR-0099 §3, #864): a spelling is a
//! builtin exactly when [`steins_catalog::knows`] says so, a known name with no
//! row on a lane's axis is a coverage gap on that axis and never a pure or
//! throwless call, and both lanes read an operand that may reach user code, and
//! unseen code (`eval`, an inclusion), as gaps.

mod contract;
pub(crate) mod engine;
pub(crate) mod method;
mod operator;
mod printf;
pub(crate) mod reach;
mod resolve;
mod setting;

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
/// Every place the bit was cleared maps to one kind below, and a site names the
/// same cause in either lane. The variants are numbered by the facts payload's
/// codec, by position. That payload is read only behind the analyzer fingerprint
/// (ADR-0092's amendment of 2026-09-27), so a stored file never meets a binary
/// that numbers them differently and no `SCHEMA_VERSION` bump follows a change;
/// new kinds are appended anyway, so the numbers the tests pin stay put.
///
/// The kinds surface in `annotate --format json` and the effect baseline, so that
/// every coverage change in an A/B names its cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum GapKind {
    /// The callee is computed: a `$f()` call that is not a body-local callback, a
    /// `$o->$m()` or `$c::m()` call, and the late-bound `static::m()` and `new
    /// static` of a class a subclass can extend. Source: [`SiteKind::Dynamic`]
    /// (both lanes' `Opaque` / `Taint`), and `resolve_new`'s `static` arm.
    DynamicCallee,
    /// The class cannot be named: a `new $c()`, an anonymous class that may bring
    /// its own constructor, a `self`, `parent` or `$this` with no class in scope, or
    /// a class no project file declares and the catalog does not know (a method or
    /// `new` whose chain leaves the analysed universe). Source: [`SiteKind::Dynamic`]
    /// `New` / `AnonymousClass`, and `resolve_new`'s and `method_edge`'s unresolved
    /// arms.
    UnknownClass,
    /// A statically named function that nothing resolves: no project body, no
    /// catalog knowledge, an ambiguous or shadowed name. Source:
    /// `FnResolution::Unknown` at a call (either lane). A known builtin the lane has
    /// no row for is [`Self::NoEffectRow`] or [`Self::NoThrowRow`] instead, and a
    /// named or spread argument list that defeats an arity is
    /// [`Self::ArgumentList`].
    UnknownFunction,
    /// A method call whose body another class may replace and that is not a
    /// non-final `$this` ([`Self::NonFinalThis`]): a private method of another
    /// class, or an engine method a declared receiver's subclass may replace.
    /// Source: the open arms of the method edge and of the engine method row.
    OpenMethod,
    /// An ADR-0067 declared receiver (`f(Repo $r) { $r->find(); }`,
    /// `$this->repo->find()`) with no checked bound to import. The throw lane
    /// reads every such receiver as this, whatever the interface declares (ADR-0099
    /// §8's ADR-0067 bullet).
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
    /// The catalog has no row on the **effect** axis for what the chain reaches: a
    /// known builtin function with no colour, out-parameter row or certification
    /// (`trim`, `array_map`), or the chain leaves the project at a class the project
    /// does not declare (the engine's, as far as the catalog can tell: `new
    /// SomeEngine`, `SomeEngine::m()`, a declared receiver of a class nothing
    /// declares).
    NoEffectRow,
    /// The same, on the **throw** axis: a known builtin that is neither on the
    /// audited throwless table nor carries a throw row ([`steins_catalog::throws_of`]),
    /// or an engine class whose constructor has none.
    NoThrowRow,
    /// A named or spread argument list that defeats the positional arity a
    /// certification needs: `array_keys(...$a)` cannot be told from
    /// `array_keys($a, $v)`, and only the one-argument form is certified pure.
    /// Effect lane only: the throw lane reads that call's operands as a
    /// [`Self::UserCodeReach`].
    ArgumentList,
    /// A builtin that raises only when a flags argument asks it to
    /// (`json_encode(…, JSON_THROW_ON_ERROR)`), called with flags the scan cannot
    /// read as a constant without that flag. Appended after #863's kinds, so the
    /// codec numbers of the earlier ones did not move.
    FlagDependentThrow,
    /// A method call on a class whose whole chain the project holds, and none
    /// declares the method: `__call` or `__callStatic` may answer, or the call is
    /// an `Error`. Source: `method_edge`'s `NotFoundChainComplete`.
    MethodNotFound,
    /// A `$this->m()` or `self::m()` in a class a subclass can extend, where a
    /// subclass may replace `m` (ADR-0099 §5.1's "non-final `$this`"). Source: the
    /// bound arm of the method edge, and an engine method row a subclass may
    /// replace.
    NonFinalThis,
    /// An operand converted to a string (ADR-0099 §4.3's ToString family: `.`,
    /// interpolation, `(string)`, `echo`, a loose comparison) that may be an
    /// object whose `__toString` the site cannot pin: a class a subclass may give
    /// one, an interface, an unknown class, a chain the project does not hold
    /// end to end. Source: the operator resolver ([`SiteKind::Operator`]).
    OperatorToString,
    /// A property fetch or store on an operand whose class may run `__get`,
    /// `__set`, `__isset`, `__unset` or a property hook (the MagicProp family,
    /// including §4.4's universe gate), a hooked promoted constructor parameter, and
    /// the engine's own code (an inherited constructor, a final accessor) run on an
    /// object whose chain hooks a property (§4.2).
    OperatorMagicProperty,
    /// An offset access on an operand that may be an `ArrayAccess` object (the
    /// ArrayAccess family: `$x[k]`, `isset`, `unset`, destructuring).
    OperatorArrayAccess,
    /// An operand iterated (`foreach`, `yield from`, a spread) that may be a
    /// `Traversable` object whose iterator methods the site cannot pin.
    OperatorIteration,
    /// An operand cloned whose `__clone` (or `__set`, for `clone` with a property
    /// list) the site cannot pin.
    OperatorClone,
    /// A value dropped (`unset`, a reassignment, the end of its scope, a `new`
    /// temporary's own expression) whose class may run `__destruct`: a body-local
    /// `new` binding or a parameter bound to a class that reaches one, itself, in
    /// a subclass, or through a typed property (ADR-0100 §7). A drop is a may-run,
    /// never an edge: the destructor runs in whichever frame releases the last
    /// reference, and the caller may keep one. Source: the Drop family of the
    /// operator resolver.
    Destructor,
    /// A setting read that depends on a value the site cannot see (ADR-0101 §3): a
    /// printf call whose `%s` consumes a value not shown to be a float or not one (a
    /// float renders through the `precision` ini), or whose format is not a literal
    /// the parser reads (a `%f`, `%g` or `%G` reads the locale), or a locale reader
    /// whose mode argument is not a literal (a sort's `$flags`, `substr_compare`'s
    /// `$case_insensitive`, `pathinfo`'s `$flags`, §3.9). A label is proven only
    /// where the read is unconditional for the call as written, so a read conditional on
    /// a value is this gap until the site rules it in or out. Effect lane only. Appended
    /// last, so no earlier kind's codec number moved.
    ValueDependentRead,
}

impl GapKind {
    /// Every kind, in the order the facts payload's codec numbers them.
    pub(crate) const ALL: [Self; 24] = [
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
        Self::ArgumentList,
        Self::FlagDependentThrow,
        Self::MethodNotFound,
        Self::NonFinalThis,
        Self::OperatorToString,
        Self::OperatorMagicProperty,
        Self::OperatorArrayAccess,
        Self::OperatorIteration,
        Self::OperatorClone,
        Self::Destructor,
        Self::ValueDependentRead,
    ];

    /// The kind's spelling on the surfaces that name it (`annotate --format
    /// json`, the effect baseline, `effect-diff`): kebab-case. Unlike the codec
    /// number, a spelling is the kind's public name and does not move.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DynamicCallee => "dynamic-callee",
            Self::UnknownClass => "unknown-class",
            Self::UnknownFunction => "unknown-function",
            Self::OpenMethod => "open-method",
            Self::DeclaredReceiver => "declared-receiver",
            Self::InteropEnvelope => "interop-envelope",
            Self::UnresolvedCallback => "unresolved-callback",
            Self::UnseenCode => "unseen-code",
            Self::UserCodeReach => "user-code-reach",
            Self::StateConstruct => "state-construct",
            Self::UnresolvedThrow => "unresolved-throw",
            Self::NoEffectRow => "no-effect-row",
            Self::NoThrowRow => "no-throw-row",
            Self::ArgumentList => "argument-list",
            Self::FlagDependentThrow => "flag-dependent-throw",
            Self::MethodNotFound => "method-not-found",
            Self::NonFinalThis => "non-final-this",
            Self::OperatorToString => "operator-to-string",
            Self::OperatorMagicProperty => "operator-magic-property",
            Self::OperatorArrayAccess => "operator-array-access",
            Self::OperatorIteration => "operator-iteration",
            Self::OperatorClone => "operator-clone",
            Self::Destructor => "destructor",
            Self::ValueDependentRead => "value-dependent-read",
        }
    }

    /// What the kind means at a site, as a clause a finding can quote after the
    /// kind's spelling (ADR-0100 §3): the `maybe-` siblings of the envelope checks
    /// name the cause of each unbounded site, and a reader should not have to look
    /// the spelling up. Message wording is not contract; the spelling is.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::DynamicCallee => "the callee is computed at run time",
            Self::UnknownClass => "the class cannot be named",
            Self::UnknownFunction => "the function resolves to no known declaration",
            Self::OpenMethod | Self::NonFinalThis => "a subclass may replace the method",
            Self::DeclaredReceiver => {
                "the receiver is known only by its declared type, which carries no checked bound"
            }
            Self::InteropEnvelope => "only an unchecked interop envelope answers the call",
            Self::UnresolvedCallback => "a callback passed here resolves to no known body",
            Self::UnseenCode => "the code it runs (`eval` or an inclusion) is never analyzed",
            Self::UserCodeReach => "an argument may reach user code the call does not rule out",
            Self::StateConstruct => "a state construct whose label is not inferred yet",
            Self::UnresolvedThrow => "the thrown class cannot be named",
            Self::NoEffectRow => "the catalog has no effect row for it",
            Self::NoThrowRow => "the catalog has no throw row for it",
            Self::ArgumentList => {
                "a named or spread argument list defeats the arity the catalog certifies"
            }
            Self::FlagDependentThrow => {
                "the flags argument is not a constant that lacks the throwing flag"
            }
            Self::MethodNotFound => {
                "no declaration in the class chain answers the method, so `__call` or an `Error` does"
            }
            Self::OperatorToString => {
                "an operand may be an object whose `__toString` the site cannot pin"
            }
            Self::OperatorMagicProperty => "a property access may run `__get`, `__set` or a hook",
            Self::OperatorArrayAccess => "an offset access may run `ArrayAccess` methods",
            Self::OperatorIteration => "an iterated operand may run `Traversable` methods",
            Self::OperatorClone => "a cloned operand may run `__clone`",
            Self::Destructor => "a dropped value may run `__destruct`",
            Self::ValueDependentRead => {
                "a setting read depends on a value the site cannot see (a `%s` of a value that \
                 may be a float, a format that is not literal, or a flag that is not literal)"
            }
        }
    }
}

/// A set of [`GapKind`]s as a bit mask: what a propagated row carries, where the
/// fixpoint joins a callee's set into every caller's. Bit `n` is the kind the
/// codec numbers `n`, so a kind appended to [`GapKind`] takes the next bit.
///
/// A propagated set is empty exactly when the row is exhaustive, so the mask
/// names every cause of a `…?` a body inherits as well as the ones it makes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GapMask(u32);

impl GapMask {
    /// The mask of an own row's kinds.
    pub(crate) fn of(kinds: &BTreeSet<GapKind>) -> Self {
        Self(kinds.iter().fold(0, |mask, &kind| mask | 1 << kind as u32))
    }

    /// Whether no kind is in the set: the exhaustive reading.
    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The kinds in the set, in codec order, spelled for a surface.
    pub(crate) fn names(self) -> Vec<&'static str> {
        GapKind::ALL
            .into_iter()
            .filter(|&kind| self.0 & 1 << kind as u32 != 0)
            .map(GapKind::as_str)
            .collect()
    }
}

/// Which of the two questions a resolution answers: they ask the same thing at
/// every site (*what code can run here?*) and read different axes of the
/// catalog for the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    /// Effect labels (ADR-0005).
    Effects,
    /// Escaping throw classes (ADR-0040).
    Throws,
}

/// What a lane knows, and so how [`resolve_site`] reads a site for it
/// (ADR-0099 §3): one body of knowledge, the catalog, read on the lane's axis.
pub(crate) enum Knowledge<'a> {
    /// A builtin is whatever [`steins_catalog::knows`] says. A known name with no
    /// row on the lane's axis is a gap on that axis ([`GapKind::NoEffectRow`],
    /// [`GapKind::NoThrowRow`]); a row, or an empty one the audit evidences, is the
    /// answer. `plugins` is the effect lane's plugin channel (ADR-0068): it colours
    /// what no row covers, in the declared lane, and the gap stays. The throw lane
    /// has none.
    ///
    /// The effect lane also reads a declared receiver's interface envelope
    /// (ADR-0067) and the constructs that prove a label of their own (output,
    /// exit, `eval`, inclusion); the throw lane keeps a declared receiver a gap
    /// and records the one construct it owns, a `match` with no `default`, plus
    /// the unseen code both lanes read.
    Catalog { lane: Lane, plugins: Option<&'a PluginFacts> },
}

impl Knowledge<'_> {
    /// Which axis this knowledge is read on.
    pub(crate) const fn lane(&self) -> Lane {
        let Self::Catalog { lane, .. } = self;
        *lane
    }
}

/// One resolved site: what runs, and why the answer is open.
///
/// What the engine may run through the site's operands is a gap like any other
/// ([`Reach::Possible`] is [`GapKind::UserCodeReach`]), so it is in [`Self::gaps`].
/// A site the lane does not record resolves to an empty value.
#[derive(Debug, Default)]
pub(crate) struct ResolvedSite {
    /// What runs at the site, in the order a report names it: a call's edge
    /// before the callbacks it hands over, a contract's own labels last.
    pub(crate) targets: Vec<Target>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Every operand is ruled out, or the callee reaches nothing.
    RuledOut,
    /// Some operand may reach user code: the site carries [`GapKind::UserCodeReach`].
    Possible,
}

/// Whether the throw lane records `site` (it has a [`steins_syntax::ThrowOrigin`]
/// view): everything but the constructs the effect lane owns alone. A `match`
/// without a `default` (it can raise `\UnhandledMatchError`) and unseen code
/// (`eval`, an inclusion: the code it runs may throw anything) are the constructs
/// both lanes record.
pub(crate) fn records_throw(site: &SiteOrigin) -> bool {
    use ConstructKind::{Eval, Include, MatchNoDefault};
    let both_lanes = |k: &ConstructKind| matches!(k, MatchNoDefault | Eval | Include(_));
    // An operator site resolves to edges and gaps, never to a fact of its own: what
    // it throws is raised in the body of the method it reaches.
    !matches!(&site.kind, SiteKind::Operator { .. })
        && !matches!(&site.kind, SiteKind::Construct(k) if !both_lanes(k))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    //! Gap kinds ride the facts payload by variant index. The payload is read only
    //! behind the analyzer fingerprint (ADR-0092's amendment of 2026-09-27), so no
    //! stored file meets a binary that numbers them differently and a new kind takes
    //! no `SCHEMA_VERSION` bump; the codec test keeps `ALL` complete and in step.
    use std::collections::BTreeSet;

    use steins_db::{EffectsPolicy, PluginFacts};
    use steins_syntax::SourceTree;

    use super::GapKind;
    use crate::facts::{FileFacts, facts_payload, fill_rows, read_facts};
    use crate::project::{FileUnit, Index, LazyTree};

    /// Every kind is numbered by its position in [`GapKind::ALL`], and `ALL` lists
    /// every variant: the one after the last does not decode.
    #[test]
    fn every_gap_kind_round_trips_the_codec_in_variant_order() {
        for (index, kind) in GapKind::ALL.into_iter().enumerate() {
            let bytes = steins_db::wire::to_vec(&kind).expect("a gap kind serializes");
            assert_eq!(bytes, [u8::try_from(index).unwrap()], "{kind:?} is numbered by position");
            let back: GapKind = steins_db::wire::from_slice(&bytes).expect("it round-trips");
            assert_eq!(back, kind);
        }
        // `ALL` lists every variant: the one after the last does not decode.
        let past = [u8::try_from(GapKind::ALL.len()).unwrap()];
        assert!(steins_db::wire::from_slice::<GapKind>(&past).is_err(), "ALL misses a variant");
    }

    /// A mask holds exactly the kinds it was built from, spelled in codec order,
    /// and every kind has its own distinct public spelling.
    #[test]
    fn a_mask_names_its_kinds_in_codec_order() {
        use super::GapMask;
        let kinds: BTreeSet<GapKind> = [GapKind::NoThrowRow, GapKind::DynamicCallee].into();
        let mask = GapMask::of(&kinds);
        assert!(!mask.is_empty() && GapMask::default().is_empty());
        assert_eq!(mask.names(), vec!["dynamic-callee", "no-throw-row"]);
        let all = GapMask::of(&GapKind::ALL.into_iter().collect());
        assert_eq!(all.names().len(), GapKind::ALL.len());
        let spelled: BTreeSet<&str> = GapKind::ALL.into_iter().map(GapKind::as_str).collect();
        assert_eq!(spelled.len(), GapKind::ALL.len(), "spellings are distinct");
        let open = GapMask::of(&BTreeSet::from([GapKind::OpenMethod]));
        assert_eq!(mask.union(open).names().len(), 3);
    }

    /// The facts of a file whose source is `src`, with the own rows the generation
    /// path builds for it.
    fn facts_of(src: &str) -> FileFacts {
        let tree = LazyTree::ready(SourceTree::parse(src));
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        let mut facts = vec![FileFacts::from_tree("t.php", units[0].tree)];
        fill_rows(&mut facts, &units, &index, &PluginFacts::none(), &EffectsPolicy::none());
        facts.remove(0)
    }

    /// Every kind survives the facts payload on an own row of each lane, and the
    /// row decodes equal: the persisted form loses no reason.
    #[test]
    fn every_gap_kind_round_trips_the_facts_payload() {
        let mut facts = facts_of("<?php function f() {}\nfunction g() {}");
        let rows = facts.rows.as_mut().expect("rows filled");
        let all: BTreeSet<GapKind> = GapKind::ALL.into_iter().collect();
        for (_, row) in &mut rows.effects {
            row.gaps = all.clone();
        }
        for (_, row) in &mut rows.throws {
            row.gaps = all.clone();
        }
        assert!(!rows.effects.is_empty() && !rows.throws.is_empty());

        let back = read_facts(&facts_payload(&facts)).expect("the payload decodes");
        let (before, after) = (facts.rows.as_ref().unwrap(), back.rows.as_ref().unwrap());
        assert_eq!(before.effects, after.effects);
        assert_eq!(before.throws, after.throws);
        assert!(after.effects.iter().all(|(_, r)| r.gaps == all && !r.exhaustive()));
        assert!(after.throws.iter().all(|(_, r)| r.gaps == all && !r.exhaustive()));
    }

    /// The gap kinds of the unit `sym` in `src`, in the effect lane and the throw
    /// lane.
    fn kinds_of(src: &str, sym: &crate::Sym) -> (BTreeSet<GapKind>, BTreeSet<GapKind>) {
        let facts = facts_of(src);
        let rows = facts.rows.as_ref().expect("rows filled");
        let effects = &rows.effects.iter().find(|(s, _)| s == sym).expect("an effect row").1;
        let throws = &rows.throws.iter().find(|(s, _)| s == sym).expect("a throw row").1;
        (effects.gaps.clone(), throws.gaps.clone())
    }

    /// A site shape: what it is, a source, the unit holding it, and the kinds each
    /// lane records for it (`[]` for a covered site).
    type Case<'a> = (&'a str, &'a str, crate::Sym, &'a [GapKind], &'a [GapKind]);

    fn func(name: &str) -> crate::Sym {
        crate::Sym::Func(name.to_owned())
    }

    fn method(class: &str, m: &str) -> crate::Sym {
        crate::Sym::Method(class.to_ascii_lowercase(), m.to_owned())
    }

    /// The shapes a function body holds: calls to functions, classes and builtins.
    fn function_shapes() -> Vec<Case<'static>> {
        use GapKind::*;
        vec![
            ("a covered body", "<?php function f() { return 1; }", func("f"), &[], &[]),
            (
                "a computed callee",
                "<?php function f($g) { $g(); }",
                func("f"),
                &[DynamicCallee],
                &[DynamicCallee],
            ),
            (
                "a computed class",
                "<?php function f($c) { new $c(); }",
                func("f"),
                &[UnknownClass],
                &[UnknownClass],
            ),
            (
                "an unknown function",
                "<?php function f() { unknown_fn(); }",
                func("f"),
                &[UnknownFunction],
                &[UnknownFunction],
            ),
            (
                "a class nobody declares",
                // Returned, so no drop site: a `new` that dies in its statement reads the
                // unseen class a second time, as a destructor that may run (ADR-0100 §7).
                "<?php function f() { return new Nowhere(); }",
                func("f"),
                &[UnknownClass],
                &[UnknownClass],
            ),
            (
                "a static call on a class nobody declares",
                "<?php function f() { Nowhere::make(); }",
                func("f"),
                &[UnknownClass],
                &[UnknownClass],
            ),
            (
                "an engine class the catalog knows, with no constructor row",
                "<?php function f() { new ReflectionClass('x'); }",
                func("f"),
                &[NoEffectRow],
                &[NoThrowRow],
            ),
            (
                "an engine method the catalog knows, with no row",
                "<?php function f() { SplFixedArray::fromArray([]); }",
                func("f"),
                &[NoEffectRow],
                &[NoThrowRow],
            ),
            (
                "an engine method with an effect row and no throw row",
                "<?php function f() { DateTime::createFromFormat('Y', 'x'); }",
                func("f"),
                &[],
                &[NoThrowRow],
            ),
            (
                "a known builtin with no row on either axis",
                "<?php declare(strict_types=1); function f(string $s) { return mb_detect_encoding($s); }",
                func("f"),
                &[NoEffectRow],
                &[NoThrowRow],
            ),
            (
                "a spread argument list",
                "<?php function f() { return array_keys(...[[1]]); }",
                func("f"),
                &[ArgumentList],
                &[UserCodeReach],
            ),
            (
                "a spread of a variable, which the callee may take by reference",
                "<?php function f(array $a) { return array_keys(...$a); }",
                func("f"),
                &[ArgumentList, OperatorIteration],
                &[UserCodeReach, OperatorIteration],
            ),
            (
                "an operand that may reach user code",
                "<?php function f($o) { return strtoupper($o); }",
                func("f"),
                &[UserCodeReach],
                &[UserCodeReach],
            ),
            (
                "a callback no body resolves",
                "<?php function f() { array_walk([1], 'no_such_fn'); }",
                func("f"),
                &[UnresolvedCallback],
                &[UnresolvedCallback],
            ),
        ]
    }

    /// The shapes a function body holds that are not a plain call: throws, unseen
    /// code, state, flags and declared receivers.
    fn construct_shapes() -> Vec<Case<'static>> {
        use GapKind::*;
        vec![
            (
                "an unresolvable throw",
                "<?php function f($o) { throw $o; }",
                func("f"),
                &[],
                &[UnresolvedThrow],
            ),
            ("eval", "<?php function f($c) { eval($c); }", func("f"), &[UnseenCode], &[UnseenCode]),
            (
                "an inclusion",
                "<?php function f($p) { include $p; }",
                func("f"),
                &[UnseenCode],
                &[UnseenCode],
            ),
            (
                "a state construct",
                "<?php function f() { global $g; }",
                func("f"),
                &[StateConstruct],
                &[],
            ),
            (
                "flags the scan cannot read",
                "<?php function f($flags) { return json_encode([1], $flags); }",
                func("f"),
                &[],
                &[FlagDependentThrow],
            ),
            (
                "a declared receiver",
                "<?php interface Repo { public function find(): int; }
                 function f(Repo $r) { return $r->find(); }",
                func("f"),
                &[DeclaredReceiver],
                &[DeclaredReceiver],
            ),
            (
                "a declared receiver with an interop envelope",
                "<?php interface Repo { /** @phpstan-pure */ public function find(): int; }
                 function f(Repo $r) { return $r->find(); }",
                func("f"),
                &[InteropEnvelope],
                &[DeclaredReceiver],
            ),
        ]
    }

    /// The shapes a method body holds: the receivers whose class is the unit's.
    fn method_shapes() -> Vec<Case<'static>> {
        use GapKind::*;
        vec![
            (
                "a method no class of a complete chain declares",
                "<?php final class A { public function run() { $this->nope(); } }",
                method("A", "run"),
                &[MethodNotFound],
                &[MethodNotFound],
            ),
            (
                "a non-final $this",
                "<?php class A { public function m() {} public function run() { $this->m(); } }",
                method("A", "run"),
                &[NonFinalThis],
                &[NonFinalThis],
            ),
            (
                "a private method of another class",
                "<?php class A { private function p() {} }
                 class B extends A { public function run() { $this->p(); } }",
                method("B", "run"),
                &[OpenMethod],
                &[OpenMethod],
            ),
            (
                "a late-bound new static",
                "<?php class A { public function __construct() {}
                 public function run() { new static(); } }",
                method("A", "run"),
                &[DynamicCallee],
                &[DynamicCallee],
            ),
            (
                "a late-bound static::m()",
                "<?php class A { public static function m() {}
                 public function run() { static::m(); } }",
                method("A", "run"),
                &[DynamicCallee],
                &[DynamicCallee],
            ),
            (
                "a closed chain, resolved",
                "<?php final class A { public function leaf() { return 1; }
                 public function run() { return $this->leaf(); } }",
                method("A", "run"),
                &[],
                &[],
            ),
        ]
    }

    /// One site shape per body, one expected kind per lane: the kind names the cause
    /// of exactly the gap that shape produces (ADR-0099 §5.1), and the two lanes
    /// name the same cause for the same site.
    #[test]
    fn the_resolver_records_the_one_kind_each_site_shape_produces() {
        let cases = function_shapes().into_iter().chain(construct_shapes()).chain(method_shapes());
        for (shape, src, sym, effects, throws) in cases {
            let (e, t) = kinds_of(src, &sym);
            let want = |kinds: &[GapKind]| kinds.iter().copied().collect::<BTreeSet<_>>();
            assert_eq!(e, want(effects), "effect lane, {shape}:\n{src}");
            assert_eq!(t, want(throws), "throw lane, {shape}:\n{src}");
        }
    }
}
