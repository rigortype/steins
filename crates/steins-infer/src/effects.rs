//! The per-declaration effect sweep the `effects-envelope` transform consumes
//! (issue #303 / ADR-0082 §7): the narrow seam the transform engine
//! (`steins-edit`) reaches into for the effect system's per-declaration verdicts,
//! exactly as [`crate::escapes`] exposes the throw system's.
//!
//! The sweep reuses the checker's own effect fixpoint rather than forking it, runs
//! it **once** for the whole project, and returns plain data keyed the way the
//! transform enumerates declarations. Candidate enumeration, refusal assembly and
//! the edit mechanics stay in the transform crate.
//!
//! ## What a declaration's entry means
//!
//! [`DeclEffects`] is the two-lane ADR-0067 answer, unflattened:
//! [`labels`](DeclEffects::labels) is what inference **proved**,
//! [`declared`](DeclEffects::declared) is what a declaration merely **bounds** (an
//! envelope imported at an interface-typed receiver, a plugin coloring), and
//! [`exhaustive`](DeclEffects::exhaustive) says whether every call resolved. A writer
//! of declarations needs all three: the union of the two lanes is the bound it may
//! claim, and a non-exhaustive summary is not a bound at all — there may be
//! effects nothing proved, so ADR-0082 §7 writes **nothing** for it.
//!
//! Unlike [`crate::EffectSummary`], the declared lane here is *raw*: the summary
//! drops a declared label the proven lane already covers because that is a display
//! rule for a margin, while a writer normalizes the two lanes together — see
//! [`DeclEffects::bound`].
//!
//! Every non-abstract declaration in the project gets an entry, including the ones
//! with nothing to say (`{}`, exhaustive). The transform decides what is a
//! candidate; a *missing* entry would make "proven pure" and "never analyzed"
//! indistinguishable, and the class-level tag of ADR-0082 §7 turns on exactly that
//! distinction.
//!
//! ## Reading what is already written
//!
//! A writer also has to read: it must not touch a docblock whose tag it cannot
//! interpret. [`existing_envelope`] is that reader, and it is the **checker's own**
//! classification — the private `interop_tag` behind ADR-0082's read path, exposed
//! here rather than re-implemented, so the transform and the analyzer agree on
//! what a tag says down to the owner's unknown-label ruling.

use std::collections::HashMap;

use steins_db::{Db, PluginFacts, Project, SourceFile, parse, project_index};
use steins_phpdoc::EnvelopeTag;

use crate::{EffectSet, FileUnit, Index, InteropTag, Sym, compute_effects, interop_tag};

/// What the effect fixpoint proves about one function or method.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclEffects {
    /// The **proven** effect labels (ADR-0018 dot-paths), sorted and deduped.
    pub labels: Vec<String>,
    /// The **declared** effect labels bounding calls the body makes (ADR-0067),
    /// sorted and deduped. Never a proof: a non-empty set means some call was
    /// answered by a contract rather than by inference.
    pub declared: Vec<String>,
    /// Whether every call the body makes resolved. `false` means some callee is
    /// unanalyzable — the declaration *may* have effects nothing proved.
    pub exhaustive: bool,
}

impl DeclEffects {
    /// The declaration's **normalized bound**: the two lanes unioned, then reduced
    /// by prefix subsumption ([`normalize_labels`]).
    ///
    /// This is the upper bound a written envelope claims. Unioning is what makes it
    /// an upper bound rather than a description: a call answered only by a declared
    /// `io` may perform any `io`, so the writer must claim `io` too.
    #[must_use]
    pub fn bound(&self) -> Vec<String> {
        normalize_labels(self.labels.iter().chain(self.declared.iter()).cloned())
    }
}

/// Reduce a label multiset to its **normal form**: sorted, deduplicated, and with
/// every label some *other* member already subsumes dropped (ADR-0018 segment-aware
/// prefix subsumption, [`steins_catalog::subsumes`]).
///
/// `{io.fs.read, io.fs}` normalizes to `{io.fs}` — the bound is the same set of
/// admissible effects either way, and the shorter spelling is the one a reader can
/// check at a glance. Sorting is lexicographic, matching the order `annotate`
/// prints an effect set in, so one vocabulary reads the same in the margin and in a
/// written tag.
#[must_use]
pub fn normalize_labels(labels: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = labels.into_iter().collect();
    out.sort();
    out.dedup();
    // Equal labels are already gone, so `subsumes(other, l)` can only fire for a
    // strictly coarser `other` — no member ever drops itself.
    let coarse: Vec<String> = out.clone();
    out.retain(|l| !coarse.iter().any(|o| o != l && steins_catalog::subsumes(o, l)));
    out
}

/// The whole-project effect sweep the `effects-envelope` planner consumes. Free
/// functions key by their lowercase-normalized FQN
/// ([`steins_syntax::FunctionDecl::fqn`]); methods by the `(class_fqn, method)`
/// pair, both ASCII-lowercased (the [`crate::promote::MethodKey`] convention, and
/// [`crate::escapes::EscapeSweep`]'s).
#[derive(Debug, Clone, Default)]
pub struct EffectSweep {
    pub functions: HashMap<String, DeclEffects>,
    pub methods: HashMap<(String, String), DeclEffects>,
}

/// Run the effect fixpoint over `project` and report, per function/method, the
/// proven labels, the declared-lane labels, and exhaustiveness.
#[must_use]
pub fn sweep_effects(db: &dyn Db, project: Project) -> EffectSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);

    let effects = compute_effects(&units, &index, project.plugins(db));
    let mut out = EffectSweep::default();
    for u in &units {
        for f in u.tree.functions() {
            out.functions.insert(f.fqn.clone(), decl_effects(&effects, &Sym::Func(f.fqn.clone())));
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                let sym = Sym::Method(c.fqn.clone(), m.name.clone());
                let key = (c.fqn.to_ascii_lowercase(), m.name.to_ascii_lowercase());
                out.methods.insert(key, decl_effects(&effects, &sym));
            }
        }
    }
    out
}

/// One declaration's two lanes, read off the fixpoint. A symbol the fixpoint never
/// keyed (an abstract method — no body, hence no effect unit) is `{}` and
/// exhaustive, the same reading [`crate::EffectSummary`] gives it.
fn decl_effects(effects: &HashMap<Sym, EffectSet>, sym: &Sym) -> DeclEffects {
    let Some(set) = effects.get(sym) else {
        return DeclEffects { labels: Vec::new(), declared: Vec::new(), exhaustive: true };
    };
    let mut labels: Vec<String> = set.findings.iter().map(|f| f.label.clone()).collect();
    labels.sort();
    labels.dedup();
    let mut declared: Vec<String> = set.declared.iter().cloned().collect();
    declared.sort();
    declared.dedup();
    DeclEffects { labels, declared, exhaustive: set.exhaustive }
}

/// What a docblock **already** says about a declaration's interop envelope, as the
/// checker itself reads it — the public face of the private `InteropTag`.
///
/// Three answers, not two, and the middle one is the owner's unknown-label ruling
/// (2026-08-12): a tag carrying any label the run's registry does not know is
/// *unspecified*, whole. Current PHPStan discards everything after
/// `@phpstan-impure`, so wild code legitimately carries one-word prose
/// (`@phpstan-impure database`), and a writer must recognize it as prose rather
/// than as a bound it may correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingEnvelope {
    /// No tag of the consulted families is written here.
    Absent,
    /// A tag is written and it bounds nothing: either an unknown label collapsed
    /// it to ⊤, or it is a spelling whose bare form means ⊤. Nothing machine-
    /// readable is claimed — and for a *writer*, nothing is safe to rewrite: those
    /// bytes may be a human's note.
    Unreadable,
    /// A usable bound: the tag family as written (so a report can quote the
    /// declaration back in its author's spelling) and its labels, every one of them
    /// known to the registry.
    Bound(EnvelopeTag, Vec<String>),
}

/// Classify the interop envelope written on `docblock`, restricted to the tag
/// families `accept` admits (the method-level `@phpstan-pure`/`@phpstan-impure`
/// pair, or the class-level `all-methods-*` one — ADR-0082 §5 keeps them apart).
///
/// `plugins` supplies the **live** registry — builtin labels plus this project's
/// plugin registrations (ADR-0068) — so a consumer classifies exactly as the
/// analyzer running beside it does. Pass [`PluginFacts::none`] for the builtin-only
/// view.
#[must_use]
pub fn existing_envelope(
    plugins: &PluginFacts,
    docblock: Option<&String>,
    accept: impl Fn(EnvelopeTag) -> bool,
) -> ExistingEnvelope {
    match interop_tag(plugins.registry(), docblock, accept) {
        InteropTag::Absent => ExistingEnvelope::Absent,
        InteropTag::Unbounded => ExistingEnvelope::Unreadable,
        InteropTag::Bound(env, labels) => ExistingEnvelope::Bound(env, labels),
    }
}

/// The members of `labels` the run's registry does not know, in the order given.
///
/// The emission counterpart of [`existing_envelope`]: a writer asks this before it
/// spells a bound, because a tag carrying an unknown label is read back as prose
/// (⊤) rather than as the bound it meant to write.
#[must_use]
pub fn unknown_labels(plugins: &PluginFacts, labels: &[String]) -> Vec<String> {
    let registry = plugins.registry();
    labels.iter().filter(|l| !registry.is_known(l)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::normalize_labels;

    fn norm(labels: &[&str]) -> Vec<String> {
        normalize_labels(labels.iter().map(|l| (*l).to_owned()))
    }

    #[test]
    fn subsumed_labels_drop_out() {
        assert_eq!(norm(&["io.fs.read", "io.fs"]), vec!["io.fs"]);
        assert_eq!(norm(&["io.fs", "io.fs.read", "io"]), vec!["io"]);
    }

    #[test]
    fn siblings_and_duplicates() {
        assert_eq!(norm(&["io.fs.write", "io.fs.read"]), vec!["io.fs.read", "io.fs.write"]);
        assert_eq!(norm(&["io", "io"]), vec!["io"]);
        // A non-segment prefix is not subsumption (ADR-0018): `io` keeps `iota`.
        assert_eq!(norm(&["iota", "io"]), vec!["io", "iota"]);
    }
}
