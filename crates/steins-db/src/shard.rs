//! The per-package symbol shard and the per-generation merge (ADR-0092 §3,
//! issue #486).
//!
//! PHP's symbol space forces one deliberate deviation from the package model:
//! autoloading is not a module system, so a symbol added in one package can
//! render a name in another package ambiguous, and `class_alias` edges cross
//! packages freely. Therefore shards are per package and **every global table
//! is recomputed per generation from the shards**: the merged symbol maps, the
//! cross-package ambiguity sets, the literal `class_alias` fold (kept
//! order-independent), and the tables `steins_infer::project::Index` carries.
//!
//! [`PackageShard`] is the per-package half of the whole-project index —
//! everything one package's files contribute, keyed by nothing outside the
//! package except the universe file slot. [`merge_shards`] recomputes the
//! global [`MergedTables`], and its result is **independent of shard order**
//! (ADR-0048 §4): ambiguity is multiset arithmetic, `fn_by_simple` sorts by
//! site, alias edges resolve against the merged textual snapshot before any
//! alias is minted, and the per-class obstacle lists are ordered by slot.
//!
//! Both existing constructors delegate here — [`crate::project_index`] and
//! `steins_infer::project::Index` build shards and merge them, so this module
//! is the one implementation under the previously duplicated symbol tables.
//! The grouping they use ([`fallback_package_key`]) is a path heuristic, and
//! deliberately so: the merge is partition-invariant (any grouping of the
//! same files merges to the same tables — the differential-oracle tests pin
//! this), so the heuristic only decides shard boundaries, never meaning.
//! The `persist` feature (issue #487) serializes shards into the `symbols`
//! section of a package artifact — see the `persist` module.

use std::collections::{HashMap, HashSet};

use steins_gen::Package;
use steins_phpdoc::{MagicTagKind, scan_magic_member_tags};
use steins_syntax::{ClassDecl, NameRef, RefKind, SourceTree};

/// A declaration's position in the analyzed universe: the owning file's slot
/// (its index in the run's file list — `Project::files` order on the salsa
/// path, `FileUnit` slice order on the units path; the two are identical by
/// construction) and the declaration's index in that file's decl list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ShardSite {
    pub file: usize,
    pub index: usize,
}

/// One recorded **silence obstacle** a class-like's docblock declares (ADR-0049
/// A14, issue #195): a `@method` / `@property*` / `@mixin` / `@phpstan-type` tag
/// says members exist somewhere the index cannot enumerate, so every absence
/// proof over that class-like is silent.
///
/// The shape is normative: a record is `(class-like, obstacle kind, subject)` —
/// **never** a class-level "has magic somewhere" boolean. A plugin pack that
/// declares what the magic actually provides (ADR-0039) must be able to discharge
/// the obstacle member by member and re-enable the absence proof for the
/// undeclared remainder, which a boolean forecloses. The discharge channel itself
/// is not built here; only the record granularity that lets it be.
///
/// Lives beside the shard (issue #486) because the obstacle table is one of the
/// per-package tables the generation merge recomputes; `steins-infer` re-exports
/// it unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct MagicObstacle {
    /// The declaring class-like's own FQN, lowercase-normalized
    /// ([`ClassDecl::fqn`]) — the class-like half of the A14 triple.
    pub class: String,
    /// Which tag recorded it. Persisted by its canonical `label()` spelling —
    /// the codec lives here, not in `steins-phpdoc`, so the zero-dep parser
    /// crate stays dependency-free; an unknown spelling is a decode error.
    #[cfg_attr(feature = "persist", serde(with = "crate::persist::magic_tag_kind"))]
    pub kind: MagicTagKind,
    /// The tag's subject as written: the method name, the property name (no `$`),
    /// the mixin target reference, the alias name. Empty when the tag's tail gave
    /// none — presence alone still obstructs.
    pub subject: String,
    /// For [`MagicTagKind::Mixin`] only: [`Self::subject`] resolved to a project
    /// FQN against the declaring file's namespace and `use` imports, so the reach
    /// walk can follow it. A target that resolves to nothing is not a finding and
    /// not an error — the `@mixin` record itself already obstructs.
    pub mixin_target: Option<String>,
}

/// The per-package half of the whole-project index (ADR-0092 §3): everything
/// the package's own files contribute, before any cross-package question is
/// asked. Built incrementally by [`PackageShard::add_file`]; global answers
/// exist only on the [`MergedTables`] side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct PackageShard {
    /// Function FQN (lowercase-normalized by the syntax layer) → its site,
    /// for FQNs this package defines exactly once.
    functions: HashMap<String, ShardSite>,
    /// Function FQNs this package defines more than once — the per-package
    /// ambiguity candidates; the merge unions them into the global set.
    ambiguous_functions: HashSet<String>,
    /// Class FQN → site, unique within the package.
    classes: HashMap<String, ShardSite>,
    /// Class FQNs defined more than once within the package.
    ambiguous_classes: HashSet<String>,
    /// Lowercased simple function name → every definition site in the package.
    fn_by_simple: HashMap<String, Vec<ShardSite>>,
    /// Literal `class_alias` edges as written, in file order. Deliberately
    /// **unresolved** — an alias target may live in another package, so
    /// resolution belongs to the merge, against the merged textual snapshot
    /// (ADR-0049 §2).
    class_alias_edges: Vec<ShardAlias>,
    /// The A14 obstacle records with their declaring file's slot, in scan
    /// order. The slot is what lets the merge reproduce the whole-universe
    /// scan order for classes sharing one lowercase FQN across packages.
    magic_obstacles: Vec<(usize, MagicObstacle)>,
    /// Property names written in the package, and whether any write went
    /// through a computed name (ADR-0078, issue #197).
    property_writes: (HashSet<String>, bool),
    /// Global constants the package declares (ADR-0078, issue #198), keyed by
    /// `steins_syntax::normalize_const_fqn`'s spelling, each with the slot of
    /// a file that declares it.
    ///
    /// One slot, not a list, and that is sound: the merged answer for a
    /// constant is set membership, so a name two files declare cannot move by
    /// one of them changing — and a name only one file declares carries that
    /// file's slot. Which of two declaring files wins is unobservable.
    constants: HashMap<String, usize>,
    /// Diagnostic path → file slot for the package's files (issue #497).
    files: HashMap<String, usize>,
}

impl PackageShard {
    /// Fold one file into the shard. `slot` is the file's universe slot; the
    /// resulting tables are independent of the order files are added in (the
    /// merge sorts wherever order is observable).
    pub fn add_file(&mut self, slot: usize, path: &str, tree: &SourceTree) {
        for (i, f) in tree.functions().iter().enumerate() {
            let site = ShardSite { file: slot, index: i };
            self.fn_by_simple.entry(f.name.to_ascii_lowercase()).or_default().push(site);
            insert_unique(&mut self.functions, &mut self.ambiguous_functions, &f.fqn, site);
        }
        for (i, c) in tree.classes().iter().enumerate() {
            let site = ShardSite { file: slot, index: i };
            insert_unique(&mut self.classes, &mut self.ambiguous_classes, &c.fqn, site);
        }
        for edge in tree.class_alias_edges() {
            self.class_alias_edges.push(ShardAlias {
                alias_fqn: edge.alias_fqn.clone(),
                target_fqn: edge.target_fqn.clone(),
                file: slot,
            });
        }
        let mut buf: Vec<MagicObstacle> = Vec::new();
        for cd in tree.classes() {
            class_magic_obstacles(tree, cd, &mut buf);
            self.magic_obstacles.extend(buf.drain(..).map(|o| (slot, o)));
        }
        self.property_writes.0.extend(tree.property_write_names().iter().cloned());
        self.property_writes.1 |= tree.writes_computed_property_name();
        self.constants.extend(tree.global_const_decls().iter().map(|d| (d.fqn.clone(), slot)));
        let entry = self.files.entry(path.to_owned()).or_insert(slot);
        if slot > *entry {
            *entry = slot;
        }
    }

    /// Fold a shard built from **one file** into this one, re-slotting every
    /// site to `slot` (issue #516).
    ///
    /// The one-file shard is the persisted form of [`Self::add_file`]: written
    /// at the canonical slot 0 so its bytes are a function of the file alone,
    /// and folded back in here under whatever slot the file holds now. That is
    /// what lets a warm run rebuild a changed package's shard without decoding
    /// the trees of the files that did not change — the reason `add_file` was
    /// the last whole-universe tree consumer left in the load phase.
    ///
    /// Equal to `add_file` by construction, not by comparison: every table is
    /// folded through the same `insert_unique` (so a name the *package*
    /// declares twice demotes exactly as it would have), and the ones with no
    /// per-file structure are unioned. `absorbing_a_file_shard_equals_adding_the_file`
    /// pins it over a universe that exercises every table.
    ///
    /// # Panics
    ///
    /// Never: `one`'s sites are read, not indexed.
    pub fn absorb_file(&mut self, one: &Self, slot: usize) {
        let at = |site: &ShardSite| ShardSite { file: slot, index: site.index };
        // `fn_by_simple` keeps every site; the merge sorts it, so the order
        // this pushes in is not observable.
        for (simple, sites) in &one.fn_by_simple {
            self.fn_by_simple.entry(simple.clone()).or_default().extend(sites.iter().map(at));
        }
        for (fqn, site) in &one.functions {
            insert_unique(&mut self.functions, &mut self.ambiguous_functions, fqn, at(site));
        }
        // A name the *file* already declared twice arrives demoted; demotion is
        // absorbing, so it stays demoted whatever else the package holds.
        for fqn in &one.ambiguous_functions {
            self.functions.remove(fqn);
            self.ambiguous_functions.insert(fqn.clone());
        }
        for (fqn, site) in &one.classes {
            insert_unique(&mut self.classes, &mut self.ambiguous_classes, fqn, at(site));
        }
        for fqn in &one.ambiguous_classes {
            self.classes.remove(fqn);
            self.ambiguous_classes.insert(fqn.clone());
        }
        self.class_alias_edges.extend(one.class_alias_edges.iter().map(|edge| ShardAlias {
            alias_fqn: edge.alias_fqn.clone(),
            target_fqn: edge.target_fqn.clone(),
            file: slot,
        }));
        self.magic_obstacles
            .extend(one.magic_obstacles.iter().map(|(_, o)| (slot, o.clone())));
        self.property_writes.0.extend(one.property_writes.0.iter().cloned());
        self.property_writes.1 |= one.property_writes.1;
        self.constants.extend(one.constants.keys().map(|key| (key.clone(), slot)));
        for path in one.files.keys() {
            let entry = self.files.entry(path.clone()).or_insert(slot);
            if slot > *entry {
                *entry = slot;
            }
        }
    }

    /// Every name this shard contributes to the merged tables, in the key
    /// namespaces a name reference is spelled in (ADR-0092 §5, issue #489
    /// slice B): `f:` a function FQN, `s:` a function's simple name, `c:` a
    /// class-like FQN (declared, or minted by a literal `class_alias`), `k:` a
    /// global constant.
    ///
    /// **What it is for.** The whole-shard reading of the warm path's name
    /// delta — what a package contributes when *every* one of its files must
    /// be presumed moved: a package that vanished between generations, or one
    /// whose per-file identity could not be established. The ordinary case is
    /// [`Self::contributed_names_from`], which asks the same question per file.
    ///
    /// The set is deliberately *contribution*, not resolution: a name this
    /// shard defines twice (and so demotes to ambiguous) is here exactly like
    /// one it defines once, because both sides of an ambiguity move the
    /// merged answer. Alias edges contribute both ends — the alias name,
    /// which the merge may mint, and the target, whose demotion would unmint
    /// it.
    ///
    /// Method names are deliberately absent: a shard carries no method table,
    /// and a method added to or removed from a class moves that *class's*
    /// answer, whose FQN is in the set. The file-level call graph closes the
    /// rest.
    #[must_use]
    pub fn contributed_names(&self) -> Vec<String> {
        self.names(&|_| true)
    }

    /// The names this shard contributes **from the given files** (issue #510):
    /// [`Self::contributed_names`] restricted to the declarations whose
    /// [`ShardSite`] names a slot in `files`, plus — wholesale — the members
    /// that carry no site at all.
    ///
    /// **Why this is the delta the warm path wants.** A name's merged answer
    /// can only move if some *file's* contribution to it moved; if every file
    /// declaring a name is byte-identical to what the published generation
    /// held, the multiset of definitions under that name is unchanged and so
    /// is every answer over it. Package granularity — the whole key set of any
    /// package with one edited file — is sound but useless in the ordinary
    /// shape of a project, where one package holds everything.
    ///
    /// **`files` are this shard's own slots.** Sites are universe slots, so an
    /// *old* shard's sites index the *old* universe: the caller resolves them
    /// through that shard's own [`Self::files`] map and compares paths, never
    /// raw slot numbers across generations.
    ///
    /// **The ambiguity sets ride wholesale**, deliberately: a name this package
    /// declares twice has no site *because* it has two, and the shard drops
    /// both when it demotes. Including a changed package's whole ambiguity set
    /// is what keeps the leg sound; they are empty in a project that compiles,
    /// which is why they are not worth a site each.
    ///
    /// Everything else is sited, including the two members that were not when
    /// issue #510 was filed. Measurement is why: on `nikic/PHP-Parser` the
    /// site-less reading put 22 `class_alias` keys (the 5.x back-compat aliases
    /// one file declares) and 3 constants into *every* edit's delta, and 61 of
    /// the 341 files matched one of them — 18% of the universe walked for an
    /// alias nothing about the edit touched.
    ///
    /// Call this only for a package that changed: with an empty `files` the
    /// answer is the ambiguity sets alone, which is the right answer for a
    /// package whose only change was a *deleted* file (the deletion shows on
    /// the old side) and the wrong question for a package that did not move.
    #[must_use]
    pub fn contributed_names_from(&self, files: &HashSet<usize>) -> Vec<String> {
        self.names(&|slot| files.contains(&slot))
    }

    /// The package's file identity: every diagnostic path it holds, with the
    /// universe slot its [`ShardSite`]s are keyed by. For a shard read back
    /// from an artifact these are the *old* universe's slots, which is what
    /// makes it the map an old-side delta resolves its sites through.
    pub fn files(&self) -> impl Iterator<Item = (&str, usize)> {
        self.files.iter().map(|(path, &slot)| (path.as_str(), slot))
    }

    /// The one implementation under both name queries: every contribution
    /// whose site `keep` admits, plus the site-less ambiguity sets.
    fn names(&self, keep: &dyn Fn(usize) -> bool) -> Vec<String> {
        let mut out = Vec::with_capacity(
            self.ambiguous_functions.len() + self.ambiguous_classes.len(),
        );
        out.extend(
            self.functions
                .iter()
                .filter(|(_, site)| keep(site.file))
                .map(|(fqn, _)| format!("f:{fqn}")),
        );
        out.extend(
            self.fn_by_simple
                .iter()
                .filter(|(_, sites)| sites.iter().any(|site| keep(site.file)))
                .map(|(simple, _)| format!("s:{simple}")),
        );
        out.extend(
            self.classes
                .iter()
                .filter(|(_, site)| keep(site.file))
                .map(|(fqn, _)| format!("c:{fqn}")),
        );
        // An alias edge is sited at the file whose `class_alias` call writes
        // it, and contributes both ends: the alias name, which the merge may
        // mint, and the target, whose demotion would unmint it. The other way
        // this pair can move — the *target's* file changing under an untouched
        // `class_alias` call — is not this leg's to catch and never was: the
        // affected set's declaration table edges every file naming the alias to
        // the target's own file, so the call-graph closure reaches it in one
        // hop (`affected.rs`, *A literal `class_alias`*).
        for edge in self.class_alias_edges.iter().filter(|edge| keep(edge.file)) {
            out.push(format!("c:{}", edge.alias_fqn));
            out.push(format!("c:{}", edge.target_fqn));
        }
        out.extend(
            self.constants
                .iter()
                .filter(|(_, slot)| keep(**slot))
                .map(|(key, _)| format!("k:{key}")),
        );
        out.extend(self.ambiguous_functions.iter().map(|fqn| format!("f:{fqn}")));
        out.extend(self.ambiguous_classes.iter().map(|fqn| format!("c:{fqn}")));
        out
    }
}

/// One literal `class_alias(target, alias)` a file writes, kept as written and
/// with the writing file's universe slot — the site that lets issue #510's
/// per-file delta answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
struct ShardAlias {
    alias_fqn: String,
    target_fqn: String,
    file: usize,
}

/// Every global table, recomputed from the shards (ADR-0092 §3). Fields are
/// public plumbing: `steins-db` turns the symbol half into a
/// [`crate::ProjectIndex`] and `steins-infer` turns the whole of it into its
/// `project::Index`; sites are universe slots ([`ShardSite`]), which each
/// consumer maps into its own file identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedTables {
    /// Unambiguous function FQN → site.
    pub functions: HashMap<String, ShardSite>,
    /// Function FQNs defined more than once anywhere in the universe.
    pub ambiguous_functions: HashSet<String>,
    /// Unambiguous class FQN → site (textual decls plus resolved aliases).
    pub classes: HashMap<String, ShardSite>,
    /// Class FQNs demoted by any collision — textual, cross-package, or alias.
    pub ambiguous_classes: HashSet<String>,
    /// Lowercased simple function name → every site, in universe file order.
    pub fn_by_simple: HashMap<String, Vec<ShardSite>>,
    /// The A14 records, keyed by the declaring class-like's lowercase FQN, each
    /// key's list in universe file order.
    pub magic_obstacles: HashMap<String, Vec<MagicObstacle>>,
    /// Property names written anywhere, and the computed-name obstacle bit.
    pub property_writes: (HashSet<String>, bool),
    /// Every global constant the universe declares.
    pub constants: HashSet<String>,
    /// Diagnostic path → file slot for every file in the universe.
    pub files: HashMap<String, usize>,
}

/// Recompute every global table from the shards. Order-independent by
/// construction (ADR-0048 §4): handing the same shards over in any order
/// produces an equal [`MergedTables`] — the property the permutation test
/// pins, and what makes a package-parallel generation build deterministic.
#[must_use]
pub fn merge_shards(shards: &[PackageShard]) -> MergedTables {
    let mut m = MergedTables::default();

    // Symbol maps and cross-package ambiguity. Two phases so the outcome is a
    // fact about the multiset of definitions, not about visit order: first
    // union the per-package ambiguity candidates, then fold in the per-package
    // uniques, demoting on any cross-package collision.
    for s in shards {
        m.ambiguous_functions.extend(s.ambiguous_functions.iter().cloned());
        m.ambiguous_classes.extend(s.ambiguous_classes.iter().cloned());
    }
    for s in shards {
        for (fqn, &site) in &s.functions {
            insert_unique(&mut m.functions, &mut m.ambiguous_functions, fqn, site);
        }
        for (fqn, &site) in &s.classes {
            insert_unique(&mut m.classes, &mut m.ambiguous_classes, fqn, site);
        }
    }

    // The simple-name table: concatenate and sort by site, which is exactly
    // the order a single whole-universe scan would have produced.
    for s in shards {
        for (simple, sites) in &s.fn_by_simple {
            m.fn_by_simple.entry(simple.clone()).or_default().extend(sites.iter().copied());
        }
    }
    for sites in m.fn_by_simple.values_mut() {
        sites.sort_unstable();
    }

    // The literal class_alias fold (ADR-0049 §2): resolve every edge against
    // the merged **textual** snapshot (no alias-to-alias chaining, so the
    // result is order-independent, ADR-0048), then mint the edges. An alias
    // colliding with a textual decl of the same FQN, or two alias edges for
    // one name, demotes to ambiguous; an absent or ambiguous target mints
    // nothing.
    let mut resolved: Vec<(String, ShardSite)> = Vec::new();
    for s in shards {
        for edge in &s.class_alias_edges {
            if m.ambiguous_classes.contains(&edge.target_fqn) {
                continue;
            }
            if let Some(&target) = m.classes.get(&edge.target_fqn) {
                resolved.push((edge.alias_fqn.clone(), target));
            }
        }
    }
    for (alias_fqn, target) in resolved {
        insert_unique(&mut m.classes, &mut m.ambiguous_classes, &alias_fqn, target);
    }

    // The obstacle table: order the records by slot (stable, so one file's
    // records keep their scan order) and group by declaring FQN — the same
    // per-key order a whole-universe scan in slot order produces.
    let mut obstacles: Vec<(usize, &MagicObstacle)> =
        shards.iter().flat_map(|s| s.magic_obstacles.iter().map(|(slot, o)| (*slot, o))).collect();
    obstacles.sort_by_key(|&(slot, _)| slot);
    for (_, o) in obstacles {
        m.magic_obstacles.entry(o.class.to_ascii_lowercase()).or_default().push(o.clone());
    }

    // The remaining tables are unions, monotone and order-free.
    for s in shards {
        m.property_writes.0.extend(s.property_writes.0.iter().cloned());
        m.property_writes.1 |= s.property_writes.1;
        m.constants.extend(s.constants.keys().cloned());
        for (path, &slot) in &s.files {
            let entry = m.files.entry(path.clone()).or_insert(slot);
            if slot > *entry {
                *entry = slot;
            }
        }
    }
    m
}

/// Insert `fqn → site`, demoting to ambiguity on any collision. `fqn` is already
/// lowercase-normalized by the syntax layer. Shared by the shard builder (within
/// one package) and the merge (across packages): both are the same multiset
/// question — one definition resolves, two or more never do.
fn insert_unique<S: Copy>(
    map: &mut HashMap<String, S>,
    ambiguous: &mut HashSet<String>,
    fqn: &str,
    site: S,
) {
    if ambiguous.contains(fqn) {
        return;
    }
    if map.remove(fqn).is_some() {
        // A second definition of the same FQN: mark ambiguous, keep it unresolved.
        ambiguous.insert(fqn.to_owned());
    } else {
        map.insert(fqn.to_owned(), site);
    }
}

/// The shard grouping the existing constructors use when no Composer partition
/// is in hand (issue #486): a pure function of the diagnostic path. A path
/// whose components spell `vendor/<a>/<b>/…` with content beneath groups as
/// the vendor package `a/b`; a shallower vendor path (`vendor/autoload.php`,
/// `vendor/composer/*.php`) groups as the stray bucket; everything else is the
/// root bucket. This is a bucketing heuristic, not a classification: the merge
/// is partition-invariant, so a "wrong" bucket can change shard boundaries but
/// never a merged table. The real classification — lock-backed, with kinds —
/// is [`crate::partition::PackagePartition`]'s.
#[must_use]
pub fn fallback_package_key(path: &str) -> String {
    let components: Vec<&str> = path.split(['/', '\\']).collect();
    if let Some(v) = components.iter().position(|c| *c == "vendor") {
        let rest = &components[v + 1..];
        if rest.len() >= 3 {
            return format!("{}/{}", rest[0], rest[1]);
        }
        return Package::VENDOR_STRAY_NAME.to_owned();
    }
    Package::ROOT_NAME.to_owned()
}

/// Append one class-like's own magic-member records to `out` (nothing appended
/// when it carries none). The per-class primitive under the shard builder and
/// under `steins_infer::magic_obstacles`' whole-project listing.
pub fn class_magic_obstacles(tree: &SourceTree, cd: &ClassDecl, out: &mut Vec<MagicObstacle>) {
    let Some(doc) = cd.docblock.as_deref() else { return };
    // Cheap reject: most class docblocks are prose, and the scan below is the
    // only place that pays.
    if !doc.contains('@') {
        return;
    }
    for tag in scan_magic_member_tags(doc) {
        let mixin_target = (tag.kind.is_mixin() && !tag.subject.is_empty())
            .then(|| tree.resolve_class_fqn(&docblock_class_ref(&tag.subject, cd.span.start)));
        out.push(MagicObstacle {
            class: cd.fqn.clone(),
            kind: tag.kind,
            subject: tag.subject,
            mixin_target,
        });
    }
}

/// Turn a class reference **written in a docblock** into a [`NameRef`] resolvable
/// against the declaring file's namespace context. `offset` is the declaration's
/// own position, so the context is the one that governs its `extends` clause.
fn docblock_class_ref(raw: &str, offset: u32) -> NameRef {
    if let Some(rest) = raw.strip_prefix('\\') {
        return NameRef { raw: rest.to_owned(), kind: RefKind::FullyQualified, offset };
    }
    // The `namespace\Foo` relative form (ADR-0049 A8) resolves against the
    // enclosing namespace with no imports applied; `raw` drops the prefix.
    // `get` (not a slice) because a class reference may be non-ASCII, and byte 10
    // is then not guaranteed to be a char boundary.
    if raw.get(..10).is_some_and(|p| p.eq_ignore_ascii_case("namespace\\")) {
        return NameRef { raw: raw[10..].to_owned(), kind: RefKind::Relative, offset };
    }
    let kind = if raw.contains('\\') { RefKind::Qualified } else { RefKind::Unqualified };
    NameRef { raw: raw.to_owned(), kind, offset }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard_over(files: &[(usize, &str, &str)]) -> PackageShard {
        let mut s = PackageShard::default();
        let trees: Vec<(usize, &str, SourceTree)> =
            files.iter().map(|&(slot, path, src)| (slot, path, SourceTree::parse(src))).collect();
        for (slot, path, tree) in &trees {
            s.add_file(*slot, path, tree);
        }
        s
    }

    /// The persisted per-file shard folds back in exactly as the file itself
    /// would have (issue #516): writing every file at the canonical slot 0 and
    /// absorbing it under its real slot reproduces `add_file`, table for
    /// table. The fixture exercises every one of them — a package-level
    /// function and class demotion, a file-level one, alias edges, A14
    /// obstacles, both property-write lanes, constants and the file map.
    #[test]
    fn absorbing_a_file_shard_equals_adding_the_file() {
        let files: [(usize, &str, &str); 4] = [
            (0, "src/a.php", "<?php function dup() {} function only_a() {} class C {} const K_A = 1;"),
            (
                1,
                "src/b.php",
                "<?php function dup() {} /** @method int m() */ class Twice {} class_alias('c', 'made'); $o->w = 1;",
            ),
            (
                2,
                "src/c.php",
                "<?php function twice_here() {} function TWICE_HERE() {} class Dupe {} class Dupe {} define('K_B', 2); $q->{$n} = 5;",
            ),
            (3, "src/d.php", "<?php namespace N; class C {} function only_d() {}"),
        ];
        let direct = shard_over(&files);
        let mut absorbed = PackageShard::default();
        for &(slot, path, src) in &files {
            let tree = SourceTree::parse(src);
            let mut one = PackageShard::default();
            // The canonical slot the payload is written at.
            one.add_file(0, path, &tree);
            absorbed.absorb_file(&one, slot);
        }
        // `fn_by_simple` is a vector per name and the merge sorts it, so the
        // comparison is taken on the merged tables the analysis actually reads.
        assert_eq!(
            merge_shards(std::slice::from_ref(&direct)),
            merge_shards(std::slice::from_ref(&absorbed)),
        );
        assert_eq!(
            direct.files().collect::<std::collections::BTreeMap<_, _>>(),
            absorbed.files().collect::<std::collections::BTreeMap<_, _>>(),
        );
        let mut a = direct.contributed_names();
        let mut b = absorbed.contributed_names();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // The fixture is only honest if the demotions fired.
        assert!(direct.ambiguous_functions.contains("dup"), "package-level demotion");
        assert!(direct.ambiguous_functions.contains("twice_here"), "file-level demotion");
        assert!(direct.ambiguous_classes.contains("dupe"), "file-level class demotion");
    }

    /// ADR-0048 §4 for the generation merge: handing the same shards over in
    /// any package order produces an identical merge — including the tables
    /// where order is observable (`fn_by_simple` vectors, per-class obstacle
    /// lists) and the cross-package demotions (ambiguity, alias collisions).
    #[test]
    fn merge_is_independent_of_package_order() {
        let a = shard_over(&[
            (0, "src/a.php", "<?php function dup() {} function only_a() {} class C {} const K_A = 1;"),
            (2, "src/b.php", "<?php /** @method int m() */ class Twice {} class_alias('c', 'made');"),
        ]);
        let b = shard_over(&[(
            1,
            "vendor/x/y/lib.php",
            "<?php function dup() {} function only_b() {} $o->w = 1; /** @property string $p */ class Twice {}",
        )]);
        let c = shard_over(&[(
            3,
            "vendor/x/z/lib.php",
            "<?php class_alias('c', 'made2'); define('K_B', 2); $q->{$n} = 5;",
        )]);

        let forward = merge_shards(&[a.clone(), b.clone(), c.clone()]);
        let reversed = merge_shards(&[c.clone(), b.clone(), a.clone()]);
        let rotated = merge_shards(&[b, c, a]);
        assert_eq!(forward, reversed);
        assert_eq!(forward, rotated);

        // The fixture is only honest if the cross-package machinery fired.
        assert!(forward.ambiguous_functions.contains("dup"), "cross-package function demotion");
        assert_eq!(forward.fn_by_simple["dup"].len(), 2);
        assert!(forward.fn_by_simple["dup"].is_sorted());
        assert!(forward.ambiguous_classes.contains("twice"), "cross-package class demotion");
        assert_eq!(forward.magic_obstacles["twice"].len(), 2, "both files' A14 records survive");
        assert!(forward.classes.contains_key("made"), "alias minted against the merged snapshot");
        assert!(forward.property_writes.1, "the computed-name bit unions in");
        assert!(forward.property_writes.0.contains("w"));
        assert!(forward.constants.contains("K_A") && forward.constants.contains("K_B"));
    }

    /// The per-file delta (issue #510): one file's edit contributes only the
    /// names *that file* sites, never its package's whole key set. Constants
    /// and `class_alias` edges are sited too — the measurement that put them
    /// there is in [`PackageShard::contributed_names_from`]'s docs.
    #[test]
    fn contributed_names_are_answerable_per_file() {
        let s = shard_over(&[
            (0, "src/edited.php", "<?php namespace App; function edited() {} class Edited {}"),
            (1, "src/quiet.php", "<?php namespace App; function quiet() {} class Quiet {}"),
            (2, "src/aliases.php", "<?php const K = 1; class_alias('app\\\\edited', 'shortcut');"),
        ]);
        let names = |slots: &[usize]| -> HashSet<String> {
            s.contributed_names_from(&slots.iter().copied().collect()).into_iter().collect()
        };

        let edited = names(&[0]);
        assert!(edited.contains("f:app\\edited"), "{edited:?}");
        assert!(edited.contains("s:edited"));
        assert!(edited.contains("c:app\\edited"));
        assert!(!edited.contains("f:app\\quiet"), "the sibling file's names stay out: {edited:?}");
        assert!(!edited.contains("s:quiet"));
        assert!(!edited.contains("c:app\\quiet"));
        assert!(!edited.contains("k:K"), "a constant another file declares: {edited:?}");
        assert!(!edited.contains("c:shortcut"), "an alias another file writes: {edited:?}");

        // The aliasing file's own edit contributes both ends of every edge it
        // writes, and the constants it declares.
        let aliases = names(&[2]);
        assert!(aliases.contains("k:K"), "{aliases:?}");
        assert!(aliases.contains("c:shortcut"), "the alias name it mints: {aliases:?}");
        assert!(aliases.contains("c:app\\edited"), "the target it resolves against: {aliases:?}");
        assert!(!aliases.contains("f:app\\edited"), "and nothing else: {aliases:?}");

        // A file that declares nothing at all contributes nothing at all.
        assert!(names(&[]).is_empty());

        // And the whole-shard reading is still the union over every file.
        let mut whole: Vec<String> = s.contributed_names();
        whole.sort();
        whole.dedup();
        let mut per_file: Vec<String> = names(&[0, 1, 2]).into_iter().collect();
        per_file.sort();
        assert_eq!(whole, per_file);
    }

    /// A name defined twice in one package has no site — the shard demoted it
    /// to the ambiguity set — so it rides every file's answer. Both sides of
    /// the ambiguity move the merged answer, and neither is attributable.
    #[test]
    fn an_ambiguous_name_rides_wholesale() {
        let s = shard_over(&[
            (0, "src/a.php", "<?php function dup() {}"),
            (1, "src/b.php", "<?php function dup() {} function only_b() {}"),
        ]);
        let from_a: HashSet<String> =
            s.contributed_names_from(&HashSet::from([0])).into_iter().collect();
        assert!(from_a.contains("f:dup"), "{from_a:?}");
        assert!(!from_a.contains("f:only_b"));
        // The simple-name table keeps every site, so it *is* answerable.
        assert!(from_a.contains("s:dup"));
        assert!(!from_a.contains("s:only_b"));
    }

    /// The file map is the old side's slot→path bridge: it round-trips the
    /// slots the shard's sites are keyed by.
    #[test]
    fn the_file_map_names_the_slots_the_sites_use() {
        let s = shard_over(&[
            (7, "src/a.php", "<?php function a() {}"),
            (9, "src/b.php", "<?php function b() {}"),
        ]);
        let mut files: Vec<(&str, usize)> = s.files().collect();
        files.sort_unstable();
        assert_eq!(files, vec![("src/a.php", 7), ("src/b.php", 9)]);
        assert_eq!(s.functions["a"].file, 7);
        assert_eq!(s.functions["b"].file, 9);
    }

    /// The bucketing heuristic: deep vendor paths group by `<a>/<b>`, shallow
    /// vendor paths land in the stray bucket, everything else in the root
    /// bucket.
    #[test]
    fn fallback_package_key_buckets_by_vendor_component() {
        assert_eq!(fallback_package_key("vendor/acme/lib/src/A.php"), "acme/lib");
        assert_eq!(fallback_package_key("/abs/proj/vendor/acme/lib/A.php"), "acme/lib");
        assert_eq!(fallback_package_key("vendor/autoload.php"), Package::VENDOR_STRAY_NAME);
        assert_eq!(fallback_package_key("vendor/composer/ClassLoader.php"), Package::VENDOR_STRAY_NAME);
        assert_eq!(fallback_package_key("src/App/Kernel.php"), Package::ROOT_NAME);
    }
}
