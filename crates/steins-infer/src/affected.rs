//! The affected set (ADR-0092 §5, issue #489 slice B): which files must be
//! walked when a generation is rebuilt, and — by subtraction — which may
//! replay their persisted walk block instead.
//!
//! The pinned rule:
//!
//! ```text
//! affected = changed_files
//!          ∪ { F : footprint(F) ∩ delta_names ≠ ∅ }
//!          ∪ { F : F reaches a changed file within MAX_BINDING_DEPTH in the
//!                  file-level call graph }
//!          ∪ ( every file, if a whole-universe verdict moved )
//! ```
//!
//! The fourth leg is not here: it is the [`crate::walk_plan::UniverseVerdict`]
//! digest, compared against the one the artifact carries, and a mismatch means
//! the orchestrator never asks this module anything. What is here is the first
//! three, plus one refinement the pinned list does not spell and soundness
//! needs (see *Inheritance*).
//!
//! ## What a walk of F reads, and which leg answers for it
//!
//! * **F's own tree** — `changed_files`. Change is *file-level, not semantic*:
//!   a descent diagnostic embeds the callee's position ("bound at f(1) call at
//!   path line N"), so a file whose lines merely moved changes a caller's
//!   message. Any byte moving in a file makes it changed. Since issue #512 the
//!   same predicate — the file's captured content fingerprint against the one
//!   its persisted row carries — also decides whether F's *tree* is loaded or
//!   re-parsed. That is a cost decision on a value a loaded tree and a fresh
//!   parse agree on byte for byte, so it leaves everything below untouched:
//!   the set this module is handed is computed from the same one predicate,
//!   never from which files happened to be parsed.
//! * **The merged index** (every absence verdict, every resolution) — the
//!   `delta_names` leg, whose completeness argument is *The name delta* below.
//! * **Other files' trees**, through the binding descent — the call-graph leg.
//! * **The whole-universe verdicts** (dam, purity oracle, never-returning set,
//!   the PHP view, the property-write obstacle) — the digest, above.
//! * **Run config and engine identity** — the replay stamp, which gates the
//!   whole section rather than any one file.
//! * **Fold answers** — identity-scoped by the fold table (#500) and by the
//!   engine posture inside the stamp: the same key under the same engine is the
//!   same answer.
//!
//! ## The name delta
//!
//! The delta is computed by the orchestrator and handed here as
//! [`AffectedInputs::delta`]; what follows is the argument that a file whose
//! footprint misses it cannot have had a resolution move under it.
//!
//! **Per file, not per package** (issue #510). A name's merged answer is a
//! function of the multiset of declarations under it, and every declaration
//! belongs to exactly one file. So the answer can only move if some *file's*
//! contribution to that name moved — and a file whose bytes are identical to
//! the ones the published generation held contributes exactly what it
//! contributed then. The delta is therefore, over every changed package: the
//! names its OLD shard sites in a file that changed, and the names its NEW
//! shard sites in one ([`PackageShard::contributed_names_from`]). A package
//! whose sources did not move contributes nothing at all — both its sides are
//! the same set.
//!
//! The package-granular reading this replaces (the union of a changed
//! package's whole old and new key sets) was sound and useless: one edited
//! file put every name its package declares into the delta, so in a
//! single-package project — a first-party repo with no vendor tree, the
//! ordinary shape — every file's footprint intersected it and nothing
//! replayed.
//!
//! **Both sides, and why.** Promotion and ambiguity demotion show on the new
//! side; *demotion to absence* — a declaration deleted, a file emptied, a
//! package removed — is visible only on the old side, because nothing in this
//! run's universe names it any more. A cross-package ambiguity demotion needs
//! the name to be *defined* in the changed file, so it is on one side or the
//! other by construction. A package the published generation did not have
//! contributes an empty old side; a package it had and this run does not
//! contributes its whole old key set, since every file it held left it.
//!
//! **Old slots index the old universe.** A shard's sites are universe slots,
//! and slot numbers are not stable across generations (the load path tracks
//! this as `slots_stable`). The old side is therefore resolved through the old
//! shard's own path→slot map: which old *paths* changed decides which old
//! *slots* count, and a raw slot number is never compared across generations.
//!
//! **Unknowable falls back up, never down.** A file whose old slot cannot be
//! determined — an unreadable trace index, a package whose artifact will not
//! decode — makes its package's delta unknowable, and the answer is the
//! wholesale set or, where not even that can be read, walking every file. The
//! one direction that is never allowed is a *smaller* set.
//!
//! **The ambiguity sets ride a changed package wholesale**: a name a package
//! declares twice has no site because it has two, and the shard drops both
//! when it demotes. They are empty in a project that compiles. The other two
//! members that had no site when #510 was filed — the constants and the
//! `class_alias` edge list — have one now, because the measurement asked:
//! `nikic/PHP-Parser`'s back-compat aliases put 22 keys into *every* edit's
//! delta and matched 61 of its 341 files, 18% of the universe walked for an
//! alias no edit touched. [`PackageShard::contributed_names_from`] carries the
//! reading.
//!
//! ## The delta leg and the descent
//!
//! The delta leg is per file, and a walk is not: G descends into F's
//! declaration and re-derives F's body there, so a resolution that moved *under
//! F* can move what G's walk concludes. F is a delta hit; G is not, and G's own
//! footprint never mentions the name.
//!
//! Almost always the ordinary closure already answers for that, and the reason
//! is worth stating because it is what keeps this leg cheap. A resolution moves
//! because some file changed, and that file is the one *declaring* the name — so
//! F, which names it, has an edge straight to it, and G reaches it through F at
//! one hop more. An added declaration, a promotion, an ambiguity demotion: all
//! of them leave the moved declaration standing in a changed file, and all of
//! them are covered without a seed.
//!
//! The exception is **removal**. A deleted file has no slot in this run's
//! universe at all, so it can never be in `changed`, and a name nothing declares
//! any more is an edge to nowhere: F's own hit is the only trace of it, and G is
//! invisible. So a delta hit seeds the descent closure when the key it hit on
//! resolves to **no** declaring file at all — which is a deleted file, a deleted
//! or renamed declaration, a removed package, and nothing else. It is empty for
//! every edit that only adds or modifies, so the leg costs nothing until it is
//! needed.
//!
//! **What it does not cover, stated rather than assumed**: a name declared
//! *twice* that loses one copy still resolves, so its namer keeps an edge, but
//! that edge may reach the surviving declaration rather than the departed one,
//! and a caller one descent above is then missed. Seeding every delta hit closes
//! that too and costs the whole leg: on `nikic/PHP-Parser` one permanently
//! ambiguous class (`Internal\TokenPolyfill`, declared twice under a version
//! guard) rides every edit's delta through the wholesale ambiguity concession,
//! and seeding on it walked 313 of 341 files where this leg walks 14. The
//! residue is the delta leg's own granularity, not this closure's, and belongs
//! with issue #510's line of work rather than here.
//!
//! ## The footprint
//!
//! `footprint(F)` is a projection of F's loaded trace IR — never a walk, and no
//! new persistence: statically-named function references, `hard_class_refs`,
//! `const_refs`, the `class_alias` ends, and the **type expressions** of F's
//! comments. Names are normalized the way the index keys them, and every
//! spelling a resolution *could* take is emitted, not just the one it takes
//! today: an unqualified call in a namespace tries `Ns\f` and then global `f`,
//! and both candidates go in, because a definition appearing at either is a
//! resolution that moved.
//!
//! **No `m:` key at all** (issue #513). A method name is not a delta key — a
//! shard carries no method table, by `PackageShard`'s own documented design —
//! and it is not an edge either: every method resolution in the analyzer enters
//! through `resolve_in_chain(start_fqn, method)`, so the class comes first and
//! the name is looked up *within* it, and the merged index holds no by-name
//! method table for anything to resolve against. A walk therefore cannot reach
//! a declaration that its file does not already reach by naming the class.
//!
//! The comment leg is the one the pinned list does not name, and it is load
//! bearing. `hard_class_refs` excludes docblock positions by design (it is the
//! `class.undefined` firing set, and a docblock name is not a hard error), so
//! without it a file whose only mention of a class is `@return Widget` has no
//! edge to Widget's file — and a caller two hops away, descending through that
//! return, would replay a stale finding. What it reads is what the phpdoc
//! scanners themselves delimit as a type: each tag's type text, the `@template`
//! bounds, the inheritance type arguments, the magic-member subjects
//! ([`docblock_type_texts`]). That is the whole of a docblock any name
//! resolution in the analysis sees, so it is the whole of what a stale answer
//! can hide in. It is still an over-approximation — `@return int the widget
//! count` hands its trailing description along with its type — and that is the
//! right side to err on.
//!
//! It replaces a tokenizer that ran over every identifier of every comment,
//! whose cost the measurement in issue #513 finally priced: the word `param`
//! appears in 164 of `nikic/PHP-Parser`'s files, and one comment's `gettype`
//! met the 173 files declaring a `getType`, so prose alone edged most of that
//! universe to most of it and a core-file edit walked 337 of 341 files. The
//! same edit now walks 20.
//!
//! ## The call graph
//!
//! File-level, and derived from the names a file *references* rather than from
//! the effect/throw own-row edges, which answer a different question. An edge
//! `F → G` says the walk of F may read G's tree: F names a function, class,
//! constant or alias G declares.
//!
//! **Resolution is class-first and upward, and that is the whole design.** A
//! call site's target is found by `resolve_call_target`, which turns every
//! receiver shape into a class FQN — `$this`/`self` the enclosing class,
//! `parent` its parent, `Foo::`/`new Foo`/`(new Foo)->` the named class, a
//! variable receiver the class its heap object carries — and then walks that
//! class's chain **upward** with `resolve_in_chain`. Nothing resolves downward:
//! `resolve_guarded` refuses outright unless the method or its class is
//! `final`/`private`, and `resolve_exact` is reached only where the runtime
//! class is already exact. So the declarations a call site can reach are the
//! named class's and its ancestors', and a method *name* buys nothing.
//!
//! That is why each hop of the backwards closure first closes its frontier
//! **downwards** through the inheritance graph. Forwards, `F → G` reaches G's
//! ancestors too; backwards, the origins of an affected file X are the files
//! naming any class that descends from one of X's. The expansion costs no hop —
//! a chain walk is not a descent — so it runs inside each of the
//! [`MAX_BINDING_DEPTH`] hops rather than consuming them, and a call chain of
//! length k is still covered at k hops however deep the class hierarchies under
//! it are.
//!
//! **The receiver whose class is not named** is the shape this cannot resolve:
//! `$w = make(); $w->render();` names neither `Widget` nor anything of its.
//! It stays sound through the descent chain rather than through a name edge —
//! the caller edges to `make()`'s file, `make()`'s own declaration names
//! `Widget` (a return hint, a `new`, a docblock type; a class the walk knows
//! about got into that store somehow, and the somewhere is a file this chain
//! passes through), so the closure reaches Widget's file one hop later than a
//! name edge would have. `an_untyped_receiver_still_reaches_the_class_its_walk_resolves`
//! pins it.
//!
//! ## Inheritance
//!
//! One refinement the pinned leg list does not spell, added because the
//! `MAX_BINDING_DEPTH` bound does not hold for it: class-chain traversal is not
//! depth-bounded in the analysis — `magic_obstacles_in_reach` follows parents,
//! interfaces and `@mixin` targets transitively, and the declared-receiver
//! lane's descendant closure (ADR-0049 A4) walks the *other* way. So the
//! inheritance graph is closed **without a bound**, in both directions, but as
//! two separate legs rather than as one connected component:
//!
//! * **Subtypes of a changed file are affected.** Their own chain walks climb
//!   through the changed declaration, with no descent in between.
//! * **Supertypes of a changed file are seeds, not conclusions.** A superclass's
//!   walk does not read its subclass's file; what reads it is the *descendant
//!   enumeration* a third file performs, and that file names the superclass —
//!   so seeding the supertype puts every namer of it one hop away, which is
//!   exactly the reach that leg needs.
//!
//! **The two legs are coupled, deliberately.** The call graph's class edge lands
//! on the class's own file and gets the chain above it from the per-hop subtype
//! expansion; the descendant reach that a non-final receiver might suggest comes
//! from the supertype seed instead of from a wider edge. Neither leg is sound
//! without the other, and the reason the affected set's answer to "what about a
//! subclass override?" differs from `resolve_effect_edge`'s (which gates on
//! `is_final`, `private` and exactness) is that the two ask different questions:
//! the effect graph asks whether a propagation *edge* exists, this asks which
//! files a walk *reads*, and reading a class's declaration is an upward walk
//! while enumerating its subclasses is a separate, seeded one.
//!
//! What the undirected connected-component reading this replaces cost was
//! measured on `Seldaek/monolog`, where every handler and formatter shares one
//! interface: editing `Logger.php` seeded **201 of 217 files** before a single
//! call edge was considered. The two-leg reading seeds 4.
//!
//! ## What over-approximation costs, and why the direction is fixed
//!
//! Every judgement here is one-sided: a file wrongly in the affected set is
//! walked, which costs time; a file wrongly out of it replays a stale finding,
//! which is the zero-FP violation the project exists to prevent. So every
//! unknown resolves to *affected*, and the paranoid verifier
//! ([`crate::generation::PARANOID_ENV`]) is what turns "we believe this closure
//! is complete" into a number over a corpus.
//!
//! [`PackageShard::contributed_names_from`]: steins_db::PackageShard::contributed_names_from
//! [`MAX_BINDING_DEPTH`]: crate::MAX_BINDING_DEPTH

use std::collections::{HashMap, HashSet};

use steins_syntax::{Callee, NameRef, RefKind, SourceTree, normalize_const_fqn};

use crate::MAX_BINDING_DEPTH;

/// What the affected-set computation needs about this run.
pub(crate) struct AffectedInputs<'a> {
    /// The universe's lowered trees, in slot order.
    pub(crate) trees: &'a [SourceTree],
    /// Slots whose bytes moved since the published generation — including
    /// every file the published generation did not have at all.
    pub(crate) changed: HashSet<usize>,
    /// The name delta, in [`PackageShard::contributed_names_from`]'s
    /// namespaces — see the module docs for what belongs in it and why.
    ///
    /// [`PackageShard::contributed_names_from`]: steins_db::PackageShard::contributed_names_from
    pub(crate) delta: HashSet<String>,
}

/// Which slots must be walked. Everything else may replay — subject to the
/// caller's own gates (its package loaded, its summaries decoded, the stamp and
/// the universe verdict unmoved).
pub(crate) fn affected_files(inputs: &AffectedInputs<'_>) -> HashSet<usize> {
    let n = inputs.trees.len();
    let decls = DeclTable::build(inputs.trees);

    // One pass over every file: its footprint decides both the delta leg and
    // its outgoing name edges, so the keys are never retained.
    let mut affected: HashSet<usize> = inputs.changed.clone();
    let mut names: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut inherits: Vec<Vec<usize>> = vec![Vec::new(); n];
    // Delta hits on a name **nothing declares any more** — see *The delta leg
    // and the descent* below. Empty for every edit that only adds or modifies.
    let mut undeclared_hits: Vec<usize> = Vec::new();
    for (file, tree) in inputs.trees.iter().enumerate() {
        let mut edges: HashSet<usize> = HashSet::new();
        let mut hit = false;
        let mut unreachable = false;
        footprint(tree, &mut |key: &str| {
            let declaring = decls.files_of(key);
            if inputs.delta.contains(key) {
                hit = true;
                unreachable |= declaring.is_none();
            }
            if let Some(files) = declaring {
                edges.extend(files.iter().copied());
            }
        });
        if hit {
            affected.insert(file);
            if unreachable {
                undeclared_hits.push(file);
            }
        }
        edges.remove(&file);
        names[file] = edges.into_iter().collect();
        inherits[file] = inheritance_edges(tree, &decls, file);
    }

    // `inherits` points a file at the files declaring its supertypes, so its
    // reverse points a file at the files declaring its subtypes.
    let subtypes = reverse(&inherits, n);
    let callers = reverse(&names, n);

    // The seeds — every file whose own walk reads a changed file with no
    // descent in between (see *Inheritance* in the module docs):
    //
    // * the changed files themselves;
    // * their **subtypes**, whose every chain walk climbs through the changed
    //   declaration;
    // * their **supertypes**, which a changed subtype does not move but which
    //   are what a descendant-enumerating file names — a seed rather than a
    //   target, so the file naming one is one hop away either way;
    // * the delta hits on a name this universe no longer declares — the
    //   removal case, argued under *The delta leg and the descent*.
    let mut seeds = closure(&inherits, &inputs.changed, usize::MAX);
    seeds.extend(closure(&subtypes, &inputs.changed, usize::MAX));
    seeds.extend(undeclared_hits);
    affected.extend(seeds.iter().copied());

    // Then the descent closure, backwards, bounded by the depth the binding
    // descent itself stops at. Each hop first closes its frontier **downwards**
    // through the inheritance graph, because a file naming class `A` reads
    // `A`'s whole ancestor chain: backwards from an affected file, the origins
    // are the files naming any of its subtypes. That expansion costs no hop —
    // a chain walk is not a descent — so it runs inside each one.
    let mut targets: HashSet<usize> = HashSet::new();
    let mut frontier: Vec<usize> = seeds.into_iter().collect();
    for _ in 0..MAX_BINDING_DEPTH {
        let mut fresh: Vec<usize> = Vec::new();
        let mut stack = frontier;
        while let Some(node) = stack.pop() {
            if targets.insert(node) {
                fresh.push(node);
                stack.extend(subtypes[node].iter().copied());
            }
        }
        let mut next: Vec<usize> = Vec::new();
        for node in fresh {
            for &caller in &callers[node] {
                if affected.insert(caller) {
                    next.push(caller);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    affected
}

// ---------------------------------------------------------------------------
// The declaration table: which files could satisfy a name reference.
// ---------------------------------------------------------------------------

/// Name key → the files declaring something under it. Rebuilt per run from the
/// trees already in hand (loaded or freshly parsed), so it costs no
/// persistence and cannot go stale.
struct DeclTable {
    files: HashMap<String, Vec<usize>>,
}

impl DeclTable {
    fn build(trees: &[SourceTree]) -> Self {
        let mut files: HashMap<String, Vec<usize>> = HashMap::new();
        let mut add = |key: String, file: usize| {
            let entry: &mut Vec<usize> = files.entry(key).or_default();
            if entry.last() != Some(&file) {
                entry.push(file);
            }
        };
        for (file, tree) in trees.iter().enumerate() {
            for f in tree.functions() {
                add(format!("f:{}", f.fqn), file);
                add(format!("s:{}", f.name.to_ascii_lowercase()), file);
            }
            for c in tree.classes() {
                add(format!("c:{}", c.fqn), file);
            }
            for edge in tree.class_alias_edges() {
                add(format!("c:{}", edge.alias_fqn), file);
            }
            for d in tree.global_const_decls() {
                add(format!("k:{}", d.fqn), file);
            }
        }
        // Second pass for the literal `class_alias` edges (ADR-0049 §2, issue
        // #36 — a literal alias mints an index edge rather than damming). The
        // alias resolves to the *target's* declaration, so a file that names
        // only the alias reads a class body in the target's file and needs an
        // edge there. Mapping the alias to the aliasing file alone (which the
        // first pass does, and which is also true — that file's `class_alias`
        // call is what mints the name) would leave the body's own file
        // unreachable and let its caller replay a stale finding.
        let mut minted: Vec<(String, Vec<usize>)> = Vec::new();
        for tree in trees {
            for edge in tree.class_alias_edges() {
                if let Some(targets) = files.get(&format!("c:{}", edge.target_fqn)) {
                    minted.push((format!("c:{}", edge.alias_fqn), targets.clone()));
                }
            }
        }
        for (alias, targets) in minted {
            let entry = files.entry(alias).or_default();
            for target in targets {
                if !entry.contains(&target) {
                    entry.push(target);
                }
            }
        }
        Self { files }
    }

    fn files_of(&self, key: &str) -> Option<&Vec<usize>> {
        self.files.get(key)
    }
}

// ---------------------------------------------------------------------------
// The footprint.
// ---------------------------------------------------------------------------

/// Stream every name key file `tree` could resolve against. Over-approximate
/// on purpose: a key that names nothing is a hash lookup that finds nothing.
///
/// Keys are built into one reused buffer rather than formatted per call. The
/// footprint of a large universe runs to millions of keys, and this is the
/// difference between an allocation each and none.
///
/// **No `m:` key is emitted, and none is wanted** (issue #513). A method name
/// is not a delta key — [`PackageShard::contributed_names_from`] carries no
/// method table, by its own documented design — and it is not an edge either,
/// because every method resolution in the analyzer enters through
/// `resolve_in_chain(start_fqn, method)`: the class comes first and the name is
/// looked up *within* it. The merged index holds no by-name method table to
/// resolve against, so no walk can reach a declaration this file does not
/// already reach by naming its class.
///
/// [`PackageShard::contributed_names_from`]: steins_db::PackageShard::contributed_names_from
fn footprint(tree: &SourceTree, sink: &mut dyn FnMut(&str)) {
    let mut buf = String::with_capacity(64);
    let mut emit = |prefix: &str, name: &str, sink: &mut dyn FnMut(&str)| {
        buf.clear();
        buf.push_str(prefix);
        buf.push_str(name);
        sink(&buf);
    };
    // `SourceTree::calls()` is the file-wide **function**-call list: a method,
    // static or constructor call lowers into the statement IR and never reaches
    // here. That is not a gap — see the `m:` note above — and every class such
    // a call names (`new X`, `X::m()`, `X::CONST`, `X::$prop`) is a
    // `hard_class_refs` entry below.
    for call in tree.calls() {
        if let Some(r) = &call.callee_ref {
            for candidate in function_candidates(tree, r) {
                emit("f:", &candidate, sink);
            }
            emit("s:", &r.simple().to_ascii_lowercase(), sink);
        }
        if let Callee::Function(name) = &call.receiver {
            emit("s:", &name.to_ascii_lowercase(), sink);
        }
    }
    for r in tree.hard_class_refs() {
        emit("c:", &tree.resolve_class_fqn(r).to_ascii_lowercase(), sink);
    }
    // A literal `class_alias('target', 'alias')` names both ends in string
    // arguments, which no `NameRef` list carries. The aliasing file depends on
    // both — on the target's declaration, which the alias resolves to, and on
    // the alias name, whose minting the merge may refuse if the target went
    // ambiguous — so both are its footprint and both are edges.
    for edge in tree.class_alias_edges() {
        emit("c:", &edge.alias_fqn, sink);
        emit("c:", &edge.target_fqn, sink);
    }
    for r in tree.const_refs() {
        for candidate in const_candidates(tree, r) {
            emit("k:", &candidate, sink);
        }
    }
    // The docblock leg (see the module docs): a class named only in a comment is
    // invisible to `hard_class_refs` and still reachable by a descent. What is
    // read is the comment's **type expressions**, as the phpdoc scanners
    // themselves delimit them ([`docblock_type_texts`]) — a docblock name is
    // written against the file's own namespace and imports exactly like a code
    // reference, so the bare spelling, the trailing segment and the
    // namespace-qualified form all go in.
    let mut qualified = String::with_capacity(64);
    for comment in tree.comments() {
        let types = docblock_type_texts(&comment.text);
        if types.is_empty() {
            continue;
        }
        let ns = tree.ctx_at(comment.span.start).namespace.to_ascii_lowercase();
        for token in types.iter().flat_map(|text| identifiers(text)) {
            let lower = token.trim_start_matches('\\').to_ascii_lowercase();
            if lower.is_empty() {
                continue;
            }
            for prefix in ["c:", "f:", "s:"] {
                emit(prefix, &lower, sink);
            }
            emit("k:", &normalize_const_fqn(token), sink);
            if !ns.is_empty() {
                qualified.clear();
                qualified.push_str(&ns);
                qualified.push('\\');
                qualified.push_str(&lower);
                emit("c:", &qualified, sink);
                emit("f:", &qualified, sink);
                qualified.clear();
                qualified.push_str(&ns);
                qualified.push('\\');
                qualified.push_str(token);
                emit("k:", &normalize_const_fqn(&qualified), sink);
            }
            if let Some(pos) = lower.rfind('\\') {
                let tail = lower[pos + 1..].to_owned();
                emit("c:", &tail, sink);
                emit("s:", &tail, sink);
            }
        }
    }
}

/// Every stretch of a comment the phpdoc scanners read as a **type
/// expression**: each tag's type text (the text [`steins_phpdoc::parse_type`]
/// is handed), the `@template` bounds, the `@extends`/`@implements`/`@use` type
/// arguments, and the magic-member subjects — which is where `@mixin`'s target
/// lives.
///
/// That is the whole of a docblock any name resolution in the analysis ever
/// sees, which is why it is the whole of what the footprint reads. The
/// tokenizer it replaces ran over every identifier of every comment, and
/// measurement retired it (issue #513): on `nikic/PHP-Parser` the word `param`
/// in 164 files' `@param` tags, and `gettype` in one comment against the 173
/// files declaring a `getType`, edged most of the universe to most of the
/// universe. Restricted to type expressions, the same edit's affected set falls
/// from 337 of 341 files to 20.
fn docblock_type_texts(text: &str) -> Vec<String> {
    if !text.contains('@') {
        return Vec::new();
    }
    let mut out: Vec<String> = steins_phpdoc::scan_docblock(text)
        .into_iter()
        .map(|tag| tag.type_text)
        .filter(|text| !text.is_empty())
        .collect();
    out.extend(steins_phpdoc::scan_template_decls(text).into_iter().filter_map(|d| d.bound));
    out.extend(steins_phpdoc::scan_inheritance_args(text));
    out.extend(
        steins_phpdoc::scan_magic_member_tags(text)
            .into_iter()
            .map(|tag| tag.subject)
            .filter(|subject| !subject.is_empty()),
    );
    out
}

/// Every FQN an unqualified/qualified/fully-qualified **function** reference
/// could denote, lowercased — the index's key space. Mirrors
/// `project::resolves_to_user_function`'s resolution shape, but emits every
/// candidate instead of stopping at the first that resolves: a definition
/// appearing at *either* is a resolution that moved.
fn function_candidates(tree: &SourceTree, r: &NameRef) -> Vec<String> {
    match r.kind {
        RefKind::FullyQualified => vec![r.raw.to_ascii_lowercase()],
        RefKind::Qualified => {
            let ctx = tree.ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            vec![fqn.to_ascii_lowercase()]
        }
        RefKind::Unqualified => {
            let ctx = tree.ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            if let Some(t) = ctx.fn_imports.get(&name) {
                return vec![t.to_ascii_lowercase()];
            }
            // The namespace candidate PHP tries first, and the global fallback
            // it falls back to — both, always.
            let mut out = vec![name.clone()];
            if !ctx.namespace.is_empty() {
                out.push(format!("{}\\{name}", ctx.namespace.to_ascii_lowercase()));
            }
            out
        }
        // ADR-0049 A8: `namespace\name` — the enclosing-namespace candidate only.
        RefKind::Relative => {
            let ctx = tree.ctx_at(r.offset);
            let fqn = if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            vec![fqn.to_ascii_lowercase()]
        }
    }
}

/// Every normalized key a bare constant fetch could denote — the same shape
/// `absence::undefined_constant_target` resolves, every candidate emitted.
/// Constants are case-sensitive past the namespace, which is exactly what
/// [`normalize_const_fqn`] encodes.
fn const_candidates(tree: &SourceTree, r: &NameRef) -> Vec<String> {
    match r.kind {
        RefKind::FullyQualified => vec![normalize_const_fqn(&r.raw)],
        RefKind::Qualified => {
            let ctx = tree.ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            vec![normalize_const_fqn(&fqn)]
        }
        RefKind::Relative => {
            let ctx = tree.ctx_at(r.offset);
            let fqn = if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            vec![normalize_const_fqn(&fqn)]
        }
        RefKind::Unqualified => {
            let ctx = tree.ctx_at(r.offset);
            if let Some(t) = ctx.const_imports.get(&r.raw) {
                return vec![normalize_const_fqn(t)];
            }
            let mut out = vec![normalize_const_fqn(&r.raw)];
            if !ctx.namespace.is_empty() {
                out.push(normalize_const_fqn(&format!("{}\\{}", ctx.namespace, r.raw)));
            }
            out
        }
    }
}

/// The identifier-ish tokens of a comment: maximal runs of PHP name characters
/// (`[A-Za-z0-9_\x80-\xff\\]`), skipping anything that is all digits or empty.
/// A tokenizer, not a docblock parser — the point is to miss no name, and a
/// non-name costs one lookup.
fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\\' || !c.is_ascii()))
        .filter(|t| !t.is_empty() && !t.chars().all(|c| c.is_ascii_digit()))
}

/// The files a file's class-likes inherit from, in the widest reading the
/// lowering supports: `extends`, `implements`, `@mixin` targets, the
/// **anonymous** classes' edges, and — for a class that uses traits, whose
/// trait names the lowering keeps only as a bit — every class-like this file
/// names at all.
///
/// Anonymous classes matter and are easy to miss: they enter no index and
/// declare no name, so nothing can ever have a *call* edge to the file holding
/// one, yet the declared-receiver lane's descendant closure (ADR-0049 A4) reads
/// them precisely because an invisible descendant of a union member would
/// otherwise be missed. Adding a `new class extends Report {}` in a file that
/// declares nothing else would, without this leg, leave every reasoner about
/// `Report`'s descendants replaying a stale answer.
fn inheritance_edges(tree: &SourceTree, decls: &DeclTable, file: usize) -> Vec<usize> {
    let mut out: HashSet<usize> = HashSet::new();
    let mut add = |fqn: &str| {
        if let Some(files) = decls.files_of(&format!("c:{fqn}")) {
            out.extend(files.iter().copied());
        }
    };
    let mut any_trait_user = false;
    for c in tree.classes() {
        if let Some(parent) = &c.parent {
            add(&tree.resolve_class_fqn(parent).to_ascii_lowercase());
        }
        for r in &c.implements {
            add(&tree.resolve_class_fqn(r).to_ascii_lowercase());
        }
        if let Some(doc) = &c.docblock {
            for target in mixin_targets(doc) {
                add(&target.to_ascii_lowercase());
                // A `@mixin` may be written unqualified against the file's
                // imports; the trailing segment covers that without lowering
                // the docblock.
                if let Some(pos) = target.rfind('\\') {
                    add(&target[pos + 1..].to_ascii_lowercase());
                }
            }
        }
        any_trait_user |= c.uses_traits;
    }
    for edge in tree.anonymous_class_edges() {
        if let Some(parent) = &edge.parent {
            add(&tree.resolve_class_fqn(parent).to_ascii_lowercase());
        }
        for r in &edge.implements {
            add(&tree.resolve_class_fqn(r).to_ascii_lowercase());
        }
    }
    if any_trait_user {
        // `ClassDecl` records only *that* traits are used, so the trait names
        // are approximated by every class-like reference the file makes —
        // `hard_class_refs` includes `use <Trait>` positions.
        for r in tree.hard_class_refs() {
            add(&tree.resolve_class_fqn(r).to_ascii_lowercase());
        }
    }
    out.remove(&file);
    out.into_iter().collect()
}

/// The `@mixin` targets a class docblock names (ADR-0049 A14's own tag), as
/// written. A one-tag scan, not a docblock parse.
fn mixin_targets(doc: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (i, _) in doc.match_indices("@mixin") {
        let rest = &doc[i + "@mixin".len()..];
        if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            continue; // `@mixinFoo` is a different tag, not this one.
        }
        if let Some(token) = identifiers(rest).next() {
            out.push(token);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Graph closure.
// ---------------------------------------------------------------------------

/// The reverse of an adjacency list.
fn reverse(edges: &[Vec<usize>], n: usize) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (from, tos) in edges.iter().enumerate() {
        for &to in tos {
            out[to].push(from);
        }
    }
    out
}

/// Breadth-first closure of `seeds` over `edges`, at most `depth` hops.
/// `usize::MAX` is the unbounded reading.
fn closure(edges: &[Vec<usize>], seeds: &HashSet<usize>, depth: usize) -> HashSet<usize> {
    let mut seen: HashSet<usize> = seeds.clone();
    let mut frontier: Vec<usize> = seeds.iter().copied().collect();
    let mut hops = 0usize;
    while !frontier.is_empty() && hops < depth {
        let mut next = Vec::new();
        for node in frontier {
            for &neighbour in &edges[node] {
                if seen.insert(neighbour) {
                    next.push(neighbour);
                }
            }
        }
        frontier = next;
        hops += 1;
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trees(sources: &[&str]) -> Vec<SourceTree> {
        sources.iter().map(|s| SourceTree::parse(s)).collect()
    }

    fn affected(trees: &[SourceTree], changed: &[usize], delta: &[&str]) -> Vec<usize> {
        let inputs = AffectedInputs {
            trees,
            changed: changed.iter().copied().collect(),
            delta: delta.iter().map(|s| (*s).to_owned()).collect(),
        };
        let mut out: Vec<usize> = affected_files(&inputs).into_iter().collect();
        out.sort_unstable();
        out
    }

    /// A changed file is affected, and so is anything that calls into it — but
    /// a file that neither changed nor reaches it replays.
    #[test]
    fn the_call_graph_leg_reaches_callers_and_stops() {
        let t = trees(&[
            "<?php function leaf(): int { return 1; }\n",
            "<?php function mid(): int { return leaf(); }\n",
            "<?php function top(): int { return mid(); }\n",
            "<?php function alone(): int { return 7; }\n",
        ]);
        assert_eq!(affected(&t, &[0], &[]), vec![0, 1, 2]);
        assert_eq!(affected(&t, &[3], &[]), vec![3]);
    }

    /// The depth bound is the descent's: a chain longer than
    /// `MAX_BINDING_DEPTH` stops being pulled in, which is exactly where the
    /// descent stops looking.
    #[test]
    fn the_call_graph_closure_stops_at_the_binding_depth() {
        let mut sources: Vec<String> =
            vec!["<?php function f0(): int { return 1; }\n".to_owned()];
        for i in 1..=MAX_BINDING_DEPTH + 2 {
            sources.push(format!("<?php function f{i}(): int {{ return f{}(); }}\n", i - 1));
        }
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        let t = trees(&refs);
        let out = affected(&t, &[0], &[]);
        assert_eq!(out.len(), MAX_BINDING_DEPTH + 1, "{out:?}");
        assert!(!out.contains(&(MAX_BINDING_DEPTH + 1)));
    }

    /// The delta leg: an untouched file whose footprint names a delta key is
    /// affected even with no call edge to anything changed — this is the leg
    /// that catches an absence finding moving under a symbol addition.
    #[test]
    fn the_delta_leg_catches_a_name_whose_resolution_could_move() {
        let t = trees(&[
            "<?php namespace App; function user(): int { return helper(); }\n",
            "<?php function unrelated(): int { return 1; }\n",
        ]);
        // `helper()` in namespace App tries `helper` then `app\helper`; a
        // definition appearing at either moves the answer.
        assert_eq!(affected(&t, &[], &["f:helper"]), vec![0]);
        assert_eq!(affected(&t, &[], &["f:app\\helper"]), vec![0]);
        assert_eq!(affected(&t, &[], &["f:something-else"]), Vec::<usize>::new());
    }

    /// Class, constant and method keys all take part, and a `new` reaches the
    /// constructor's class.
    #[test]
    fn the_delta_leg_covers_classes_constants_and_methods() {
        let t = trees(&[
            "<?php $w = new \\Acme\\Widget(); $w->render(); echo \\Acme\\LIMIT;\n",
        ]);
        assert_eq!(affected(&t, &[], &["c:acme\\widget"]), vec![0]);
        assert_eq!(affected(&t, &[], &["k:acme\\LIMIT"]), vec![0]);
    }

    /// The docblock leg: a class named only in a `@return` still puts an edge
    /// on the file that names it, so a caller two hops away is pulled in.
    #[test]
    fn a_docblock_only_class_reference_still_makes_an_edge() {
        let t = trees(&[
            "<?php namespace Acme; class Widget { public int $n = 1; }\n",
            "<?php namespace Acme;\n/** @return Widget */\nfunction make() { return new Widget(); }\n",
            "<?php namespace Acme; function top() { return make()->n; }\n",
        ]);
        // File 2 reaches file 0 through file 1's docblock-named return.
        assert_eq!(affected(&t, &[0], &[]), vec![0, 1, 2]);
    }

    /// Inheritance closes without a bound and in both directions: a changed
    /// base pulls in its subclasses, and a changed subclass pulls in files that
    /// enumerate the base's descendants.
    #[test]
    fn inheritance_closes_unbounded_and_both_ways() {
        let mut sources = vec!["<?php class B0 {}\n".to_owned()];
        for i in 1..=MAX_BINDING_DEPTH + 3 {
            sources.push(format!("<?php class B{i} extends B{} {{}}\n", i - 1));
        }
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        let t = trees(&refs);
        let deepest = MAX_BINDING_DEPTH + 3;
        let from_root = affected(&t, &[0], &[]);
        assert!(from_root.contains(&deepest), "a changed base reaches past the depth bound");
        let from_leaf = affected(&t, &[deepest], &[]);
        assert!(from_leaf.contains(&0), "a changed subclass reaches its base");
    }

    /// A trait-using class widens to every class-like the file names, since the
    /// lowering keeps only the bit — the over-approximation the module docs
    /// declare, pinned so a later narrowing is a deliberate change.
    #[test]
    fn a_trait_user_widens_to_every_class_it_names() {
        let t = trees(&[
            "<?php trait T { public function t(): int { return 1; } }\n",
            "<?php class C { use T; }\n",
        ]);
        assert_eq!(affected(&t, &[0], &[]), vec![0, 1]);
    }

    /// Nothing changed and nothing in the delta: nothing is affected, which is
    /// the no-change warm run's whole point.
    #[test]
    fn an_unchanged_universe_affects_nothing() {
        let t = trees(&[
            "<?php namespace App; class A { public function go(): int { return \\App\\help(); } }\n",
            "<?php namespace App; function help(): int { return 1; }\n",
        ]);
        assert_eq!(affected(&t, &[], &[]), Vec::<usize>::new());
    }

    /// An anonymous class declares no name, so no call edge can ever reach the
    /// file holding one — and the declared-receiver lane reads it anyway, as an
    /// invisible descendant. The inheritance leg is what connects it.
    #[test]
    fn an_anonymous_subclass_is_an_inheritance_edge() {
        let t = trees(&[
            "<?php\nnamespace App;\nclass Report {}\n",
            "<?php\nnamespace App;\n$r = new class extends Report {};\n",
            "<?php\nnamespace App;\nfunction consume(Report $r): int { return 1; }\n",
        ]);
        // The file holding the anon subclass changed: everything reasoning
        // about `Report`'s descendants must be walked.
        let out = affected(&t, &[1], &[]);
        assert!(out.contains(&0), "the parent's file: {out:?}");
        assert!(out.contains(&2), "a file naming the parent: {out:?}");
    }

    /// A literal `class_alias` mints an index edge rather than damming (issue
    /// #36), so a file that names only the alias reads a class body in the
    /// *target's* file and must be walked when that file changes. Neither end
    /// is a `NameRef`, so both legs are alias-specific.
    #[test]
    fn a_class_alias_reaches_the_target_declarations_file() {
        let t = trees(&[
            "<?php\nnamespace Lib;\nclass Real { public int $n = 1; }\n",
            "<?php\nclass_alias('lib\\\\real', 'shortcut');\n",
            "<?php\nfunction use_it(\\Shortcut $s): int { return $s->n; }\n",
        ]);
        // File 2 names only the alias; file 0 declares the body it reads.
        assert_eq!(affected(&t, &[0], &[]), vec![0, 1, 2]);
        // And the delta leg reaches the aliasing file through either end.
        assert_eq!(affected(&t, &[], &["c:lib\\real"]), vec![1]);
        assert_eq!(affected(&t, &[], &["c:shortcut"]), vec![1, 2]);
    }

    /// The receiver shape this leg cannot resolve, pinned so a later tightening
    /// cannot drop it (issue #513). `$w`'s class is named in *neither* the
    /// calling file nor its docblocks — it arrives through `make()`'s return
    /// type — and the walk still resolves `$w->render()` into `Widget`'s file.
    /// The chain that keeps it sound is the descent one: the caller edges to
    /// `make()`'s file, which names `Widget`, so the closure reaches it at the
    /// second hop rather than the first.
    #[test]
    fn an_untyped_receiver_still_reaches_the_class_its_walk_resolves() {
        let t = trees(&[
            "<?php final class Widget { public function render(): int { return 1; } }\n",
            "<?php function make(): Widget { return new Widget(); }\n",
            "<?php function top(): int { $w = make(); return $w->render(); }\n",
        ]);
        assert_eq!(affected(&t, &[0], &[]), vec![0, 1, 2]);
    }

    /// A named receiver resolves *upwards*: `Impl::run()` runs `Base`'s body,
    /// and the file naming `Impl` names neither `Base` nor `run`. The subtype
    /// expansion inside each descent hop is what carries it — and it costs no
    /// hop, so a two-deep call chain through two such receivers still lands
    /// inside the budget.
    #[test]
    fn a_named_receiver_reaches_the_ancestor_that_declares_the_body() {
        let t = trees(&[
            "<?php class Base { public static function run(): int { return 1; } }\n",
            "<?php class Impl extends Base {}\n",
            "<?php class Mid { public static function go(): int { return Impl::run(); } }\n",
            "<?php function top(): int { return Mid::go(); }\n",
        ]);
        let out = affected(&t, &[0], &[]);
        assert!(out.contains(&2), "the file naming the subclass: {out:?}");
        assert!(out.contains(&3), "and its own caller: {out:?}");
    }

    /// A subclass appearing or moving reaches every file that *names the
    /// superclass*, because the declared-receiver lane enumerates the whole
    /// descendant set — the seed leg the call graph cannot answer for.
    #[test]
    fn a_changed_subclass_reaches_the_files_naming_its_supertype() {
        let t = trees(&[
            "<?php namespace App; class Report {}\n",
            "<?php namespace App; class Detailed extends Report {}\n",
            "<?php namespace App; function consume(Report $r): int { return 1; }\n",
        ]);
        let out = affected(&t, &[1], &[]);
        assert!(out.contains(&0), "the supertype's file: {out:?}");
        assert!(out.contains(&2), "a file naming the supertype: {out:?}");
    }

    /// A name a comment merely *says* is not an edge. Method resolution is
    /// class-first, so nothing can arrive at `Report::render` without naming
    /// `Report`; the tokenizer this replaces edged every file whose prose
    /// spelled `render` to every file declaring one (issue #513).
    #[test]
    fn a_name_in_comment_prose_is_not_an_edge() {
        let t = trees(&[
            "<?php class Report { public function render(): int { return 1; } }\n",
            "<?php\n// Report: this helper renders nothing.\n/** A summary about Report. */\nfunction show(): int { return 1; }\n",
            "<?php\n/** @return int Renders by calling render twice. */\nfunction count_it(): int { return 1; }\n",
        ]);
        assert_eq!(affected(&t, &[0], &[]), vec![0]);
        // …and the delta leg does not fire on prose either.
        assert_eq!(affected(&t, &[], &["c:report"]), Vec::<usize>::new());
    }

    /// A **removed** declaration is the one delta shape the ordinary closure
    /// cannot answer for: a deleted file has no slot to be `changed`, so the
    /// file naming it reaches nothing, and a caller descending into that file
    /// sees the moved answer with no key of its own. Such a hit seeds the
    /// descent closure — and a hit whose key still resolves to a changed file
    /// does not, because the edge to that file already carries its callers.
    #[test]
    fn a_removed_declaration_seeds_the_descent_closure() {
        let t = trees(&[
            "<?php function user(): int { return helper(); }\n",
            "<?php function top(): int { return user(); }\n",
        ]);
        // Nothing in this universe declares `helper` any more: the caller of
        // `user()` must be walked too.
        assert_eq!(affected(&t, &[], &["f:helper"]), vec![0, 1]);
    }

    /// `@mixin` targets are inheritance edges, and `@mixinFoo` is not `@mixin`.
    #[test]
    fn mixin_targets_are_read_as_one_tag() {
        assert_eq!(mixin_targets("/** @mixin Widget */"), vec!["Widget"]);
        assert_eq!(mixin_targets("/** @mixin \\Acme\\Widget */"), vec!["\\Acme\\Widget"]);
        assert!(mixin_targets("/** @mixinFoo Widget */").is_empty());
        assert_eq!(mixin_targets("/** @mixin A\n * @mixin B */"), vec!["A", "B"]);
    }
}
