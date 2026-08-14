//! Transform #5 — interop-envelope emission (issue #303 / ADR-0082 §7).
//!
//! Writes the proven *effect* bound as ADR-0082's interop envelopes (upstream's
//! purity tags) — sister of [`crate::envelope`], which writes the proven
//! *throw* set as `@throws`, same mechanics.
//!
//! ## Emission policy (ADR-0082 §7): stricter than the tags require
//!
//! A written tag is a contract the repo then owns (ADR-0037):
//!
//! | Situation | Written |
//! |---|---|
//! | exhaustive summary, bound has a label beyond `mutate.local` | `@phpstan-impure <labels>` |
//! | every method of a class provenly, exhaustively pure | `@phpstan-all-methods-pure` on the class, and **no** method tags |
//! | non-exhaustive summary | nothing (`effects-not-exhaustive`) |
//! | pure function / method | nothing — no per-declaration `@phpstan-pure` is ever written |
//! | declaration carries `#[\Steins\Effect]` / `#[\Steins\Pure]` | nothing (`attribute-envelope`) |
//! | a tag is already written that the registry cannot read | nothing (`existing-tag-unreadable`) |
//! | the computed bound names a label the registry does not know | nothing (`bound-label-unknown`) |
//!
//! A bare tag (⊤) is never written: absence of information already means ⊤
//! (ADR-0082 §3).
//!
//! ## Unknown labels are prose, not a stale bound (2026-08-12 ruling)
//!
//! Reading uses the **live** label registry via
//! [`steins_infer::effects::existing_envelope`]. An unknown label makes a tag
//! unspecified whole (PHPStan discards all after `@phpstan-impure`) — refused,
//! nothing written; the same blocks writing a bound with an unknown label,
//! closing the round trip both ways.
//!
//! ## The bound
//!
//! **Union of both ADR-0067 lanes** (proven ∪ declared), reduced by prefix
//! subsumption ([`steins_infer::effects::normalize_labels`]), written
//! comma-space separated in lexicographic order (matches `annotate`).
//! `mutate.local` (ADR-0063 §2.3) is what ADR-0082 §3 reads as `@phpstan-pure`:
//! `{}` or `{mutate.local}` is **pure** and gets nothing written; wider bounds
//! are written whole, `mutate.local` included.
//!
//! ## Idempotence and post-check
//!
//! Same normalized bound already declared → `already-declared`; a different
//! bound has its text replaced in place (the honesty repair [`crate::honesty`]
//! applies to `@param`), so a second run is a no-op (test-pinned). Measured on
//! the **default surface** like `throws-envelope`: an envelope write is itself
//! a contract change (feeds `effect.envelope-exceeded`), so the contract layer
//! would let a correct emission veto its own success.

use steins_db::{Db, PluginFacts, Project, SourceFile, parse};
use steins_infer::effects::{
    DeclEffects, EffectSweep, ExistingEnvelope, existing_envelope, normalize_labels, sweep_effects,
    unknown_labels,
};
use steins_phpdoc::ast::Span as DocSpan;
use steins_phpdoc::{DocTag, EnvelopeTag, TagKind, scan_docblock};
use steins_syntax::{ClassDecl, MethodDecl, Span, SourceTree};

use crate::envelope::{FileCtx, HeadKind, create_docblock, extend_docblock};
use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};

// ---- Stable refusal reason names (ADR-0034 point 2) ------------------------

/// Not exhaustive (an unanalyzable callee): no label list is an upper bound
/// (ADR-0082 §7 writes nothing rather than a bare ⊤ tag).
pub const REASON_EFFECTS_NOT_EXHAUSTIVE: &str = "effects-not-exhaustive";
/// This exact bound is already declared (idempotence: second run lands here).
pub const REASON_ALREADY_DECLARED: &str = "already-declared";
/// Carries the **checked** spelling — `#[\Steins\Effect(...)]` / `#[\Steins\Pure]`
/// (ADR-0006) — which shadows the docblock stratum outright (ADR-0082 §1); a
/// twin would duplicate, not inform, and one attribute-bearing method refuses
/// the class-level tag too.
pub const REASON_ATTRIBUTE_ENVELOPE: &str = "attribute-envelope";
/// An already-written tag the registry cannot read as a bound — an unknown
/// label collapses it to ⊤ (2026-08-12 ruling), so it may be a human's note,
/// not a stale bound; nothing is written over it.
pub const REASON_EXISTING_TAG_UNREADABLE: &str = "existing-tag-unreadable";
/// The computed bound names a label the registry does not know, so the tag
/// would read back as prose (⊤) rather than the bound it meant.
pub const REASON_BOUND_LABEL_UNKNOWN: &str = "bound-label-unknown";
/// No lossless insertion point, or the written tag did not survive the
/// re-parse round-trip. Shared with the `@throws` sister (same reason name).
pub const REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE: &str =
    crate::envelope::REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE;
/// The declaration head does not start its own line, so a docblock would
/// rewrite foreign bytes. Shared with the `@throws` sister.
pub const REASON_DECLARATION_MID_LINE: &str = crate::envelope::REASON_DECLARATION_MID_LINE;

/// The by-ref-into-caller-local color (ADR-0063 §2.3) ADR-0082 §3 reads as
/// `@phpstan-pure`; nothing else present is **pure**.
const MUTATE_LOCAL: &str = "mutate.local";

/// The interop-envelope emission transform (issue #303).
#[derive(Debug, Clone, Copy, Default)]
pub struct EffectsEnvelope;

impl Transform for EffectsEnvelope {
    fn id(&self) -> &'static str {
        "effects-envelope"
    }
}

/// Which declaration a staged edit annotates, for the post-splice re-parse lookup.
enum Target {
    /// Free function, by lowercase-normalized FQN.
    Func(String),
    /// Method, by `(class_fqn, method)` — both ASCII-lowercased.
    Method(String, String),
    /// Class-like, by ASCII-lowercased FQN (the class-level tag).
    Class(String),
}

/// What the re-parse must read back on the target for the edit to enter the plan.
enum Expect {
    /// `@phpstan-impure <labels>` with exactly these labels, in this order.
    Impure(Vec<String>),
    /// `@phpstan-all-methods-pure`.
    AllMethodsPure,
}

/// A decision that produced an edit, held back for the round-trip check.
struct Staged {
    site: SiteRef,
    edit: Edit,
    target: Target,
    expect: Expect,
}

/// Plan interop-envelope emission over `project`. Pure planning — the caller
/// (CLI) drives the dry-run diff, default-surface post-check, and `--apply`
/// write (ADR-0034 point 3). Takes no vouch set, like its `@throws` sister: an
/// effect bound is a forward fact of the body and callees, so ADR-0046 §2
/// caller-enumerability has no bearing (`eval` can add effects to a *caller*,
/// never un-prove this body). `partitions` is the region map (ADR-0047 §6),
/// `None` for the single-region identity; no decision here reads it.
#[must_use]
pub fn plan_effects_envelope(
    db: &dyn Db,
    project: Project,
    partitions: Option<&crate::regions::PartitionMap>,
) -> TransformReport {
    let _ = partitions;
    let sweep: EffectSweep = sweep_effects(db, project);
    let files: Vec<SourceFile> = project.files(db).to_vec();
    let layout = project.layout(db);
    // Live registry (builtin + plugins, ADR-0068), same one the checker uses.
    let plugins = project.plugins(db);

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Vendor is never a candidate (ADR-0015); the sweep still spans it so
        // propagation through vendor callees works.
        if layout.is_vendor(path) {
            continue;
        }
        let tree = parse(db, file);
        let fcx = FileCtx::new(path, file.text(db));
        let mut em = Emission {
            plugins,
            fcx: &fcx,
            staged: Vec::new(),
            refusals: &mut refusals,
            oracle: &mut oracle,
        };

        for func in tree.functions() {
            let Some(eff) = sweep.functions.get(&func.fqn) else { continue };
            let p = tree.position(func.span.start);
            let site = SiteRef::new(
                path.to_owned(),
                p.line,
                p.column,
                format!("function {}() @phpstan-impure", func.name),
            );
            decide_decl(
                &mut em,
                eff,
                site,
                Target::Func(func.fqn.clone()),
                DeclShape {
                    has_attribute: func.effect_envelope.is_some(),
                    docblock: func.docblock.as_ref(),
                    docblock_span: func.docblock_span,
                    name_span: func.span,
                },
            );
        }

        for class in tree.classes() {
            // Class-level claim decided first: ADR-0082 §7 writes it *instead
            // of* per-method tags where it holds.
            if class_is_provenly_pure(class, &sweep) {
                decide_class(&mut em, class, tree, &sweep);
            }
            // Per-method pass still runs (safe, not contradictory: a
            // qualifying class's methods are all pure and pure never gets a
            // tag), so it can still refuse an unreadable method tag by name.
            for method in &class.methods {
                let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());
                let Some(eff) = sweep.methods.get(&key) else { continue };
                let p = tree.position(method.span.start);
                let site = SiteRef::new(
                    path.to_owned(),
                    p.line,
                    p.column,
                    format!("{}::{}() @phpstan-impure", class.name, method.name),
                );
                decide_decl(
                    &mut em,
                    eff,
                    site,
                    Target::Method(key.0, key.1),
                    DeclShape {
                        has_attribute: method.effect_envelope.is_some(),
                        docblock: method.docblock.as_ref(),
                        docblock_span: method.docblock_span,
                        name_span: method.span,
                    },
                );
            }
        }

        let staged = std::mem::take(&mut em.staged);
        verify_and_commit(&fcx, staged, &mut plan, &mut refusals, &mut oracle);
    }

    TransformReport {
        plan,
        refusals,
        oracle,
        obstacles: Vec::new(),
        vouched_exemptions: Vec::new(),
        asserted_admissions: Vec::new(),
    }
}

/// One file's emission context: inputs (registry, bytes) and outputs (staged
/// edits, refusals, oracle) bundled into one `&mut` rather than eight params.
struct Emission<'a> {
    /// The plugin channel, consulted for the **live** label registry alone.
    plugins: &'a PluginFacts,
    fcx: &'a FileCtx<'a>,
    staged: Vec<Staged>,
    refusals: &'a mut Vec<Refusal>,
    oracle: &'a mut CompletenessOracle,
}

impl Emission<'_> {
    /// Record a refusal against the oracle in one place (ADR-0034 point 3b).
    fn refuse(&mut self, site: SiteRef, reason: &'static str, detail: String) {
        self.oracle.refused += 1;
        self.refusals.push(Refusal::new(site, reason, detail));
    }
}

/// The facets of a declaration an emission decision reads — same four for a
/// free function and a method, so one `decide` serves both.
struct DeclShape<'a> {
    /// Whether the checked spelling is present (see [`REASON_ATTRIBUTE_ENVELOPE`]).
    has_attribute: bool,
    docblock: Option<&'a String>,
    docblock_span: Option<Span>,
    name_span: Span,
}

/// Decide one function/method: refuse with a named reason, or stage the edit
/// that writes (or corrects) its `@phpstan-impure` bound.
///
/// A **pure** bound is not enumerated: ADR-0082 §7 writes no per-declaration
/// pure tag. Exception: an existing tag the registry cannot read is still
/// enumerated, since "we left your docblock alone" is owed regardless.
fn decide_decl(
    em: &mut Emission,
    eff: &DeclEffects,
    site: SiteRef,
    target: Target,
    shape: DeclShape,
) {
    let bound = eff.bound();
    // Shadows totally (ADR-0082 §1): nothing beside an attribute is read here.
    let (reading, tag_span) = if shape.has_attribute {
        (ExistingEnvelope::Absent, None)
    } else {
        existing_tag(em.plugins, shape.docblock, method_level)
    };
    let unreadable = reading == ExistingEnvelope::Unreadable;
    if is_pure(&bound) && !unreadable {
        return;
    }
    em.oracle.enumerated += 1;

    if shape.has_attribute {
        em.refuse(
            site,
            REASON_ATTRIBUTE_ENVELOPE,
            "the declaration carries a checked effect envelope (#[\\Steins\\Effect] / #[\\Steins\\Pure]); \
             a docblock twin would duplicate the authoritative spelling"
                .to_owned(),
        );
        return;
    }
    // Checked first: those bytes are not ours to move.
    if unreadable {
        em.refuse(
            site,
            REASON_EXISTING_TAG_UNREADABLE,
            "an interop-envelope tag is already written here and the label registry cannot read \
             it as a bound (an unknown label makes the whole tag unspecified); it may well be \
             prose, so nothing is written over it"
                .to_owned(),
        );
        return;
    }
    if !eff.exhaustive {
        em.refuse(
            site,
            REASON_EFFECTS_NOT_EXHAUSTIVE,
            format!(
                "inference is not exhaustive here (proven {}), so no label list is an upper bound; \
                 a bare @phpstan-impure would be ⊤ and is never written",
                render_labels(&bound)
            ),
        );
        return;
    }
    // Unknown labels here mean a checked attribute's declared lane
    // (`effect.unknown-label` already reports it there) or an unloaded plugin.
    let unknown = unknown_labels(em.plugins, &bound);
    if !unknown.is_empty() {
        em.refuse(
            site,
            REASON_BOUND_LABEL_UNKNOWN,
            format!(
                "the proven bound names {}, which this run's label registry does not know; the \
                 tag would read back as prose rather than as a bound",
                render_labels(&unknown)
            ),
        );
        return;
    }

    let tag = format!("@phpstan-impure {}", render_labels(&bound));
    let built = match (&reading, tag_span, shape.docblock_span) {
        (ExistingEnvelope::Bound(env, labels), Some(ts), Some(ds)) => {
            if *env == EnvelopeTag::Impure && normalize_labels(labels.clone()) == bound {
                em.refuse(
                    site,
                    REASON_ALREADY_DECLARED,
                    format!("the declaration already declares {tag}"),
                );
                return;
            }
            Ok(replace_tag(em.fcx, ds, ts, &tag))
        }
        (_, _, Some(ds)) => extend_docblock(em.fcx, ds, std::slice::from_ref(&tag)),
        (_, _, None) => {
            create_docblock(em.fcx, shape.name_span, HeadKind::Function, std::slice::from_ref(&tag))
        }
    };
    match built {
        Ok(edit) => em.staged.push(Staged { site, edit, target, expect: Expect::Impure(bound) }),
        Err((reason, detail)) => em.refuse(site, reason, detail),
    }
}

/// Decide the class-level `@phpstan-all-methods-pure` tag for a class whose every
/// declared method is already known provenly pure ([`class_is_provenly_pure`]).
fn decide_class(em: &mut Emission, class: &ClassDecl, tree: &SourceTree, sweep: &EffectSweep) {
    let p = tree.position(class.span.start);
    let site = SiteRef::new(
        em.fcx.path.to_owned(),
        p.line,
        p.column,
        format!("class {} @phpstan-all-methods-pure", class.name),
    );
    em.oracle.enumerated += 1;

    if class.methods.iter().any(|m| m.effect_envelope.is_some()) {
        em.refuse(
            site,
            REASON_ATTRIBUTE_ENVELOPE,
            "a declared method carries a checked effect envelope (#[\\Steins\\Effect] / \
             #[\\Steins\\Pure]); the class-wide claim would speak for a declaration whose \
             authoritative bound is already written"
                .to_owned(),
        );
        return;
    }
    if let Some(m) = class.methods.iter().find(|m| !method_effects(class, m, sweep).exhaustive) {
        em.refuse(
            site,
            REASON_EFFECTS_NOT_EXHAUSTIVE,
            format!(
                "{}::{}() is not exhaustive, so the class-wide purity claim is not proven",
                class.name, m.name
            ),
        );
        return;
    }
    // Kept apart from the method-level pair (ADR-0082 §5); unreadable = prose.
    match existing_envelope(em.plugins, class.docblock.as_ref(), class_level) {
        ExistingEnvelope::Unreadable => {
            em.refuse(
                site,
                REASON_EXISTING_TAG_UNREADABLE,
                "a class-level interop-envelope tag is already written here and the label \
                 registry cannot read it as a bound; it may well be prose, so nothing is written \
                 over it"
                    .to_owned(),
            );
            return;
        }
        ExistingEnvelope::Bound(env, _) => {
            let detail = match env {
                EnvelopeTag::AllMethodsPure => {
                    "the class already declares @phpstan-all-methods-pure".to_owned()
                }
                // A wider claim (`all-methods-impure`) is not false, so this
                // transform never narrows it.
                _ => "the class already carries a class-level interop envelope; a wider standing \
                      claim is not false, and this transform does not narrow one"
                    .to_owned(),
            };
            em.refuse(site, REASON_ALREADY_DECLARED, detail);
            return;
        }
        ExistingEnvelope::Absent => {}
    }

    let tag = "@phpstan-all-methods-pure".to_owned();
    let built = match class.docblock_span {
        Some(ds) => extend_docblock(em.fcx, ds, std::slice::from_ref(&tag)),
        None => create_docblock(em.fcx, class.span, HeadKind::Class, std::slice::from_ref(&tag)),
    };
    match built {
        Ok(edit) => em.staged.push(Staged {
            site,
            edit,
            target: Target::Class(class.fqn.to_ascii_lowercase()),
            expect: Expect::AllMethodsPure,
        }),
        Err((reason, detail)) => em.refuse(site, reason, detail),
    }
}

/// Whether every declared method is pure by the proven bound, void-returning
/// ones and the constructor included (ADR-0082 §7 strictness). Exhaustiveness
/// is not read here: a non-exhaustive method refuses with a named reason
/// (ADR-0034), not a silent non-candidate. Excluded outright, since no
/// proven-pure reading exists: an **interface** or **trait** (no bodies), a
/// class with an **abstract** method, and a class declaring **no** method.
fn class_is_provenly_pure(class: &ClassDecl, sweep: &EffectSweep) -> bool {
    if class.is_interface || class.is_trait || class.methods.is_empty() {
        return false;
    }
    if class.methods.iter().any(|m| m.is_abstract) {
        return false;
    }
    class.methods.iter().all(|m| is_pure(&method_effects(class, m, sweep).bound()))
}

/// One method's sweep entry, or the empty-and-exhaustive reading when the fixpoint
/// keyed no unit for it (the same default [`steins_infer::effects`] gives).
fn method_effects(class: &ClassDecl, method: &MethodDecl, sweep: &EffectSweep) -> DeclEffects {
    let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());
    sweep
        .methods
        .get(&key)
        .cloned()
        .unwrap_or(DeclEffects { labels: Vec::new(), declared: Vec::new(), exhaustive: true })
}

/// Whether a normalized bound is the **pure** bound: empty, or `mutate.local`
/// alone (ADR-0063 §2.3 / ADR-0082 §3).
fn is_pure(bound: &[String]) -> bool {
    bound.iter().all(|l| l == MUTATE_LOCAL)
}

/// The written label list: comma-space separated, normalized (lexicographic)
/// order.
fn render_labels(bound: &[String]) -> String {
    bound.join(", ")
}

/// The method/function-level envelope families (ADR-0082 §5: the class-level
/// pair on a method says nothing about that method).
fn method_level(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::Pure | EnvelopeTag::Impure)
}

/// The class-level envelope families.
fn class_level(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::AllMethodsPure | EnvelopeTag::AllMethodsImpure)
}

/// The interop envelope already in `doc`: how the checker reads it
/// ([`steins_infer::effects::existing_envelope`]) and where its bytes are
/// (from the scanner); both name the *first* accepted tag.
fn existing_tag(
    plugins: &PluginFacts,
    doc: Option<&String>,
    accept: impl Fn(EnvelopeTag) -> bool + Copy,
) -> (ExistingEnvelope, Option<DocSpan>) {
    let reading = existing_envelope(plugins, doc, accept);
    let span = doc.and_then(|d| envelope_tag_span(d, accept));
    (reading, span)
}

/// Docblock-relative span of the first tag in `doc` whose family `accept`
/// admits — `@` to the end of trimmed content.
fn envelope_tag_span(doc: &str, accept: impl Fn(EnvelopeTag) -> bool) -> Option<DocSpan> {
    scan_docblock(doc).into_iter().find_map(|t: DocTag| match t.kind {
        TagKind::InteropEnvelope(env) if accept(env) => Some(t.tag_span),
        _ => None,
    })
}

/// Rewrite an existing envelope tag's text in place (ADR-0082 §7's "makes
/// docblocks honest"); only the tag's own bytes move, rest byte-preserved. A
/// grammar-legal trailing comment (`@phpstan-impure io.db (cache TTL)`) is
/// carried over — the bound was wrong, not the note — but a `@phpstan-pure`
/// description is not, since that claim is what this edit corrects.
fn replace_tag(fcx: &FileCtx, ds: Span, tag_span: DocSpan, tag: &str) -> Edit {
    let start = ds.start + tag_span.start;
    let end = ds.start + tag_span.end;
    let old = &fcx.text[start as usize..end as usize];
    let comment = old.find('(').map_or(String::new(), |i| format!(" {}", &old[i..]));
    Edit {
        path: fcx.path.to_owned(),
        span: ByteSpan::new(start, end),
        replacement: format!("{tag}{comment}"),
    }
}

/// Free-function twin of [`Emission::refuse`], for the pre-file-context path.
fn refuse(
    oracle: &mut CompletenessOracle,
    refusals: &mut Vec<Refusal>,
    site: SiteRef,
    reason: &'static str,
    detail: String,
) {
    oracle.refused += 1;
    refusals.push(Refusal::new(site, reason, detail));
}

/// The round-trip gate (ADR-0003): apply staged edits to a scratch copy,
/// re-parse, and admit each into the real plan only if the docblock now
/// carries the written tag with exactly the written labels.
fn verify_and_commit(
    fcx: &FileCtx,
    staged: Vec<Staged>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    if staged.is_empty() {
        return;
    }
    // Scratch plan for the splice; a claimed-bytes overlap is an invariant
    // break, surfaced as a refusal, never a panic.
    let mut trial = EditPlan::new();
    let mut kept: Vec<Staged> = Vec::new();
    for s in staged {
        if trial.add_edit(s.edit.clone()).is_err() {
            refuse(
                oracle,
                refusals,
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "internal: emission edits overlapped; skipped".to_owned(),
            );
        } else {
            kept.push(s);
        }
    }
    let edited = trial.apply_file(fcx.path, fcx.text);
    let tree = SourceTree::parse(&edited);

    for s in kept {
        let doc: Option<&str> = match &s.target {
            Target::Func(fqn) => tree
                .functions()
                .iter()
                .find(|f| f.fqn == *fqn)
                .and_then(|f| f.docblock.as_deref()),
            Target::Method(cf, mn) => tree
                .classes()
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(cf))
                .and_then(|c| c.methods.iter().find(|m| m.name.eq_ignore_ascii_case(mn)))
                .and_then(|m| m.docblock.as_deref()),
            Target::Class(cf) => tree
                .classes()
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(cf))
                .and_then(|c| c.docblock.as_deref()),
        };
        if !doc.is_some_and(|d| declares(d, &s.expect)) {
            refuse(
                oracle,
                refusals,
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "the written interop envelope does not round-trip: after the splice the parser \
                 does not read it back on this declaration"
                    .to_owned(),
            );
            continue;
        }
        if plan.add_edit(s.edit).is_err() {
            refuse(
                oracle,
                refusals,
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "internal: emission edit overlapped another edit; skipped".to_owned(),
            );
        } else {
            oracle.transformed += 1;
        }
    }
}

/// Whether `doc` carries the tag the edit meant to write, read back through the
/// same scanner the analyzer reads envelopes with.
fn declares(doc: &str, expect: &Expect) -> bool {
    scan_docblock(doc).iter().any(|t| match (&t.kind, expect) {
        (TagKind::InteropEnvelope(EnvelopeTag::Impure), Expect::Impure(labels)) => {
            t.labels == *labels
        }
        (TagKind::InteropEnvelope(EnvelopeTag::AllMethodsPure), Expect::AllMethodsPure) => true,
        _ => false,
    })
}
