//! Transform #3 — `@throws` envelope seeding (issue #115 / ADR-0040).
//!
//! For a declaration the engine **proves** throws — the same proven-escape
//! machinery behind `throw.undeclared` — this transform writes the missing
//! `@throws` tags: creating the docblock when absent, appending to it when
//! present. One command turns proven escapes into declared envelopes, so a repo
//! can adopt the `throws-direct` / `contracts` stages without hand-writing tags
//! or baselining the debt.
//!
//! ## Enumeration domain
//! Every annotatable declaration (free function or method, project files only —
//! vendor is outside the write contract, ADR-0015) with at least one
//! **envelope-relevant** escaping throw class, as reported by
//! [`steins_infer::escapes::sweep_escapes`]: a class not provably unchecked
//! (the `Error`/`LogicException` families never count, ADR-0007). Each
//! candidate is transformed or refused with a named reason — the ADR-0034
//! completeness oracle.
//!
//! ## Only proven escapes are written (ADR-0037)
//! A written tag is a contract the repo then owns — written-by-tool is
//! *declared*, not proven — so the write set is exactly the classes for which
//! `throw.undeclared` fires (or would fire once an envelope exists): escape
//! `Yes`, checked `Yes`, not covered by a declared `@throws`. A `Maybe` escape
//! (or a class whose hierarchy leaves known territory) never becomes a tag; a
//! declaration with nothing proven refuses `escape-not-proven`.
//!
//! ## Idempotence
//! The second run enumerates the same candidates and refuses each with
//! `already-declared`: every proven escape is covered by the envelope the first
//! run wrote (the tag spells the exact FQN the machinery reports, fully
//! qualified, so coverage resolves context-free). Running twice is a no-op.
//!
//! ## Lossless extension (ADR-0003)
//! An existing docblock is extended by inserting whole `* @throws \FQN` lines
//! before its closing `*/` line — a pure byte insertion; every existing line is
//! byte-preserved. A docblock with no such insertion point (a single-line
//! `/** … */`, or content sharing the closing line), or one whose seeded tag
//! the scanner cannot see again after the splice, refuses
//! `docblock-not-round-trippable`. The round-trip is *verified*, not assumed:
//! the planner re-parses each edited file and confirms every seeded class is
//! now declared on its declaration before the edit enters the plan. Seeded lines
//! carry the file's own terminator (`\r\n` in a CRLF file), so a seeded file
//! never acquires mixed endings.
//!
//! ## Post-check surface (issue #115 decision)
//! The CLI's zero-new-diagnostics post-check measures this transform on the
//! **default surface** (proof + mechanics) — and this transform *only*. The two
//! phpdoc transforms stay measured against every layer, contract included.
//!
//! The asymmetry is the point, not an inconsistency: seeding an envelope is
//! supposed to move the contract surface. Writing `@throws` onto an override is
//! precisely what gives its ancestor's narrower envelope something to be widened
//! against, so `throw.liskov-widened` appears where there was none; measured
//! against the contract layer, a correct seed would veto its own success. A
//! promotion or an honesty repair has no such property, so their broader net
//! stays. See `PostCheckSurface` in the CLI for the per-transform choice and the
//! test that pins it.

use steins_db::{Db, Project, SourceFile, parse};
use steins_infer::escapes::{DeclEscapes, EscapeSweep, sweep_escapes};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_syntax::{Span, SourceTree};

use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};

// ---- Stable refusal reason names (ADR-0034 point 2, seeding-specific) ------

/// No escape of the candidate is proven: every envelope-relevant class is on the
/// Maybe side (a `Maybe` escape, or a hierarchy that leaves known territory). A
/// Maybe never becomes a declared envelope (ADR-0037/0040).
pub const REASON_ESCAPE_NOT_PROVEN: &str = "escape-not-proven";
/// Every proven escape is already covered by the declared `@throws` envelope —
/// nothing to seed. The second run of the transform lands here (idempotence).
pub const REASON_ALREADY_DECLARED: &str = "already-declared";
/// The existing docblock offers no lossless insertion point (single-line, or
/// content sharing the closing `*/` line), or the seeded tag did not survive the
/// re-parse round-trip (e.g. the spliced docblock is not the one the parser
/// associates with the declaration).
pub const REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE: &str = "docblock-not-round-trippable";
/// The declaration head does not start its own line (a one-line `<?php function
/// …` file, two declarations sharing a line), so a docblock cannot be inserted
/// above it without rewriting foreign bytes.
pub const REASON_DECLARATION_MID_LINE: &str = "declaration-mid-line";

/// The `@throws`-envelope seeding transform (issue #115).
#[derive(Debug, Clone, Copy, Default)]
pub struct ThrowsEnvelope;

impl Transform for ThrowsEnvelope {
    fn id(&self) -> &'static str {
        "throws-envelope"
    }
}

/// The bytes one file's seeding decisions splice into: its source text, its
/// diagnostic path, and the line terminator to write. Bundled because every edit
/// builder needs all three and none of them varies per candidate.
struct FileCtx<'a> {
    path: &'a str,
    text: &'a str,
    /// The terminator a seeded line ends with, matched to the file's own. Every
    /// *existing* line is byte-preserved either way (the seeded lines are new
    /// bytes); matching it keeps a CRLF file from acquiring lone-LF lines, which
    /// would be a gratuitous diff for every later reader of that file.
    nl: &'static str,
}

impl<'a> FileCtx<'a> {
    fn new(path: &'a str, text: &'a str) -> Self {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Self { path, text, nl }
    }
}

/// Which declaration a staged edit seeds, for the post-splice re-parse lookup.
enum DeclKey {
    /// Free function, by lowercase-normalized FQN.
    Func(String),
    /// Method, by `(class_fqn, method)` — both ASCII-lowercased.
    Method(String, String),
}

/// A candidate that produced an edit, held back until the whole file's splice is
/// re-parsed and each seeded class is verified declared (the round-trip check).
struct Staged {
    site: SiteRef,
    edit: Edit,
    classes: Vec<String>,
    decl: DeclKey,
}

/// Plan the `@throws` envelope seeding over `project`. Pure planning: no files
/// are written and no diagnostics are re-checked here — the caller (CLI) drives
/// the dry-run diff, the default-surface post-check, and any `--apply` write
/// (ADR-0034 point 3).
///
/// Unlike promotion/honesty this transform takes no vouch set: proven escapes
/// are forward facts of the declaration's own body and callees, so the
/// dynamic-code obstacles that make "all callers proven" unknowable (ADR-0046
/// §2) have no bearing here — an `eval` can add *more* throwers, never
/// un-prove a proven escape.
///
/// `partitions` is the region map (ADR-0047 §6), `None` for the single-region
/// identity. No seeding decision reads the map; the parameter reserves the seam
/// for scoped enumeration without changing any verdict.
#[must_use]
pub fn plan_throws_envelope(
    db: &dyn Db,
    project: Project,
    partitions: Option<&crate::regions::PartitionMap>,
) -> TransformReport {
    // The region map is accepted but not consumed: no seeding verdict reads it.
    let _ = partitions;
    let sweep: EscapeSweep = sweep_escapes(db, project);
    let files: Vec<SourceFile> = project.files(db).to_vec();
    let layout = project.layout(db);

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Vendor declarations are never candidates: a docblock write into
        // `vendor/` is outside the tool's write contract (ADR-0015). The escape
        // sweep still spans vendor, so propagation through vendor callees works.
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

/// Decide one candidate: refuse (`escape-not-proven` / `already-declared` / an
/// edit-mechanics reason), or stage its edit for the file's round-trip check.
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

    let built = match docblock_span {
        Some(ds) => extend_docblock(fcx, ds, &writable),
        None => create_docblock(fcx, name_span, &writable),
    };
    match built {
        Ok(edit) => staged.push(Staged { site, edit, classes: writable, decl }),
        Err((reason, detail)) => {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, reason, detail));
        }
    }
}

/// Extend an existing docblock losslessly: insert one `* @throws \FQN` line per
/// class before the closing `*/` line (a pure byte insertion — every existing
/// line is byte-preserved). Refuses when the docblock has no closing line of its
/// own to insert before.
fn extend_docblock(
    fcx: &FileCtx,
    ds: Span,
    classes: &[String],
) -> Result<Edit, (&'static str, String)> {
    let doc = &fcx.text[ds.start as usize..ds.end as usize];
    let Some(last_nl) = doc.rfind('\n') else {
        return Err((
            REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
            "single-line docblock: a @throws line cannot be inserted without rewriting the existing line"
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
    // The closing line's gutter (` */` → one space): body lines align their `*`
    // with the closing `*`, so the inserted line is gutter + `* @throws …`.
    let gutter = &closing[..closing.len() - 2];
    let mut insertion = String::new();
    for class in classes {
        insertion.push_str(gutter);
        insertion.push_str("* @throws \\");
        insertion.push_str(class);
        insertion.push_str(fcx.nl);
    }
    let at = ds.start + (last_nl as u32) + 1;
    Ok(Edit { path: fcx.path.to_owned(), span: ByteSpan::at(at), replacement: insertion })
}

/// Create a fresh docblock above the declaration's head line, matching its
/// indentation. Refuses when the head does not start its own line (nothing but
/// whitespace and declaration-head keywords may precede the name).
fn create_docblock(
    fcx: &FileCtx,
    name_span: Span,
    classes: &[String],
) -> Result<Edit, (&'static str, String)> {
    let name_start = name_span.start as usize;
    let line_start = fcx.text[..name_start].rfind('\n').map_or(0, |p| p + 1);
    let prefix = &fcx.text[line_start..name_start];
    if !head_prefix_ok(prefix) {
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
    for class in classes {
        block.push_str(&indent);
        block.push_str(" * @throws \\");
        block.push_str(class);
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

/// Whether the bytes between a head line's start and the declaration name are
/// nothing but whitespace and declaration-head tokens (`public static function
/// `, `function &`, …) — the guard that keeps a docblock insertion from landing
/// mid-statement (`<?php function f() {}`, two declarations on one line).
fn head_prefix_ok(prefix: &str) -> bool {
    const ALLOWED: &[&str] =
        &["abstract", "final", "public", "protected", "private", "static", "function", "&"];
    let mut saw_function = false;
    for tok in prefix.split_whitespace() {
        if !ALLOWED.iter().any(|a| tok.eq_ignore_ascii_case(a)) {
            return false;
        }
        if tok.eq_ignore_ascii_case("function") {
            saw_function = true;
        }
    }
    saw_function
}

/// The round-trip gate (ADR-0003 applied to seeding): apply the file's staged
/// edits to a scratch copy, re-parse it, and admit each staged edit into the
/// real plan only if its declaration's docblock now declares every seeded class
/// — proving the tag creation/extension is one the parser reads back exactly as
/// intended (and hence that a second run sees the envelope: idempotence).
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
    // A scratch plan for the splice. An overlap here (two candidates claiming
    // the same insertion point) is an internal invariant break surfaced as a
    // refusal, never a panic — mirroring the honesty transform's `account`.
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

/// Whether `doc` carries a `@throws` tag spelling exactly `\{class}` for every
/// seeded class — the fully-qualified spelling the transform writes, compared
/// byte-for-byte (the seeded tag is ours; nothing about it should have changed).
fn declares_all(doc: &str, classes: &[String]) -> bool {
    let tags = scan_docblock(doc);
    classes.iter().all(|class| {
        let want = format!("\\{class}");
        tags.iter()
            .any(|t| t.kind == TagKind::Throws && t.type_text.trim() == want)
    })
}
