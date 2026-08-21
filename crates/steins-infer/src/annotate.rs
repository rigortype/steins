//! `annotate` facts (ADR-0020): the Rigor-style margin of proven facts per line, the
//! single-file / project entry points that render it, and the effect-summary entry
//! points that share its project resolution (issue #65). Entry points here are
//! re-exported from the crate root.

use std::collections::HashMap;

use steins_contract::normalize::FinalKeyword;
use steins_db::{
    Db, EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, parse, project_index,
};
use steins_syntax::{ClassDecl, FunctionDecl, SourceTree};

use crate::{Cx, Diagnostic, FileUnit, Folder, Index, Store, analyze_scope, check_units};
use crate::purity::{EffectSummary, effect_summary_units};

// ---------------------------------------------------------------------------
// `annotate` facts (ADR-0020): the Rigor-style margin — proven facts only.
// ---------------------------------------------------------------------------

/// One proven fact the `annotate` margin can print against a source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactKind {
    /// The inferred effect set: `labels` is the proven lane, `declared` the
    /// ADR-0067 declared one (rendered `≤label`, normalized against `labels`),
    /// and `tolerated` the subset of `labels` the `[effects]` policy discharges
    /// wholly at this unit (rendered `~label`, ADR-0084 §4).
    Effects {
        labels: Vec<String>,
        declared: Vec<String>,
        tolerated: Vec<String>,
        exhaustive: bool,
    },
    /// The inferred throw set (ADR-0040): the classes a function/method can raise
    /// that escape it, with a shared `…?` taint marker when non-exhaustive.
    Throws { classes: Vec<String>, exhaustive: bool },
    Value { var: String, rendered: String },
    ExactClass { var: String, class: String },
    Finding { id: &'static str },
}

/// A [`FactKind`] keyed to a 1-based source line (ADR-0020 margin display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFact {
    pub line: u32,
    pub kind: FactKind,
}

impl LineFact {
    /// The margin body (without the `//=>` prefix or padding).
    #[must_use]
    pub fn body(&self) -> String {
        match &self.kind {
            FactKind::Effects { labels, declared, tolerated, exhaustive } => {
                // A tolerated label keeps its place and its spelling and gains a
                // `~`: the effect is still proven and still printed, it is only no
                // longer judged (ADR-0084 §4). Display vocabulary only — no
                // docblock-writing surface reads this rendering.
                let mut parts: Vec<String> = labels
                    .iter()
                    .map(|l| if tolerated.contains(l) { format!("~{l}") } else { l.clone() })
                    .collect();
                // A declared bound shares the braces with the proven labels but
                // wears a `≤`: "at most this, because a contract says so" — never
                // "this happens, because we saw it" (ADR-0067).
                parts.extend(declared.iter().map(|l| format!("≤{l}")));
                if !*exhaustive {
                    parts.push("…?".to_owned());
                }
                format!("effects: {{{}}}", parts.join(", "))
            }
            FactKind::Throws { classes, exhaustive } => {
                let mut parts = classes.clone();
                if !*exhaustive {
                    parts.push("…?".to_owned());
                }
                format!("throws: {{{}}}", parts.join(", "))
            }
            FactKind::Value { var, rendered } => format!("${var} = {rendered}"),
            FactKind::ExactClass { var, class } => format!("${var}: {class} (exact)"),
            FactKind::Finding { id } => format!("✗ {id}"),
        }
    }
}

/// Single-file annotate facts (kept for tests / the no-`--project` CLI path).
#[must_use]
pub fn annotate_facts(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let _ = (functions, classes);
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    annotate_units(
        &units,
        &index,
        0,
        folder,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// Salsa-fed single-file annotate.
#[must_use]
pub fn annotate_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<LineFact> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    annotate_units(
        &units,
        &index,
        0,
        folder,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// Project-aware annotate (ADR-0020, `--project`): compute the margin facts for
/// `target` while resolving names, classes, and effects against the whole
/// `project`. Returns facts for the target file only.
#[must_use]
pub fn annotate_project(
    db: &dyn Db,
    project: Project,
    target: SourceFile,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);
    let Some(target_idx) = handles.iter().position(|&f| f == target) else {
        return Vec::new();
    };
    annotate_units(
        &units,
        &index,
        target_idx,
        folder,
        project.layout(db),
        project.plugins(db),
        project.effects(db),
    )
}

/// Salsa-fed single-file effect summaries — the data source behind `annotate
/// --format json` (issue #65). Mirrors [`annotate_file`], but returns the
/// [`EffectSummary`] list itself rather than rendered [`LineFact`]s, so a JSON
/// consumer keeps the proven-labels/exhaustiveness dimensions distinct instead
/// of reading the `…?`-flattened margin string.
#[must_use]
pub fn effect_summaries_file(db: &dyn Db, file: SourceFile) -> Vec<EffectSummary> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    effect_summary_units(&units, &index, 0, &PluginFacts::none(), &EffectsPolicy::none())
}

/// Project-aware effect summaries (issue #65): the same cross-file resolution
/// [`annotate_project`] uses for the margin, returning [`EffectSummary`] for
/// the target file only.
#[must_use]
pub fn effect_summaries_project(
    db: &dyn Db,
    project: Project,
    target: SourceFile,
) -> Vec<EffectSummary> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);
    let Some(target_idx) = handles.iter().position(|&f| f == target) else {
        return Vec::new();
    };
    effect_summary_units(&units, &index, target_idx, project.plugins(db), project.effects(db))
}

/// Compute the annotate facts for `target` file within a project view.
#[allow(clippy::too_many_arguments)]
fn annotate_units(
    units: &[FileUnit],
    index: &Index,
    target: usize,
    folder: &mut dyn Folder,
    layout: &ProjectLayout,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> Vec<LineFact> {
    let mut facts: Vec<LineFact> = Vec::new();

    // 1. Effects (and throws) on each declaration line in the target file.
    for s in effect_summary_units(units, index, target, plugins, policy) {
        let throws_present = !s.throws.is_empty() || !s.throws_exhaustive;
        facts.push(LineFact {
            line: s.line,
            kind: FactKind::Effects {
                labels: s.labels,
                declared: s.declared,
                tolerated: s.tolerated,
                exhaustive: s.exhaustive,
            },
        });
        // Throws print on the same line, after effects, only when non-empty
        // (or tainted) — one color, one spelling (ADR-0006): throws are their
        // own margin fact, never an effect label.
        if throws_present {
            facts.push(LineFact {
                line: s.line,
                kind: FactKind::Throws { classes: s.throws, exhaustive: s.throws_exhaustive },
            });
        }
    }

    // 2. Value / exact-class facts from the propagation walk of the target file.
    let cx = Cx::new(units, index, target);
    let mut sink: Vec<Diagnostic> = Vec::new();
    for scope in cx.tree().scopes() {
        analyze_scope(
            &cx,
            folder,
            scope,
            HashMap::new(),
            Store::default(),
            None,
            None,
            Some(&mut facts),
            None,
            None,
            None,
            None,
            &mut sink,
        );
    }

    // 3. Findings on the target file (project-wide check, filtered by path).
    let target_path = units[target].path;
    for d in
        check_units(units, index, folder, true, FinalKeyword::Enforced, layout, plugins, policy)
    {
        if d.path == target_path {
            facts.push(LineFact { line: d.line, kind: FactKind::Finding { id: d.id } });
        }
    }

    facts.sort_by_key(|f| f.line);
    facts
}
