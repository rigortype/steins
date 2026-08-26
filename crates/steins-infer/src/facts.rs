//! The per-file **facts** (issue #516): the projection of one file's tree that
//! every whole-universe phase reads, and the `facts` section that persists it.
//!
//! ## Why this exists
//!
//! With the walk proportional to the edit (#510, #512, #513), a warm no-change
//! rebuild's remaining cost was not: on a 341-file target, capture 14 ms +
//! **trees 40 ms** + analyze 9 ms. The 40 ms was every file's lowered tree
//! being decoded on every run, and it was decoded because a handful of phases
//! that run over the *whole* universe needed something only the tree carried:
//!
//! | phase | what it read off every tree |
//! |---|---|
//! | [`crate::dam::dam_facts`] | the first parse error, the dynamism sites |
//! | `never_returning_names` | every `: never` scope's name |
//! | the PHP-view guard | whether the file declares `PHP_VERSION_ID` |
//! | the parse-failure sweep | the first error and how many followed |
//! | [`crate::affected`] | the footprint, the declared names, the inheritance refs |
//! | the two fixpoints | every declaration's own row |
//! | the shard rebuild | every declaration, for `PackageShard::add_file` |
//! | the three reporting gates | whether any docblock spells `@throws` / purity / an envelope |
//!
//! Every one of those is a *summary* of a tree, orders of magnitude smaller
//! than the tree. Persisting the summaries is what lets a tree be decoded only
//! for a file that will actually be walked — which is what
//! [`crate::project::LazyTree`] then makes true.
//!
//! ## Resolved or unresolved?
//!
//! Issue #489's pinned design said the own rows' edges must persist
//! **unresolved** and be re-resolved at merge time, because an edge's existence
//! is a function of the merged universe. That is true of the edges and *also*
//! of everything else in an own row — whether a call contributes a builtin's
//! findings or an edge, whether a conditional-purity contract discharges, which
//! interface envelope the declared lane imports — so "persist the row
//! unresolved" would mean persisting the origins themselves, which is a slice
//! of the tree, plus the file's namespace contexts and its line table. It would
//! also need a second implementation of the classifier reading that form.
//!
//! This slice persists the rows **resolved**, and the licence is the affected
//! set the run has already computed. The argument:
//!
//! * An own row of a declaration in file F is a function of F's origins and of
//!   how the merged index resolves the names F *references*. Nothing else: the
//!   classifier runs on a bare [`crate::cx::Cx`] with no dam, no purity oracle
//!   and no PHP view.
//! * `affected` already over-approximates "some resolution F makes could have
//!   moved" — that is precisely what its delta leg and its one-hop call-graph
//!   leg exist for, and a *walk* of F resolves a superset of what the
//!   classifier resolves (the walk resolves the same call sites and then
//!   descends). So `F ∉ affected` implies F's row is what it was.
//! * The run is already willing to replay F's whole diagnostic block on that
//!   same judgement, and replaying a finding is the strictly stronger claim.
//!
//! So: the tree-derived half of a file's facts is licensed by the file's
//! content fingerprint alone; the own rows additionally need `F ∉ affected`.
//! The orchestrator drops the rows of an affected file and recomputes them —
//! from a tree it is loading anyway, because an affected file is walked.
//!
//! ## What the row does *not* cover, stated
//!
//! Two whole-universe reporting passes — `effect_diagnostics` and
//! `throw_diagnostics` — emit from a declaration's own docblock and cannot be
//! summarized, because what they emit depends on the propagated fixpoint. They
//! are gated per file instead ([`FileFacts::spells_throws`],
//! [`FileFacts::spells_envelope`]): a file whose declarations spell no
//! `@throws` produces nothing from the throw pass, so its tree is not touched.
//! The effects pass is coarser — its Liskov leg reads a class's *ancestors'*
//! envelopes, so a file declaring no envelope can still emit — and it therefore
//! loads every tree in a project that declares an envelope anywhere. That is
//! recorded rather than hidden: `#[\Steins\Pure]` is this analyzer's own
//! annotation and the phpstan interop tags are uncommon, so the measured
//! corpus pays nothing for it; narrowing it needs a persisted
//! class → envelope table and belongs with a later slice.
//!
//! ## Codec
//!
//! serde_json inside a per-file payload of `steins-db`'s `facts` section, the
//! same framing as `trace`: strict inverses, `deny_unknown_fields`, every
//! decode failure a [`Miss`] the caller degrades to reading the tree. The name
//! keys travel as 64-bit hashes rather than strings — the footprint is the bulk
//! of the payload and a string table would undo the point (measured: 19,766
//! deduplicated keys over `nikic/PHP-Parser`, 158 KB as hashes). A hash
//! collision is one-sided: it can make two distinct names look like one, which
//! adds an edge or a delta hit and walks a file that need not have been. It
//! cannot hide one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use steins_db::{EffectsPolicy, PackageShard, PluginFacts};
use steins_domain::Certainty;
use steins_gen::Miss;
use steins_syntax::{DynamismKind, IncludePath, RetHintKind, ScopeOwner, SourceTree};

use crate::affected::{alias_key_edges, declared_keys, footprint_keys, inherit_keys};
use crate::project::{FileUnit, Index};
use crate::purity::{EffectFinding, EffectOwnRow, classify_effect_origins};
use crate::throws::{ResolvedCatch, ThrowFact, ThrowOwnRow, classify_throw_origins};
use crate::{Sym, cx::Cx};

// ---------------------------------------------------------------------------
// The key hash.
// ---------------------------------------------------------------------------

/// The stable 64-bit hash the name keys travel as (FNV-1a).
///
/// Stability matters within a run — a file whose facts were decoded and one
/// whose facts were just derived must agree — and across runs of the same
/// analyzer version, which the replay stamp already pins. Deliberately not
/// `DefaultHasher`: its output is explicitly not stable across releases.
#[must_use]
pub(crate) fn key_hash(key: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// The value.
// ---------------------------------------------------------------------------

/// One file's whole-universe projection — see the module docs for the table of
/// what reads which field.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileFacts {
    /// The file's first parse error, when it has one: position, message, and
    /// how many followed. The vendor presumption and the dam verdict are
    /// merge-time questions and are deliberately not baked in.
    pub(crate) parse_error: Option<ParseError>,
    /// The file's dam candidates (ADR-0049 §2), each with the pre-resolved
    /// include target the universe test compares against.
    pub(crate) dynamism: Vec<DamCandidate>,
    /// The simple names of this file's `: never` functions and methods.
    pub(crate) never_returning: Vec<String>,
    /// Whether the file declares a userland `PHP_VERSION_ID` (issue #29).
    pub(crate) version_id_declared: bool,
    /// Whether any declaration's docblock spells `@throws` — the exact gate on
    /// whether `throw_diagnostics` can emit anything for this file.
    pub(crate) spells_throws: bool,
    /// Whether any declaration spells an effect envelope or an interop one.
    pub(crate) spells_envelope: bool,
    /// Whether any declaration's docblock spells a purity-bearing callable.
    pub(crate) spells_purity: bool,
    /// The affected set's footprint, hashed and sorted.
    pub(crate) footprint: Vec<u64>,
    /// The name keys this file declares, hashed and sorted.
    pub(crate) declares: Vec<u64>,
    /// The class keys this file's class-likes inherit from, hashed and sorted.
    pub(crate) inherits: Vec<u64>,
    /// This file's literal `class_alias` edges as `(alias, target)` key pairs.
    pub(crate) alias_edges: Vec<(u64, u64)>,
    /// The file's own [`PackageShard`] contribution, built at the canonical
    /// slot 0 and folded in under its real slot by
    /// [`PackageShard::absorb_file`].
    pub(crate) shard: PackageShard,
    /// The file's fixpoint own rows, or `None` when this run must recompute
    /// them (a changed file, an affected one, a decode that gave everything
    /// else but not these).
    pub(crate) rows: Option<OwnRows>,
}

/// A file's first parse error, as the parse-failure finding needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParseError {
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) message: String,
    /// How many further errors the file has past the first.
    pub(crate) further: usize,
}

/// One dam candidate: a runtime-definition construct's position and kind, with
/// the include target already resolved against the file's own directory.
/// Whether it *stands* is a merge-time question (the vendor presumption, the
/// universe membership test), exactly as `dam.rs`'s own module doc says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DamCandidate {
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) kind: CandidateKind,
}

/// Which runtime-definition construct a [`DamCandidate`] records. The include
/// arm carries the *pre-resolved* normalized target — `None` for a path that
/// can never be benign (unproven, or a relative literal), `Some(p)` for one
/// that is benign exactly when the universe holds `p`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum CandidateKind {
    Eval,
    Include { target: Option<String> },
    ClassAlias,
    DefineDynamic,
}

/// A file's fixpoint own rows: the declarations it contributes, in the order
/// the fixpoints enumerate them, and each one's effect and throw row.
///
/// The sym list carries duplicates and its order is load bearing — the
/// fixpoints' final collect is driven by it (ADR-0048 §4 makes the *answer*
/// order-independent; the collect's iteration order is not).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OwnRows {
    pub(crate) syms: Vec<Sym>,
    pub(crate) effects: Vec<(Sym, EffectOwnRow)>,
    pub(crate) throws: Vec<(Sym, ThrowOwnRow)>,
}

impl FileFacts {
    /// The tree-derived half: everything that is a function of the file's bytes
    /// alone. [`Self::rows`] is left `None` for [`fill_rows`] to complete,
    /// because an own row additionally needs the merged index.
    pub(crate) fn from_tree(path: &str, tree: &SourceTree) -> Self {
        let mut shard = PackageShard::default();
        shard.add_file(0, path, tree);
        let spells = |doc: Option<&String>, needle: &str| doc.is_some_and(|t| t.contains(needle));
        let mut spells_throws = false;
        let mut spells_purity = false;
        let mut spells_envelope = false;
        let own_doc = |doc: Option<&String>| {
            (
                spells(doc, "throws"),
                spells(doc, "pure-callable") || spells(doc, "pure-closure"),
                crate::purity::spells_interop_envelope(doc),
            )
        };
        for f in tree.functions() {
            let (t, p, e) = own_doc(f.docblock.as_ref());
            spells_throws |= t;
            spells_purity |= p;
            spells_envelope |= e || f.effect_envelope.is_some();
        }
        for c in tree.classes() {
            // A class-level tag is a declaration of its own (ADR-0082 §5), and
            // it is read even when no method carries anything.
            spells_envelope |= crate::purity::spells_interop_envelope(c.docblock.as_ref());
            for m in &c.methods {
                let (t, p, e) = own_doc(m.docblock.as_ref());
                spells_throws |= t;
                spells_purity |= p;
                spells_envelope |= e || m.effect_envelope.is_some();
            }
        }
        Self {
            parse_error: parse_error_of(tree),
            dynamism: dam_candidates_of(path, tree),
            never_returning: never_returning_of(tree),
            version_id_declared: tree.php_version_id_declared(),
            spells_throws,
            spells_envelope,
            spells_purity,
            footprint: footprint_keys(tree),
            declares: declared_keys(tree),
            inherits: inherit_keys(tree),
            alias_edges: alias_key_edges(tree),
            shard,
            rows: None,
        }
    }
}

/// One file's first parse error, as the parse-failure finding needs it.
///
/// The three projections below are the ones a run *without* persisted facts
/// still needs per file (`check_project` and every other ungated entry point),
/// so they are their own functions rather than a whole [`FileFacts`]: deriving
/// the rest — the footprint, the shard, the own rows — would be real work on a
/// path that never asks for it.
pub(crate) fn parse_error_of(tree: &SourceTree) -> Option<ParseError> {
    tree.parse_errors().first().map(|first| {
        let pos = tree.position(first.span.start);
        ParseError {
            line: pos.line,
            column: pos.column,
            message: first.message.clone(),
            further: tree.parse_errors().len() - 1,
        }
    })
}

/// One file's dam candidates, with each include's target already resolved
/// against the file's own directory.
pub(crate) fn dam_candidates_of(path: &str, tree: &SourceTree) -> Vec<DamCandidate> {
    tree.dynamism_sites()
        .iter()
        .map(|site| {
            let pos = tree.position(site.span.start);
            DamCandidate {
                line: pos.line,
                column: pos.column,
                kind: match &site.kind {
                    DynamismKind::Eval => CandidateKind::Eval,
                    DynamismKind::Include(ip) => {
                        CandidateKind::Include { target: include_target(ip, path) }
                    }
                    DynamismKind::ClassAlias => CandidateKind::ClassAlias,
                    DynamismKind::DefineDynamic => CandidateKind::DefineDynamic,
                },
            }
        })
        .collect()
}

/// The simple names of every `: never` function and method in one file — the
/// per-file half of the run's never-returning veto set.
pub(crate) fn never_returning_of(tree: &SourceTree) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for scope in tree.scopes() {
        if scope.ret_hint.is_none_or(|h| h.kind != RetHintKind::Never) {
            continue;
        }
        let name = match &scope.owner {
            ScopeOwner::Function(name) => name,
            ScopeOwner::Method { method, .. } => method,
            ScopeOwner::TopLevel | ScopeOwner::Closure { .. } => continue,
        };
        let lower = name.to_ascii_lowercase();
        if !out.contains(&lower) {
            out.push(lower);
        }
    }
    out
}

/// The normalized path an include's benignity test compares against the
/// universe, or `None` for a path that can never be benign.
///
/// The reading is `dam::include_is_benign`'s, moved to write time because it is
/// the half that is a function of the *file* — the universe membership test
/// that consumes it stays where it was, at merge time.
fn include_target(ip: &IncludePath, from: &str) -> Option<String> {
    match ip {
        IncludePath::Unproven => None,
        // `./x` is `Literal("./x")` — not absolute, so it stays unproven (A5:
        // `./` anchors to CWD, not the including file's directory).
        IncludePath::Literal(p) => {
            crate::dam::is_absolute(p).then(|| crate::dam::normalize_path(p))
        }
        IncludePath::DirRelative(suffix) => {
            let rel = suffix.strip_prefix('/').unwrap_or(suffix);
            Some(crate::dam::normalize_path(&crate::dam::join(crate::dam::dir_of(from), rel)))
        }
    }
}

/// Fill in the own rows of every file whose facts do not carry them, from the
/// merged index this run built (issue #516).
///
/// Eager, deliberately, and only on the generation path: the fixpoints are lazy
/// everywhere else and stay so (a project spelling no envelope, no purity and
/// no `@throws` pays nothing for them). Here the rows must exist whether or not
/// a gate fires, because they are persisted — and a run that wrote facts
/// without rows would force the *next* run to decode that file's tree the
/// moment any gate started firing. The cost is bounded by what the run is
/// already doing: rows are computed only for files whose facts are being built
/// fresh, which are the files whose trees this run has in hand anyway.
pub(crate) fn fill_rows(
    facts: &mut [FileFacts],
    units: &[FileUnit<'_>],
    index: &Index,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) {
    for (fi, unit) in units.iter().enumerate() {
        if facts[fi].rows.is_some() {
            continue;
        }
        facts[fi].rows = Some(own_rows_of(units, index, plugins, policy, fi, unit));
    }
}

/// One file's own rows, in the enumeration order both fixpoints use:
/// functions, then each class's methods, then the closure/arrow bodies
/// (ADR-0033).
fn own_rows_of(
    units: &[FileUnit<'_>],
    index: &Index,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
    fi: usize,
    unit: &FileUnit<'_>,
) -> OwnRows {
    let cx = Cx::new(units, index, fi);
    let tree = unit.tree;
    let mut syms: Vec<Sym> = Vec::new();
    let mut effects: HashMap<Sym, EffectOwnRow> = HashMap::new();
    let mut throws: HashMap<Sym, ThrowOwnRow> = HashMap::new();
    let mut order: Vec<Sym> = Vec::new();
    let mut classify = |sym: Sym,
                        class_fqn: Option<&str>,
                        params: &[steins_syntax::Param],
                        effect_origins: &[steins_syntax::EffectOrigin],
                        throw_origins: &[steins_syntax::ThrowOrigin],
                        syms: &mut Vec<Sym>| {
        syms.push(sym.clone());
        if !effects.contains_key(&sym) {
            order.push(sym.clone());
        }
        let erow = effects.entry(sym.clone()).or_insert_with(EffectOwnRow::new);
        classify_effect_origins(&cx, class_fqn, params, effect_origins, plugins, policy, erow);
        let trow = throws.entry(sym).or_insert_with(ThrowOwnRow::new);
        classify_throw_origins(&cx, class_fqn, throw_origins, trow);
    };
    for f in tree.functions() {
        classify(
            Sym::Func(f.fqn.clone()),
            None,
            &f.params,
            &f.effect_origins,
            &f.throw_origins,
            &mut syms,
        );
    }
    for c in tree.classes() {
        for m in &c.methods {
            classify(
                Sym::Method(c.fqn.clone(), m.name.clone()),
                Some(&c.fqn),
                &m.params,
                &m.effect_origins,
                &m.throw_origins,
                &mut syms,
            );
        }
    }
    for scope in tree.scopes() {
        if let ScopeOwner::Closure { def_offset } = &scope.owner {
            classify(
                Sym::Closure(unit.path.to_owned(), *def_offset),
                None,
                &scope.params,
                &scope.effect_origins,
                &scope.throw_origins,
                &mut syms,
            );
        }
    }
    OwnRows {
        syms,
        effects: order
            .iter()
            .map(|s| (s.clone(), effects.remove(s).expect("one row per distinct sym")))
            .collect(),
        throws: order
            .iter()
            .map(|s| (s.clone(), throws.remove(s).expect("one row per distinct sym")))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// The wire form.
// ---------------------------------------------------------------------------

/// One file's facts payload, as the `facts` section carries it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFacts {
    parse_error: Option<ParseError>,
    dynamism: Vec<DamCandidate>,
    never_returning: Vec<String>,
    version_id_declared: bool,
    spells_throws: bool,
    spells_envelope: bool,
    spells_purity: bool,
    /// Hex of the packed little-endian key hashes — one string rather than a
    /// JSON array of numbers, which is both smaller and faster to scan.
    footprint: String,
    declares: String,
    inherits: String,
    alias_edges: String,
    shard: PackageShard,
    rows: StoredRows,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRows {
    syms: Vec<Sym>,
    effects: Vec<(Sym, StoredEffectRow)>,
    throws: Vec<(Sym, StoredThrowRow)>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEffectRow {
    findings: Vec<EffectFinding>,
    declared: Vec<String>,
    exhaustive: bool,
    edges: Vec<Sym>,
    untainting: Vec<Sym>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredThrowRow {
    /// `(fact, certainty)` pairs — a JSON object cannot key on a struct.
    facts: Vec<(ThrowFact, u8)>,
    exhaustive: bool,
    edges: Vec<(Sym, Vec<Vec<ResolvedCatch>>)>,
}

/// The three [`Certainty`] values as their wire bytes; anything else is a
/// decode failure, so a doctored payload cannot invent a fourth.
fn certainty_byte(c: Certainty) -> u8 {
    match c {
        Certainty::No => 0,
        Certainty::Maybe => 1,
        Certainty::Yes => 2,
    }
}

fn certainty_of(b: u8) -> Option<Certainty> {
    match b {
        0 => Some(Certainty::No),
        1 => Some(Certainty::Maybe),
        2 => Some(Certainty::Yes),
        _ => None,
    }
}

/// Pack sorted key hashes into lowercase hex.
fn pack(keys: &[u64]) -> String {
    let mut out = String::with_capacity(keys.len() * 16);
    for key in keys {
        out.push_str(&format!("{key:016x}"));
    }
    out
}

/// Strict inverse of [`pack`]: the length must be a multiple of 16 and every
/// character a hex digit.
fn unpack(text: &str) -> Option<Vec<u64>> {
    if !text.len().is_multiple_of(16) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 16);
    for chunk in text.as_bytes().chunks(16) {
        let s = std::str::from_utf8(chunk).ok()?;
        out.push(u64::from_str_radix(s, 16).ok()?);
    }
    Some(out)
}

/// Serialize one file's facts. Infallible: every field is plain data.
#[must_use]
pub(crate) fn facts_payload(facts: &FileFacts) -> Vec<u8> {
    let rows = facts.rows.as_ref().expect("facts are persisted with their rows filled in");
    let mut alias = Vec::with_capacity(facts.alias_edges.len() * 2);
    for (a, t) in &facts.alias_edges {
        alias.push(*a);
        alias.push(*t);
    }
    let stored = StoredFacts {
        parse_error: facts.parse_error.clone(),
        dynamism: facts.dynamism.clone(),
        never_returning: facts.never_returning.clone(),
        version_id_declared: facts.version_id_declared,
        spells_throws: facts.spells_throws,
        spells_envelope: facts.spells_envelope,
        spells_purity: facts.spells_purity,
        footprint: pack(&facts.footprint),
        declares: pack(&facts.declares),
        inherits: pack(&facts.inherits),
        alias_edges: pack(&alias),
        shard: facts.shard.clone(),
        rows: StoredRows {
            syms: rows.syms.clone(),
            effects: rows
                .effects
                .iter()
                .map(|(sym, row)| {
                    let mut findings: Vec<EffectFinding> = row.findings.iter().cloned().collect();
                    findings.sort();
                    let mut declared: Vec<String> = row.declared.iter().cloned().collect();
                    declared.sort();
                    let mut edges: Vec<Sym> = row.edges.iter().cloned().collect();
                    edges.sort();
                    let mut untainting: Vec<Sym> = row.untainting.iter().cloned().collect();
                    untainting.sort();
                    (
                        sym.clone(),
                        StoredEffectRow {
                            findings,
                            declared,
                            exhaustive: row.exhaustive,
                            edges,
                            untainting,
                        },
                    )
                })
                .collect(),
            throws: rows
                .throws
                .iter()
                .map(|(sym, row)| {
                    let mut facts: Vec<(ThrowFact, u8)> =
                        row.facts.iter().map(|(f, c)| (f.clone(), certainty_byte(*c))).collect();
                    facts.sort();
                    (
                        sym.clone(),
                        StoredThrowRow {
                            facts,
                            exhaustive: row.exhaustive,
                            edges: row.edges.clone(),
                        },
                    )
                })
                .collect(),
        },
    };
    serde_json::to_vec(&stored).expect("a facts payload serializes")
}

/// Decode one file's facts. Every way the bytes can be wrong is a [`Miss`],
/// which the caller degrades to reading the file's tree.
pub(crate) fn read_facts(bytes: &[u8]) -> Result<FileFacts, Miss> {
    let corrupt = || Miss::Corrupt("facts payload is not a fact set");
    let stored: StoredFacts = serde_json::from_slice(bytes).map_err(|_| corrupt())?;
    let alias = unpack(&stored.alias_edges).ok_or_else(corrupt)?;
    if !alias.len().is_multiple_of(2) {
        return Err(corrupt());
    }
    let mut throws = Vec::with_capacity(stored.rows.throws.len());
    for (sym, row) in stored.rows.throws {
        let mut facts: HashMap<ThrowFact, Certainty> = HashMap::with_capacity(row.facts.len());
        for (fact, byte) in row.facts {
            facts.insert(fact, certainty_of(byte).ok_or_else(corrupt)?);
        }
        throws.push((sym, ThrowOwnRow { facts, exhaustive: row.exhaustive, edges: row.edges }));
    }
    Ok(FileFacts {
        parse_error: stored.parse_error,
        dynamism: stored.dynamism,
        never_returning: stored.never_returning,
        version_id_declared: stored.version_id_declared,
        spells_throws: stored.spells_throws,
        spells_envelope: stored.spells_envelope,
        spells_purity: stored.spells_purity,
        footprint: unpack(&stored.footprint).ok_or_else(corrupt)?,
        declares: unpack(&stored.declares).ok_or_else(corrupt)?,
        inherits: unpack(&stored.inherits).ok_or_else(corrupt)?,
        alias_edges: alias.chunks(2).map(|p| (p[0], p[1])).collect(),
        shard: stored.shard,
        rows: Some(OwnRows {
            syms: stored.rows.syms,
            effects: stored
                .rows
                .effects
                .into_iter()
                .map(|(sym, row)| {
                    (
                        sym,
                        EffectOwnRow {
                            findings: row.findings.into_iter().collect(),
                            declared: row.declared.into_iter().collect(),
                            exhaustive: row.exhaustive,
                            edges: row.edges.into_iter().collect(),
                            untainting: row.untainting.into_iter().collect(),
                        },
                    )
                })
                .collect(),
            throws,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key hash is the *stable* one, not `DefaultHasher`: a run that
    /// derived a footprint and a run that decoded one must agree, and the
    /// stamp only pins the analyzer version.
    #[test]
    fn the_key_hash_is_pinned() {
        assert_eq!(key_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(key_hash("c:app\\widget"), key_hash("c:app\\widget"));
        assert_ne!(key_hash("c:a"), key_hash("c:b"));
    }

    /// The packed key form is a strict inverse, and refuses anything that is
    /// not a whole number of hex-spelled keys.
    #[test]
    fn packed_keys_round_trip_and_refuse_garbage() {
        let keys = vec![0u64, 1, u64::MAX, key_hash("f:x")];
        assert_eq!(unpack(&pack(&keys)), Some(keys));
        assert_eq!(unpack(""), Some(Vec::new()));
        assert_eq!(unpack("abc"), None);
        assert_eq!(unpack("zzzzzzzzzzzzzzzz"), None);
    }

    /// Every field of a file's facts survives the disk boundary, own rows
    /// included — and a doctored payload is a `Miss`, never a partial value.
    #[test]
    fn a_fact_set_round_trips_through_its_payload() {
        let tree = SourceTree::parse(
            "<?php\nnamespace App;\nuse RuntimeException;\n\
             /** @throws RuntimeException */\nfunction boom(): int { throw new RuntimeException('x'); }\n\
             class C extends \\App\\Base { public function m(): never { exit(1); } }\n\
             class_alias('app\\\\c', 'shortcut');\ninclude $dynamic;\n",
        );
        let lazy = crate::project::LazyTree::borrowed(&tree);
        let units = [FileUnit { path: "src/a.php", tree: &lazy }];
        let index = Index::from_units(&units);
        let mut facts = vec![FileFacts::from_tree("src/a.php", &tree)];
        fill_rows(&mut facts, &units, &index, &PluginFacts::none(), &EffectsPolicy::none());
        let original = facts.pop().expect("one file");

        assert!(original.spells_throws, "the fixture must exercise the throws gate");
        assert!(!original.never_returning.is_empty(), "and the never-returning leg");
        assert!(!original.dynamism.is_empty(), "and the dam leg");
        assert!(!original.alias_edges.is_empty(), "and the alias leg");

        let decoded = read_facts(&facts_payload(&original)).expect("the payload decodes");
        assert_eq!(decoded, original);

        for doctored in [
            b"}".to_vec(),
            br#"{"parse_error": null}"#.to_vec(),
            {
                let mut v = facts_payload(&original);
                // A certainty byte outside the closed set.
                if let Some(pos) = v.windows(9).position(|w| w == b"\"facts\":[") {
                    v.splice(pos..pos, b"\"extra\":1,".iter().copied());
                }
                v
            },
        ] {
            assert!(read_facts(&doctored).is_err(), "a doctored payload must miss");
        }
    }

    /// The include target is resolved at write time and the universe test is
    /// not — the split ADR-0049 A5's reading needs.
    #[test]
    fn include_targets_resolve_against_the_including_file() {
        assert_eq!(include_target(&IncludePath::Unproven, "src/a.php"), None);
        assert_eq!(include_target(&IncludePath::Literal("./x.php".to_owned()), "src/a.php"), None);
        assert_eq!(
            include_target(&IncludePath::Literal("/proj/x.php".to_owned()), "src/a.php"),
            Some("/proj/x.php".to_owned())
        );
        assert_eq!(
            include_target(&IncludePath::DirRelative("/util.php".to_owned()), "src/a.php"),
            Some("src/util.php".to_owned())
        );
    }
}
