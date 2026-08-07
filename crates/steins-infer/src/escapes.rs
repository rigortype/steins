//! The proven-escape sweep for `@throws` envelope seeding (issue #115): the
//! narrow seam the transform engine (`steins-edit`) reaches into for the throw
//! system's per-declaration verdicts, the same way [`crate::promote`] exposes
//! the reverse call-site sweep for promotion/honesty.
//!
//! The sweep reuses the checker's own machinery — the throw fixpoint behind
//! `throw.undeclared` (ADR-0040), the checked accounting (ADR-0007), and the
//! declared-`@throws` resolution — rather than forking it, and returns plain
//! data. The transform crate owns candidate enumeration, refusal assembly, and
//! the edit mechanics.
//!
//! ## What a declaration's entry means
//!
//! A declaration appears in the sweep iff it has at least one **envelope-
//! relevant** escaping throw class: a class that is not provably unchecked (the
//! `Error`/`LogicException` families never count against envelopes, ADR-0007).
//! Each class carries two verdicts:
//!
//! * `proven` — the escape is `Yes` **and** the class is provably checked: the
//!   exact conjunction under which `throw.undeclared` would fire for it. Only
//!   these may become a written envelope entry (ADR-0037: only proven facts are
//!   written; a `Maybe` never becomes a declared contract).
//! * `covered` — a declared `@throws` already admits it (subtype not-`No`,
//!   mirroring the checker's own silence rule).
//!
//! Order is deterministic: classes appear in the source order of their first
//! escaping origin (`(origin_file, offset, class)` — the same sort
//! `throw.undeclared` emission uses), deduplicated case-insensitively.

use std::collections::HashMap;

use steins_db::{Db, Project, SourceFile, parse, project_index};
use steins_domain::Certainty;

use crate::{Cx, FileUnit, Index, Sym, compute_throws, declared_throws, throw_checked, throw_subtype};

/// One envelope-relevant escaping throw class of a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeClass {
    /// The class FQN exactly as the proven-escape machinery reports it (resolved
    /// in the origin file's context, no leading `\`) — the spelling discipline
    /// `throw.undeclared` uses; the transform writes this, never a respelling.
    pub class: String,
    /// `true` iff the escape is proven (`Yes`) **and** the class is provably
    /// checked — the conjunction under which `throw.undeclared` would fire.
    /// `false` is the Maybe side (a `Maybe` escape, or a class whose hierarchy
    /// leaves known territory): never written (ADR-0037/0040).
    pub proven: bool,
    /// `true` iff a declared `@throws` on the declaration already admits this
    /// class (subtype resolution not-`No` — the checker's own silence rule).
    pub covered: bool,
}

/// The envelope-relevant escaping classes of one declaration, in deterministic
/// first-origin source order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclEscapes {
    pub classes: Vec<EscapeClass>,
}

impl DeclEscapes {
    /// The classes a seeding transform may write: proven and not already covered
    /// by the declared envelope, in sweep order.
    #[must_use]
    pub fn writable(&self) -> Vec<&EscapeClass> {
        self.classes.iter().filter(|c| c.proven && !c.covered).collect()
    }

    /// Whether any class is proven (regardless of coverage) — distinguishes the
    /// already-declared verdict from the nothing-proven one.
    #[must_use]
    pub fn any_proven(&self) -> bool {
        self.classes.iter().any(|c| c.proven)
    }
}

/// The whole-project escape sweep the `@throws`-seeding planner consumes.
/// Free functions key by their lowercase-normalized FQN ([`steins_syntax::FunctionDecl::fqn`]);
/// methods by the `(class_fqn, method)` pair, both ASCII-lowercased (the
/// [`crate::promote::MethodKey`] convention).
#[derive(Debug, Clone, Default)]
pub struct EscapeSweep {
    pub functions: HashMap<String, DeclEscapes>,
    pub methods: HashMap<(String, String), DeclEscapes>,
}

/// Run the throw fixpoint over `project` and report, per function/method, the
/// envelope-relevant escaping classes with their proven/covered verdicts.
#[must_use]
pub fn sweep_escapes(db: &dyn Db, project: Project) -> EscapeSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);

    let throws = compute_throws(&units, &index);
    let mut out = EscapeSweep::default();
    for fi in 0..units.len() {
        let cx = Cx::new(&units, &index, fi);
        for f in cx.tree().functions() {
            let sym = Sym::Func(f.fqn.clone());
            if let Some(d) = decl_escapes(&cx, &throws, &sym, f.span.start, f.docblock.as_deref()) {
                out.functions.insert(f.fqn.clone(), d);
            }
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let sym = Sym::Method(c.fqn.clone(), m.name.clone());
                if let Some(d) =
                    decl_escapes(&cx, &throws, &sym, m.span.start, m.docblock.as_deref())
                {
                    let key = (c.fqn.to_ascii_lowercase(), m.name.to_ascii_lowercase());
                    out.methods.insert(key, d);
                }
            }
        }
    }
    out
}

/// One declaration's envelope-relevant classes, or `None` when it has none (an
/// empty entry would enumerate a candidate with nothing to decide).
fn decl_escapes(
    cx: &Cx,
    throws: &HashMap<Sym, crate::ThrowSet>,
    sym: &Sym,
    offset: u32,
    docblock: Option<&str>,
) -> Option<DeclEscapes> {
    let set = throws.get(sym)?;
    let declared = declared_throws(cx, offset, docblock);

    // Deterministic order: the same first-origin source sort `throw.undeclared`
    // emission uses.
    let mut facts: Vec<(&crate::ThrowFact, Certainty)> =
        set.facts.iter().map(|(f, c)| (f, *c)).collect();
    facts.sort_by(|a, b| {
        (a.0.origin_file, a.0.offset, &a.0.class).cmp(&(b.0.origin_file, b.0.offset, &b.0.class))
    });

    let mut classes: Vec<EscapeClass> = Vec::new();
    for (fact, cert) in facts {
        let checked = throw_checked(cx, &fact.class);
        if checked == Certainty::No {
            continue; // Error/LogicException families never count (ADR-0007).
        }
        let proven = cert == Certainty::Yes && checked == Certainty::Yes;
        if let Some(existing) =
            classes.iter_mut().find(|c| c.class.eq_ignore_ascii_case(&fact.class))
        {
            // Same class through several origins: proven if any origin proves it;
            // the first origin keeps the position in the order.
            existing.proven = existing.proven || proven;
            continue;
        }
        let covered =
            declared.iter().any(|d| throw_subtype(cx, &fact.class, d) != Certainty::No);
        classes.push(EscapeClass { class: fact.class.clone(), proven, covered });
    }
    (!classes.is_empty()).then_some(DeclEscapes { classes })
}
