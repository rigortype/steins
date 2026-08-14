//! Transform #3 — `@throws` envelope seeding (issue #115 / ADR-0040).
//!
//! For a declaration the engine **proves** throws — via
//! [`steins_infer::escapes::sweep_escapes`] — this transform writes the
//! missing `@throws` tags, creating or extending the docblock.
//!
//! ## Enumeration domain
//! Every annotatable declaration (free function or method, project files only
//! — vendor is outside the write contract, ADR-0015) with at least one
//! **envelope-relevant** escaping throw class (not provably unchecked; the
//! `Error`/`LogicException` families never count, ADR-0007) is a candidate,
//! transformed or refused with a named reason (ADR-0034 completeness oracle).
//! The write set is exactly the classes for which `throw.undeclared` fires
//! (escape `Yes`, checked `Yes`, uncovered — ADR-0037): a `Maybe` escape
//! refuses `escape-not-proven`; an already-covered escape refuses
//! `already-declared` (why a second run is a no-op).
//!
//! ## Lossless extension (ADR-0003)
//! An existing docblock gets `* @throws \FQN` lines inserted before its
//! closing `*/` — a pure byte insertion, byte-preserving every existing line.
//! No insertion point, or a seeded tag that fails the re-parse round-trip,
//! refuses `docblock-not-round-trippable`; the planner verifies every seeded
//! class is declared before the edit enters the plan. Seeded lines match the
//! file's own terminator, so endings never mix.
//!
//! ## Post-check surface (issue #115 decision)
//! The CLI's post-check measures this transform on the **default surface**
//! only (proof + mechanics), unlike the phpdoc transforms (every layer,
//! contract included): writing `@throws` onto an override gives its
//! ancestor's envelope something to widen against, so `throw.liskov-widened`
//! would fire where there was none — a contract-layer check would veto a
//! correct seed's own success. See `PostCheckSurface` in the CLI.

use steins_db::{Db, Project, SourceFile, parse};
use steins_infer::escapes::{DeclEscapes, EscapeSweep, sweep_escapes};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_syntax::{Span, SourceTree};

use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};

// Stable refusal reason names (ADR-0034 point 2, seeding-specific).

/// Every envelope-relevant class is on the Maybe side (see module doc).
pub const REASON_ESCAPE_NOT_PROVEN: &str = "escape-not-proven";
/// Every proven escape is already covered by the declared `@throws` envelope.
pub const REASON_ALREADY_DECLARED: &str = "already-declared";
/// No lossless insertion point, or the seeded tag failed the round-trip check
/// (see "Lossless extension" in the module doc).
pub const REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE: &str = "docblock-not-round-trippable";
/// The declaration head does not start its own line.
pub const REASON_DECLARATION_MID_LINE: &str = "declaration-mid-line";

/// The `@throws`-envelope seeding transform (issue #115).
#[derive(Debug, Clone, Copy, Default)]
pub struct ThrowsEnvelope;

impl Transform for ThrowsEnvelope {
    fn id(&self) -> &'static str {
        "throws-envelope"
    }
}

/// One file's source text, diagnostic path, and line terminator (matched to
/// the file's own, so CRLF files never acquire lone-LF lines). Shared with
/// the sister transform (`effects-envelope`, ADR-0082 §7).
pub(crate) struct FileCtx<'a> {
    pub(crate) path: &'a str,
    pub(crate) text: &'a str,
    pub(crate) nl: &'static str,
}

impl<'a> FileCtx<'a> {
    pub(crate) fn new(path: &'a str, text: &'a str) -> Self {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Self { path, text, nl }
    }
}

/// Which declaration a staged edit seeds, for the post-splice re-parse
/// lookup: free function by lowercase FQN, or method by ASCII-lowercased
/// `(class_fqn, method)`.
enum DeclKey {
    Func(String),
    Method(String, String),
}

/// A candidate that produced an edit, held for the round-trip check.
struct Staged {
    site: SiteRef,
    edit: Edit,
    classes: Vec<String>,
    decl: DeclKey,
}

/// Plan the `@throws` envelope seeding over `project`. Pure planning: no files
/// are written and no diagnostics are re-checked — the caller (CLI) drives the
/// dry-run diff, post-check, and any `--apply` write (ADR-0034 point 3).
/// Unlike promotion/honesty, takes no vouch set: proven escapes are forward
/// facts, so the dynamic-code "all callers proven" unknowability (ADR-0046
/// §2) doesn't apply — `eval` can only add *more* throwers, never un-prove
/// one. `partitions` is the region map (ADR-0047 §6), unread here (reserves
/// the seam for scoped enumeration).
#[must_use]
pub fn plan_throws_envelope(
    db: &dyn Db,
    project: Project,
    partitions: Option<&crate::regions::PartitionMap>,
) -> TransformReport {
    // Accepted but not consumed (see doc above).
    let _ = partitions;
    let sweep: EscapeSweep = sweep_escapes(db, project);
    let files: Vec<SourceFile> = project.files(db).to_vec();
    let layout = project.layout(db);

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Excluded here (ADR-0015), but the sweep still spans vendor callees.
        if layout.is_vendor(path) {
            continue;
        }
        let tree = parse(db, file);
        let fcx = FileCtx::new(path, file.text(db));

        let mut staged: Vec<Staged> = Vec::new();
        for func in tree.functions() {
            let Some(esc) = sweep.functions.get(&func.fqn) else { continue };
            let p = tree.position(func.span.start);
            let site = SiteRef::new(
                path.to_owned(),
                p.line,
                p.column,
                format!("function {}() @throws", func.name),
            );
            decide(
                esc, site, DeclKey::Func(func.fqn.clone()), func.docblock_span, func.span, &fcx,
                &mut staged, &mut refusals, &mut oracle,
            );
        }
        for class in tree.classes() {
            for method in &class.methods {
                let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());
                let Some(esc) = sweep.methods.get(&key) else { continue };
                let p = tree.position(method.span.start);
                let site = SiteRef::new(
                    path.to_owned(),
                    p.line,
                    p.column,
                    format!("{}::{}() @throws", class.name, method.name),
                );
                decide(
                    esc, site, DeclKey::Method(key.0, key.1), method.docblock_span, method.span,
                    &fcx, &mut staged, &mut refusals, &mut oracle,
                );
            }
        }

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

/// Decide one candidate: refuse, or stage its edit for the round-trip check.
#[allow(clippy::too_many_arguments)]
fn decide(
    esc: &DeclEscapes,
    site: SiteRef,
    decl: DeclKey,
    docblock_span: Option<Span>,
    name_span: Span,
    fcx: &FileCtx,
    staged: &mut Vec<Staged>,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    oracle.enumerated += 1;

    let writable: Vec<String> = esc.writable().into_iter().map(|c| c.class.clone()).collect();
    if writable.is_empty() {
        let (reason, detail) = if esc.any_proven() {
            (
                REASON_ALREADY_DECLARED,
                "every proven escape is already covered by the declared @throws envelope"
                    .to_owned(),
            )
        } else {
            let names: Vec<&str> = esc.classes.iter().map(|c| c.class.as_str()).collect();
            (
                REASON_ESCAPE_NOT_PROVEN,
                format!(
                    "no escape is proven ({} might escape); a Maybe escape never becomes a declared envelope",
                    names.join(", ")
                ),
            )
        };
        oracle.refused += 1;
        refusals.push(Refusal::new(site, reason, detail));
        return;
    }

    let tags: Vec<String> = writable.iter().map(|class| format!("@throws \\{class}")).collect();
    let built = match docblock_span {
        Some(ds) => extend_docblock(fcx, ds, &tags),
        None => create_docblock(fcx, name_span, HeadKind::Function, &tags),
    };
    match built {
        Ok(edit) => staged.push(Staged { site, edit, classes: writable, decl }),
        Err((reason, detail)) => {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, reason, detail));
        }
    }
}

/// Extend an existing docblock losslessly: insert one `* <tag>` line per entry
/// of `tags` before the closing `*/` line. `tags` are whole rendered tags
/// without the gutter (`@throws \RuntimeException`, `@phpstan-impure io.db`);
/// the sister transform (ADR-0082 §7) writes its envelopes through this same
/// function.
pub(crate) fn extend_docblock(
    fcx: &FileCtx,
    ds: Span,
    tags: &[String],
) -> Result<Edit, (&'static str, String)> {
    let doc = &fcx.text[ds.start as usize..ds.end as usize];
    let Some(last_nl) = doc.rfind('\n') else {
        return Err((
            REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
            "single-line docblock: a tag line cannot be inserted without rewriting the existing line"
                .to_owned(),
        ));
    };
    let closing = &doc[last_nl + 1..];
    if closing.trim_start() != "*/" {
        return Err((
            REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
            "the closing `*/` shares its line with other content; no lossless insertion point"
                .to_owned(),
        ));
    }
    // Body lines align their `*` with the closing `*`: gutter + `* @throws …`.
    let gutter = &closing[..closing.len() - 2];
    let mut insertion = String::new();
    for tag in tags {
        insertion.push_str(gutter);
        insertion.push_str("* ");
        insertion.push_str(tag);
        insertion.push_str(fcx.nl);
    }
    let at = ds.start + (last_nl as u32) + 1;
    Ok(Edit { path: fcx.path.to_owned(), span: ByteSpan::at(at), replacement: insertion })
}

/// Create a fresh docblock above the declaration's head line, matching its
/// indentation, carrying one `* <tag>` line per entry of `tags`. Refuses when
/// the head does not start its own line.
pub(crate) fn create_docblock(
    fcx: &FileCtx,
    name_span: Span,
    head: HeadKind,
    tags: &[String],
) -> Result<Edit, (&'static str, String)> {
    let name_start = name_span.start as usize;
    let line_start = fcx.text[..name_start].rfind('\n').map_or(0, |p| p + 1);
    let prefix = &fcx.text[line_start..name_start];
    if !head_prefix_ok(prefix, head) {
        return Err((
            REASON_DECLARATION_MID_LINE,
            "the declaration does not start its own line, so a docblock cannot be inserted losslessly above it"
                .to_owned(),
        ));
    }
    let indent: String = prefix.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let mut block = String::new();
    block.push_str(&indent);
    block.push_str("/**");
    block.push_str(fcx.nl);
    for tag in tags {
        block.push_str(&indent);
        block.push_str(" * ");
        block.push_str(tag);
        block.push_str(fcx.nl);
    }
    block.push_str(&indent);
    block.push_str(" */");
    block.push_str(fcx.nl);
    Ok(Edit {
        path: fcx.path.to_owned(),
        span: ByteSpan::at(line_start as u32),
        replacement: block,
    })
}

/// Which declaration head a created docblock goes above (disjoint keyword
/// sets, else a docblock could land between a `class` head and a method
/// sharing its line): `Function` is `public static function f(`; `Class` is
/// `final class C` / `interface I` (ADR-0082 §7's class-level tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadKind {
    Function,
    Class,
}

/// Whether the bytes before the declaration name are nothing but whitespace
/// and declaration-head tokens — guards against a docblock landing
/// mid-statement (`<?php function f() {}`, two declarations on one line).
fn head_prefix_ok(prefix: &str, head: HeadKind) -> bool {
    const FUNCTION_ALLOWED: &[&str] =
        &["abstract", "final", "public", "protected", "private", "static", "function", "&"];
    const CLASS_ALLOWED: &[&str] =
        &["abstract", "final", "readonly", "class", "interface", "enum", "trait"];
    let (allowed, keyword) = match head {
        HeadKind::Function => (FUNCTION_ALLOWED, &["function"][..]),
        HeadKind::Class => (CLASS_ALLOWED, &["class", "interface", "enum", "trait"][..]),
    };
    let mut saw_keyword = false;
    for tok in prefix.split_whitespace() {
        if !allowed.iter().any(|a| tok.eq_ignore_ascii_case(a)) {
            return false;
        }
        if keyword.iter().any(|k| tok.eq_ignore_ascii_case(k)) {
            saw_keyword = true;
        }
    }
    saw_keyword
}

/// The round-trip gate (ADR-0003 applied to seeding, see module doc): apply
/// the file's staged edits to a scratch copy, re-parse, and admit each staged
/// edit into the real plan only if its docblock now declares every seeded
/// class.
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
    // An overlap (two candidates claiming the same insertion point) is an
    // invariant break surfaced as a refusal, never a panic (mirrors `account`
    // in the honesty transform).
    let mut trial = EditPlan::new();
    let mut kept: Vec<Staged> = Vec::new();
    for s in staged {
        if trial.add_edit(s.edit.clone()).is_err() {
            oracle.refused += 1;
            refusals.push(Refusal::new(
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "internal: seeding edits overlapped; skipped".to_owned(),
            ));
        } else {
            kept.push(s);
        }
    }
    let edited = trial.apply_file(fcx.path, fcx.text);
    let tree = SourceTree::parse(&edited);

    for s in kept {
        let doc: Option<&str> = match &s.decl {
            DeclKey::Func(fqn) => tree
                .functions()
                .iter()
                .find(|f| f.fqn == *fqn)
                .and_then(|f| f.docblock.as_deref()),
            DeclKey::Method(cf, mn) => tree
                .classes()
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(cf))
                .and_then(|c| c.methods.iter().find(|m| m.name.eq_ignore_ascii_case(mn)))
                .and_then(|m| m.docblock.as_deref()),
        };
        let verified = doc.is_some_and(|d| declares_all(d, &s.classes));
        if !verified {
            oracle.refused += 1;
            refusals.push(Refusal::new(
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "the seeded @throws tag does not round-trip: after the splice the parser does not read it back on this declaration"
                    .to_owned(),
            ));
            continue;
        }
        if plan.add_edit(s.edit).is_err() {
            oracle.refused += 1;
            refusals.push(Refusal::new(
                s.site,
                REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
                "internal: seeding edit overlapped another edit; skipped".to_owned(),
            ));
        } else {
            oracle.transformed += 1;
        }
    }
}

/// Whether `doc` carries a `@throws` tag spelling `\{class}` for every seeded
/// class, byte-for-byte.
fn declares_all(doc: &str, classes: &[String]) -> bool {
    let tags = scan_docblock(doc);
    classes.iter().all(|class| {
        let want = format!("\\{class}");
        tags.iter()
            .any(|t| t.kind == TagKind::Throws && t.type_text.trim() == want)
    })
}
