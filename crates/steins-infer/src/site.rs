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
/// The kinds surface in `annotate --format json` and the effect baseline, so that
/// every coverage change in an A/B names its cause.
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
    /// `FnResolution::Unknown` at a call (either lane). A known builtin the lane has
    /// no row for is [`Self::NoEffectRow`] or [`Self::NoThrowRow`] instead.
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
    ArgumentList,
    /// A builtin that raises only when a flags argument asks it to
    /// (`json_encode(…, JSON_THROW_ON_ERROR)`), called with flags the scan cannot
    /// read as a constant without that flag. Appended after #863's kinds, so the
    /// codec numbers of the earlier ones did not move.
    FlagDependentThrow,
}

impl GapKind {
    /// Every kind, in the order the facts payload's codec numbers them.
    pub(crate) const ALL: [Self; 15] = [
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
    /// The site asked nothing of its operands: a construct, a throw, a project
    /// call.
    #[default]
    Unasked,
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
    !matches!(
        &site.kind,
        SiteKind::Construct(k)
            if !matches!(k, ConstructKind::MatchNoDefault | ConstructKind::Eval | ConstructKind::Include(_))
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    //! Gap kinds ride the facts payload by variant index, so their order is a
    //! format fact (ADR-0099 §5.4). The payload is decoded past the analyzer gate,
    //! which is why a new kind takes no `SCHEMA_VERSION` bump.
    use std::collections::BTreeSet;

    use steins_db::{EffectsPolicy, PluginFacts};
    use steins_syntax::SourceTree;

    use super::GapKind;
    use crate::facts::{FileFacts, facts_payload, fill_rows, read_facts};
    use crate::project::{FileUnit, Index, LazyTree};

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
        assert_eq!(mask.union(GapMask::of(&BTreeSet::from([GapKind::OpenMethod]))).names().len(), 3);
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

    /// The kinds the resolver records, by the site shape that produces each, are
    /// the ones the own rows carry — and a body with none is exhaustive.
    #[test]
    fn the_resolver_records_the_kind_each_site_shape_produces() {
        let src = "<?php
interface Repo {}
class Open { public function m() {} public function run($f, $c, Repo $r, $o) {
    $f(); new $c(); unknown_fn(); $this->m(); $r->find();
    new Engine(); array_keys(...$o); array_walk($o, 'no_such_fn'); strtoupper($o);
    throw $o;
}
public function ev($c) { eval($c); } }
final class Closed { public function leaf() { return 1; } public function run() { return $this->leaf(); } }";
        let facts = facts_of(src);
        let rows = facts.rows.as_ref().expect("rows filled");
        let sym = |class: &str, method: &str| crate::Sym::Method(class.to_ascii_lowercase(), method.to_owned());
        let effects = |class: &str, method: &str| {
            rows.effects.iter().find(|(s, _)| *s == sym(class, method)).expect("a row").1.gaps.clone()
        };
        let throws = |class: &str, method: &str| {
            rows.throws.iter().find(|(s, _)| *s == sym(class, method)).expect("a row").1.gaps.clone()
        };
        use GapKind::*;
        let effect_expected: BTreeSet<GapKind> = [
            DynamicCallee, UnknownClass, UnknownFunction, OpenMethod, DeclaredReceiver, NoEffectRow,
            ArgumentList, UnresolvedCallback, UserCodeReach,
        ]
        .into_iter()
        .collect();
        assert_eq!(effects("Open", "run"), effect_expected);
        // `eval` is unseen code in both lanes.
        assert_eq!(effects("Open", "ev"), BTreeSet::from([UnseenCode]));
        assert_eq!(throws("Open", "ev"), BTreeSet::from([UnseenCode]));
        assert!(effects("Closed", "run").is_empty() && effects("Closed", "leaf").is_empty());
        let throw_expected: BTreeSet<GapKind> = [
            DynamicCallee, UnknownClass, UnknownFunction, OpenMethod, DeclaredReceiver,
            NoThrowRow, UnresolvedCallback, UnresolvedThrow, UserCodeReach,
        ]
        .into_iter()
        .collect();
        assert_eq!(throws("Open", "run"), throw_expected);
        assert!(throws("Closed", "run").is_empty());
    }
}
