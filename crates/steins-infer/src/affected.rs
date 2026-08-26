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
//!   message. Any byte moving in a file makes it changed.
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
//! ## The footprint
//!
//! `footprint(F)` is a projection of F's loaded trace IR — never a walk, and no
//! new persistence: call and method-call names, `hard_class_refs`, `const_refs`,
//! and the identifier tokens of F's comments. Names are normalized the way the
//! index keys them, and every spelling a resolution *could* take is emitted,
//! not just the one it takes today: an unqualified call in a namespace tries
//! `Ns\f` and then global `f`, and both candidates go in, because a definition
//! appearing at either is a resolution that moved.
//!
//! The comment leg is the one the pinned list does not name, and it is load
//! bearing. `hard_class_refs` excludes docblock positions by design (it is the
//! `class.undefined` firing set, and a docblock name is not a hard error), so
//! without it a file whose only mention of a class is `@return Widget` has no
//! edge to Widget's file — and a caller two hops away, descending through that
//! return, would replay a stale finding. Tokenizing comments over-approximates
//! wildly and that is fine: a token that names nothing resolves to no file and
//! is in no delta, so it costs a hash lookup and nothing else.
//!
//! ## The call graph
//!
//! File-level, and derived from *resolved call sites* rather than from the
//! effect/throw own-row edges, which answer a different question. Being coarser
//! than the descent's real reach is sound; being finer is not — so a method
//! call contributes an edge to **every** file declaring a method of that name,
//! since which class the receiver resolves to is a walk's answer and not
//! available here. The closure is the backwards reachability from the changed
//! files, bounded by [`MAX_BINDING_DEPTH`], which is the bound the descent
//! itself stops at.
//!
//! With the delta file-granular (issue #510) **this is now the widest leg by
//! far**, and it saturates: editing `nikic/PHP-Parser`'s `Lexer.php` puts 17
//! files in the delta leg and 337 of 341 in the affected set, because eight
//! hops of "every file declaring a method of this name" reach a whole
//! codebase through its ordinary method names. An edit whose file declares
//! nothing widely named costs 4. Tightening this — resolved receivers, a
//! smaller bound, a directional reading — is the closure work issue #489's
//! design pin left out of scope until a measurement asked for it. This is that
//! measurement; it is not this issue's change.
//!
//! ## Inheritance
//!
//! One refinement the pinned leg list does not spell, added because the
//! `MAX_BINDING_DEPTH` bound does not hold for it. Class-chain traversal is not
//! depth-bounded in the analysis — `magic_obstacles_in_reach` follows parents,
//! interfaces and `@mixin` targets transitively, and the declared-receiver
//! lane's descendant closure walks the *other* way — so an inheritance edge is
//! closed **without a bound and in both directions**. What it costs is a class
//! hierarchy's whole connected component per edited class file — the price of
//! an unbounded traversal priced honestly, and on `nikic/PHP-Parser` one extra
//! file per edit next to the call graph's hundreds. Narrowing it means
//! bounding the analysis's own chain walks first, not the closure over them.
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

use steins_syntax::{Callee, NameRef, RefKind, SourceTree, StaticClass, normalize_const_fqn};

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
    // its outgoing call-graph edges, so the keys are never retained.
    let mut affected: HashSet<usize> = inputs.changed.clone();
    let mut callees: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut inherits: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (file, tree) in inputs.trees.iter().enumerate() {
        let mut edges: HashSet<usize> = HashSet::new();
        let mut hit = false;
        footprint(tree, &mut |key: &str| {
            if inputs.delta.contains(key) {
                hit = true;
            }
            if let Some(files) = decls.files_of(key) {
                edges.extend(files.iter().copied());
            }
        });
        if hit {
            affected.insert(file);
        }
        edges.remove(&file);
        callees[file] = edges.into_iter().collect();
        inherits[file] = inheritance_edges(tree, &decls, file);
    }

    // Inheritance first, unbounded and undirected: a changed superclass moves
    // its subclasses' answers, and a changed subclass moves what a declared
    // supertype's descendant closure enumerates.
    let seeds = closure(&undirected(&inherits, n), &inputs.changed, usize::MAX);
    affected.extend(seeds.iter().copied());

    // Then the call graph, backwards from those seeds, bounded by the depth
    // the binding descent itself stops at.
    let reverse = reverse(&callees, n);
    affected.extend(closure(&reverse, &seeds, MAX_BINDING_DEPTH));
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
                for m in &c.methods {
                    add(format!("m:{}", m.name.to_ascii_lowercase()), file);
                }
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
/// footprint of a large universe runs to tens of millions of keys, and this is
/// the difference between an allocation each and none.
fn footprint(tree: &SourceTree, sink: &mut dyn FnMut(&str)) {
    let mut buf = String::with_capacity(64);
    let mut emit = |prefix: &str, name: &str, sink: &mut dyn FnMut(&str)| {
        buf.clear();
        buf.push_str(prefix);
        buf.push_str(name);
        sink(&buf);
    };
    for call in tree.calls() {
        if let Some(r) = &call.callee_ref {
            for candidate in function_candidates(tree, r) {
                emit("f:", &candidate, sink);
            }
            emit("s:", &r.simple().to_ascii_lowercase(), sink);
        }
        match &call.receiver {
            Callee::Function(name) => emit("s:", &name.to_ascii_lowercase(), sink),
            Callee::Method { method, .. } => emit("m:", &method.to_ascii_lowercase(), sink),
            Callee::Static { class, method } => {
                emit("m:", &method.to_ascii_lowercase(), sink);
                if let StaticClass::Named(r) = class {
                    emit("c:", &tree.resolve_class_fqn(r).to_ascii_lowercase(), sink);
                }
            }
            Callee::Construct { class } => {
                emit("c:", &tree.resolve_class_fqn(class).to_ascii_lowercase(), sink);
                // The constructor is a method like any other; a class that
                // gains or loses one moves an arity answer.
                emit("m:", "__construct", sink);
            }
            Callee::DynamicVar(_) | Callee::Dynamic => {}
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
    // The docblock leg (see the module docs): a class named only in a comment
    // is invisible to `hard_class_refs` and still reachable by a descent. A
    // docblock name is written against the file's own namespace and imports
    // exactly like a code reference, so the bare spelling, the trailing segment
    // and the namespace-qualified form all go in — resolving one properly would
    // mean lowering every docblock here, for a key that costs one lookup.
    let mut qualified = String::with_capacity(64);
    for comment in tree.comments() {
        let ns = tree.ctx_at(comment.span.start).namespace.to_ascii_lowercase();
        for token in identifiers(&comment.text) {
            let lower = token.trim_start_matches('\\').to_ascii_lowercase();
            if lower.is_empty() {
                continue;
            }
            for prefix in ["c:", "f:", "s:", "m:"] {
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
                emit("m:", &tail, sink);
            }
        }
    }
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

/// An adjacency list plus its reverse, merged — the undirected reading.
fn undirected(edges: &[Vec<usize>], n: usize) -> Vec<Vec<usize>> {
    let mut out = reverse(edges, n);
    for (from, tos) in edges.iter().enumerate() {
        out[from].extend(tos.iter().copied());
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

    /// `@mixin` targets are inheritance edges, and `@mixinFoo` is not `@mixin`.
    #[test]
    fn mixin_targets_are_read_as_one_tag() {
        assert_eq!(mixin_targets("/** @mixin Widget */"), vec!["Widget"]);
        assert_eq!(mixin_targets("/** @mixin \\Acme\\Widget */"), vec!["\\Acme\\Widget"]);
        assert!(mixin_targets("/** @mixinFoo Widget */").is_empty());
        assert_eq!(mixin_targets("/** @mixin A\n * @mixin B */"), vec!["A", "B"]);
    }
}
