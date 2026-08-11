//! Transform #5 — interop-envelope emission (issue #303 / ADR-0082 §7).
//!
//! The sister of [`crate::envelope`]: where that one writes the proven *throw*
//! set as `@throws` tags, this one writes the proven *effect* bound as the
//! interop envelopes of ADR-0082 — upstream's own purity tags, parameterized with
//! a label list at upstream's own suggestion. Same mechanics (create the docblock
//! when absent, extend it losslessly when present, verify the round-trip), a
//! different tag family.
//!
//! ## Read faithfully, write conservatively (ADR-0082 §7)
//!
//! The reading side (slices 1–3) mirrors upstream's semantics quirks included.
//! This side is deliberately **stricter than the tags require**, because a written
//! tag is a contract the repo then owns (ADR-0037). The whole emission policy:
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
//! A bare tag is never written in either family: a bare `@phpstan-impure` is ⊤,
//! which is exactly what the absence of information already means (ADR-0082 §3),
//! so writing one is docblock litter.
//!
//! ## What is already written may be prose (the 2026-08-12 ruling)
//!
//! A writer has to read first, and it reads through the **live** label registry —
//! builtin labels plus this project's plugin registrations — via
//! [`steins_infer::effects::existing_envelope`], which is the analyzer's own
//! classification rather than a second opinion. Under the owner's ruling a tag
//! carrying any label the registry does not know is *unspecified*, whole: current
//! PHPStan discards everything after `@phpstan-impure`, so `@phpstan-impure
//! database` is most likely a human's one-word note, and `@phpstan-impure io.netw`
//! is indistinguishable from one. Neither is a stale bound to correct. Such a site
//! is refused by name and **nothing** is written there — not a replacement, not a
//! second tag beside it.
//!
//! The rule runs in the other direction too: the transform refuses to write a bound
//! containing a label the registry does not know, because that tag would read back
//! as prose rather than as the claim it meant. The two rules together are what make
//! the round trip closed.
//!
//! ## The bound, and what "pure" means here
//!
//! The bound is the **union of both ADR-0067 lanes** — proven ∪ declared — reduced
//! by prefix subsumption ([`steins_infer::effects::normalize_labels`]): a call
//! answered only by a declared `io` may perform any `io`, so an upper bound has to
//! claim it, and a proven `io.fs.read` beside a declared `io.fs` writes `io.fs`
//! alone. Labels are written comma-space separated in lexicographic order — the
//! order `annotate` prints an effect set in, so the margin and the tag read alike.
//!
//! `mutate.local` is the degenerate member every envelope tolerates (ADR-0063 §2.3),
//! and ADR-0082 §3 makes it exactly what `@phpstan-pure` means. So a bound of
//! `{}` or `{mutate.local}` is **pure**, and pure declarations get nothing; a bound
//! that reaches beyond it is written whole, `mutate.local` included, because that
//! is the truth of the bound.
//!
//! ## Idempotence
//!
//! A declaration already carrying a *readable* interop envelope stating the same
//! normalized bound refuses `already-declared`. One stating a different readable
//! bound has that tag's text replaced in place — the transform makes docblocks
//! honest, the way [`crate::honesty`] rewrites a lying `@param`. Running the
//! transform on its own output is therefore a no-op, and a test pins it.
//!
//! ## Post-check surface
//!
//! Like `throws-envelope`, this transform is measured on the **default surface**:
//! writing an envelope *is* a contract change (it is what gives
//! `effect.envelope-exceeded` something to check), so measuring it against the
//! contract layer would let a correct emission veto its own success.

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

/// The declaration's effect summary is **not exhaustive**: some callee is
/// unanalyzable, so there may be effects nothing proved and no label list is an
/// upper bound. ADR-0082 §7 writes nothing rather than a bare ⊤ tag.
pub const REASON_EFFECTS_NOT_EXHAUSTIVE: &str = "effects-not-exhaustive";
/// An interop envelope stating exactly this bound is already written. The second
/// run of the transform lands here (idempotence).
pub const REASON_ALREADY_DECLARED: &str = "already-declared";
/// The declaration carries the **checked** spelling — `#[\Steins\Effect(...)]` or
/// `#[\Steins\Pure]` (ADR-0006). That envelope is authoritative and shadows the
/// docblock stratum outright (ADR-0082 §1), so a docblock twin would be
/// duplication, not information. For a class-level tag, one attribute-bearing
/// method is enough: the class claim would speak for a method whose author already
/// wrote the authoritative bound.
pub const REASON_ATTRIBUTE_ENVELOPE: &str = "attribute-envelope";
/// An interop-envelope tag is already written here and the run's label registry
/// cannot read it as a bound — an unknown label collapses the whole tag to ⊤ (the
/// owner's 2026-08-12 ruling), and a bare class-level `all-methods-impure` says ⊤
/// outright. Those bytes are not a stale bound to correct: current PHPStan
/// discards everything after `@phpstan-impure`, so `@phpstan-impure database` is
/// most likely a human's note. The transform writes **nothing** at such a site —
/// no replacement, no second tag — and says so by name.
pub const REASON_EXISTING_TAG_UNREADABLE: &str = "existing-tag-unreadable";
/// The bound the engine computed contains a label the run's registry does not
/// know, so the tag this transform would write reads back as prose (⊤) rather than
/// as the bound it meant. Writing a claim we cannot re-read is worse than writing
/// nothing.
pub const REASON_BOUND_LABEL_UNKNOWN: &str = "bound-label-unknown";
/// The existing docblock offers no lossless insertion point, or the written tag did
/// not survive the re-parse round-trip. Shared verbatim with the `@throws` sister
/// transform — same mechanics, same reason name.
pub const REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE: &str =
    crate::envelope::REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE;
/// The declaration head does not start its own line, so a docblock cannot be
/// inserted above it without rewriting foreign bytes. Shared with the `@throws`
/// sister transform.
pub const REASON_DECLARATION_MID_LINE: &str = crate::envelope::REASON_DECLARATION_MID_LINE;

/// The by-ref-into-a-caller-local color (ADR-0063 §2.3), which every envelope
/// tolerates and which ADR-0082 §3 reads `@phpstan-pure` as. A bound consisting of
/// nothing else is the **pure** bound, and pure is exactly what this transform
/// never writes per declaration.
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

/// A decision that produced an edit, held back until the whole file's splice is
/// re-parsed and the written tag is read back (the round-trip check).
struct Staged {
    site: SiteRef,
    edit: Edit,
    target: Target,
    expect: Expect,
}

/// Plan interop-envelope emission over `project`. Pure planning: no files are
/// written and no diagnostics are re-checked here — the caller (CLI) drives the
/// dry-run diff, the default-surface post-check, and any `--apply` write (ADR-0034
/// point 3).
///
/// Like its `@throws` sister this takes no vouch set: an effect bound is a forward
/// fact of the declaration's own body and callees, so the ADR-0046 §2
/// caller-enumerability obstacles have no bearing on it — `eval` can add more
/// effects to a *caller*, never un-prove what this body does.
///
/// `partitions` is the region map (ADR-0047 §6), `None` for the single-region
/// identity. No emission decision reads the map; the parameter reserves the seam.
#[must_use]
pub fn plan_effects_envelope(
    db: &dyn Db,
    project: Project,
    partitions: Option<&crate::regions::PartitionMap>,
) -> TransformReport {
    // The region map is accepted but not consumed: no emission verdict reads it.
    let _ = partitions;
    let sweep: EffectSweep = sweep_effects(db, project);
    let files: Vec<SourceFile> = project.files(db).to_vec();
    let layout = project.layout(db);
    // The **live** label registry — builtin plus this project's plugin
    // registrations (ADR-0068) — the same one the checker beside us asks. A
    // transform that classified labels against a different vocabulary than the
    // analyzer would write tags the analyzer cannot read (and refuse to touch tags
    // it can).
    let plugins = project.plugins(db);

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Vendor declarations are never candidates: a docblock write into
        // `vendor/` is outside the tool's write contract (ADR-0015). The effect
        // sweep still spans vendor, so propagation through vendor callees works.
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
            // The class-level claim is decided first: where it holds, ADR-0082 §7
            // writes it *instead of* per-method tags.
            if class_is_provenly_pure(class, &sweep) {
                decide_class(&mut em, class, tree, &sweep);
            }
            // The per-method pass runs either way, and that is safe rather than
            // contradictory: a class only qualifies above when **every** method is
            // provenly pure, and a pure declaration never produces a tag — so a
            // qualifying class cannot also grow a method tag. What the pass still
            // owes those methods is the one decision that is not about writing:
            // a method whose own tag the registry cannot read is refused by name
            // (rule 2), while the class-wide claim stands on provenness alone
            // (rule 3) and upstream's nearest-wins keeps that method's own story
            // truthful.
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

/// One file's emission context: what every decision reads (the run's registry and
/// the file's bytes) and what every decision writes into (the staged edits, the
/// refusals, the oracle). Bundled so a decision takes its inputs as arguments and
/// its outputs as one `&mut`, rather than eight parameters.
struct Emission<'a> {
    /// The plugin channel, consulted for the **live** label registry alone.
    plugins: &'a PluginFacts,
    fcx: &'a FileCtx<'a>,
    staged: Vec<Staged>,
    refusals: &'a mut Vec<Refusal>,
    oracle: &'a mut CompletenessOracle,
}

impl Emission<'_> {
    /// Record a refusal against the oracle in one place, so no arm can forget the
    /// count (ADR-0034 point 3b).
    fn refuse(&mut self, site: SiteRef, reason: &'static str, detail: String) {
        self.oracle.refused += 1;
        self.refusals.push(Refusal::new(site, reason, detail));
    }
}

/// The facets of a declaration an emission decision reads — the same four for a
/// free function and a method, so one `decide` serves both.
struct DeclShape<'a> {
    /// Whether the **checked** spelling (`#[\Steins\Effect]` / `#[\Steins\Pure]`)
    /// is present, which shadows this whole stratum (ADR-0082 §1).
    has_attribute: bool,
    docblock: Option<&'a String>,
    docblock_span: Option<Span>,
    name_span: Span,
}

/// Decide one function/method: refuse with a named reason, or stage the edit that
/// writes (or corrects) its `@phpstan-impure` bound.
///
/// A declaration whose bound is **pure** is not a candidate and is not enumerated:
/// ADR-0082 §7 writes no per-declaration pure tag, so there is no decision to
/// account for. The one exception is a declaration whose existing tag the registry
/// cannot read — "we left your docblock alone" is an answer the report owes,
/// whatever the bound turned out to be.
fn decide_decl(
    em: &mut Emission,
    eff: &DeclEffects,
    site: SiteRef,
    target: Target,
    shape: DeclShape,
) {
    let bound = eff.bound();
    // A checked envelope shadows this whole stratum (ADR-0082 §1), and the
    // shadowing is total: the checker does not read the docblock there, so neither
    // does this — no tag beside an attribute is classified, corrected, or refused
    // about.
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
    // Before anything about our own knowledge: those bytes are not ours to move.
    // An unknown label makes the whole tag unspecified (the owner's 2026-08-12
    // ruling), and the likeliest reading of `@phpstan-impure database` — which
    // current PHPStan discards wholesale — is a human's note, not a stale bound.
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
    // The bound is spelled from proven/declared labels, which are registry-known
    // wherever they came from the catalog or the plugin channel. The one way one
    // is not is an unknown label in a *checked* attribute envelope reaching this
    // declaration's declared lane — `effect.unknown-label` already reports that on
    // the attribute, and writing the label on into a docblock would produce a tag
    // whose own next run reads it as prose. Refuse instead. (The same guard covers
    // the accepted scenario where a plugin that registered a label is later
    // unloaded: the tag we wrote then reads as prose, and the rule above leaves it
    // untouched.)
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
        // A readable envelope is already written here. Same bound → nothing to do;
        // different bound → rewrite that tag's text in place.
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
        // No envelope yet: insert the tag line, creating the docblock if needed.
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
    // The class-level tag families, read through the same registry (ADR-0082 §5
    // keeps them apart from the method-level pair). An unreadable class tag is
    // prose we must not overwrite, exactly as at a method.
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
                // Narrowing someone's `all-methods-impure` to `all-methods-pure`
                // would rewrite a claim that is *wider* than the truth, not one
                // that is false. Write conservatively: leave it, and say so.
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

/// Whether every method the class **declares** is pure by the proven bound —
/// void-returning ones and the constructor included (ADR-0082 §7's deliberate
/// strictness: the void quirk is a *reading* quirk of upstream's tag, and the
/// write only happens where the claim is true under any reading).
///
/// Exhaustiveness is *not* read here: a non-exhaustive method makes the class a
/// candidate that refuses with a named reason, not a silent non-candidate — the
/// ADR-0034 completeness oracle wants that decision visible.
///
/// Three shapes are excluded outright, because for them a proven-pure reading is
/// not available at all:
/// - an **interface** (and a trait, whose members are not lowered): no bodies, so
///   nothing about an implementation is proven;
/// - a class with an **abstract** method, for the same reason;
/// - a class-like declaring **no** method: `@phpstan-all-methods-pure` over the
///   empty set is vacuously true and says nothing.
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

/// The written label list: comma-space separated, in the normalized (lexicographic)
/// order the bound already carries.
fn render_labels(bound: &[String]) -> String {
    bound.join(", ")
}

/// The method/function-level envelope families (ADR-0082 §5: the class-level pair
/// written on a method says nothing about that method).
fn method_level(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::Pure | EnvelopeTag::Impure)
}

/// The class-level envelope families.
fn class_level(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::AllMethodsPure | EnvelopeTag::AllMethodsImpure)
}

/// The interop envelope already written in `doc`: **how the checker reads it**, and
/// where its bytes are.
///
/// Both halves are needed and neither substitutes for the other. The reading is
/// [`steins_infer::effects::existing_envelope`] — the analyzer's own classification,
/// registry and unknown-label ruling included — and it decides whether the tag may
/// be touched at all. The span is where a correction would go, and it comes from
/// the scanner because the reading does not carry one. Both name the *first* tag of
/// an accepted family, so they cannot end up describing different tags.
fn existing_tag(
    plugins: &PluginFacts,
    doc: Option<&String>,
    accept: impl Fn(EnvelopeTag) -> bool + Copy,
) -> (ExistingEnvelope, Option<DocSpan>) {
    let reading = existing_envelope(plugins, doc, accept);
    let span = doc.and_then(|d| envelope_tag_span(d, accept));
    (reading, span)
}

/// The docblock-relative span of the first interop-envelope tag in `doc` whose
/// family `accept` admits — the tag proper, from its `@` to the end of its trimmed
/// content.
fn envelope_tag_span(doc: &str, accept: impl Fn(EnvelopeTag) -> bool) -> Option<DocSpan> {
    scan_docblock(doc).into_iter().find_map(|t: DocTag| match t.kind {
        TagKind::InteropEnvelope(env) if accept(env) => Some(t.tag_span),
        _ => None,
    })
}

/// Rewrite an existing envelope tag's text in place — the honesty repair applied to
/// a bound (ADR-0082 §7's "makes docblocks honest"). Only the tag's own bytes move;
/// the gutter, the rest of the line and every other line are byte-preserved.
///
/// A grammar-legal trailing comment (`@phpstan-impure io.db (cache TTL)`) is carried
/// over: the bound was wrong, the author's note about it was not. Any *other*
/// trailing prose is not — the only tag that can carry some is a `@phpstan-pure`
/// with a description, and its claim is what this edit exists to correct.
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

/// Record a refusal against the oracle in one place, so no arm can forget the
/// count (ADR-0034 point 3b).
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

/// The round-trip gate (ADR-0003 applied to emission), the `@throws` sister's
/// verbatim: apply the file's staged edits to a scratch copy, re-parse it, and admit
/// each staged edit into the real plan only if the target declaration's docblock now
/// carries the written tag with exactly the written labels — proving the parser
/// reads the tag back as intended, and hence that a second run sees it
/// (idempotence).
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
    // A scratch plan for the splice. An overlap here (two decisions claiming the
    // same bytes) is an internal invariant break surfaced as a refusal, never a
    // panic — mirroring the `@throws` sister.
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
