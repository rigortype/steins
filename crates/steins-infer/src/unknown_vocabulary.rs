//! `phpdoc.unknown-vocabulary` (ADR-0091 §6, issue #479): a hyphenated
//! identifier in a phpdoc type position that is not recognized vocabulary.
//!
//! An unrecognized identifier normally forces silence, because it could be a
//! `@template` name, a `@phpstan-type` alias, or a class the index cannot see.
//! For a **hyphenated** one all three are impossible (ADR-0091 §4): PHP's
//! compiler rejects `-` in a class-like name, and §4.1 makes a hyphenated
//! template name or alias a refusal rather than a declaration. What is left is
//! a closed set of two — a misspelling of vocabulary, or vocabulary from a tool
//! Steins does not model — and neither can be a false claim about the
//! *program*. The identifier provably denotes nothing.
//!
//! # The rewrites this runs after
//!
//! The reservation speaks for what **survives** the `@template` shadow, so the
//! ordering is part of the rule and not an implementation detail. Here it holds
//! by construction rather than by a pass: the tag scanner reads a `@template`
//! name with `is_ident_byte`, which excludes `-`, so no hyphenated name can
//! enter a shadow set at all and every hyphenated identifier reaching this
//! module has survived one. `@template T-of-X` scans as a template named `T`;
//! `@param T-of-X` therefore names nothing the docblock declared, and reporting
//! it is what §4.1 rules — an alias or template name in the reserved space is a
//! refusal, not a declaration. `a_truncated_template_name_does_not_shadow`
//! pins it, so a scanner that later admitted `-` would fail here first.
//!
//! # The allowlist, and the baseline it moves
//!
//! Builtin tables ∪ plugin registrations (ADR-0091 §4.1). The builtin half is
//! `steins_contract::is_unknown_vocabulary`, which reads the tables that own
//! the vocabulary so no second list can drift from them. The plugin half is a
//! per-project fact and arrives after plugin load — see [`VocabularyAllowlist`]
//! for what it holds today and why.

use steins_db::PluginFacts;
use steins_phpdoc::ast::{Type as PType, TypeKind as PKind};
use steins_phpdoc::{TagKind, scan_docblock};
use steins_syntax::CommentKind;

use crate::PHPDOC_UNKNOWN_VOCABULARY_ID;
use crate::contract::{for_each_child_type, parse_tag_type};
use crate::cx::Cx;
use crate::docblock_hygiene::hygiene_diag;
use crate::project::Diagnostic;

/// The names allowed into the reserved hyphen space beyond the builtin tables:
/// what this project's **plugins** registered (ADR-0091 §4.1).
///
/// The reservation is deliberately not a freeze. Without a registration channel
/// §3 would fix the vocabulary at whatever Steins happens to ship, and every
/// unmodeled spelling would stay a defect forever; with one, a plugin extends
/// the space the way a PHPStan extension adds type resolution the core does not.
///
/// **Empty for every project today, consistently rather than by omission.**
/// `steins-plugin.json` carries no type-vocabulary registration kind yet — §4.1
/// names it as a *second* kind on the existing manifest and it is not built —
/// so nothing can have registered. The seam exists here anyway because that is
/// where the coupling lands: the check reads a per-project value assembled
/// after plugin load, not a constant, and the day the registration kind ships
/// this is the only place that changes.
pub(crate) struct VocabularyAllowlist<'a> {
    registered: &'a [String],
}

impl<'a> VocabularyAllowlist<'a> {
    /// The allowlist for a project whose plugin channel has already loaded.
    ///
    /// Taking the loaded facts is what states the ordering §4.1 requires:
    /// computing this id before plugin load would report vocabulary the project
    /// legitimately registered. They contribute nothing yet for the reason
    /// above, which is why the argument is read and discarded rather than
    /// omitted — omitting it would let a caller ask the question too early.
    pub(crate) fn for_project(plugins: &'a PluginFacts) -> Self {
        let _ = plugins;
        Self { registered: &[] }
    }

    /// Whether `name` is reported — the whole silence decision, in one place so
    /// a test can ask it without a file to walk.
    ///
    /// The two halves of ADR-0091 §4.1's allowlist, in the order that makes the
    /// cheap one decide most calls: the builtin tables (a hyphen-free name
    /// leaves here immediately, and it is the overwhelmingly common case), then
    /// this project's plugin registrations.
    fn reports(&self, name: &str) -> bool {
        steins_contract::is_unknown_vocabulary(name) && !self.registers(&normalize(name))
    }

    /// Whether `norm` — already normalized as the lowering tables normalize —
    /// is vocabulary this project's plugins registered.
    fn registers(&self, norm: &str) -> bool {
        self.registered.iter().any(|r| normalize(r) == norm)
    }
}

/// The identifier normalization both lowering tables apply: leading `\`
/// stripped, ASCII-lowercased. Namespace qualification carries no meaning in
/// the reserved space (ADR-0091 §3.1 — there is no name to resolve), so the
/// leading backslash is all there is to strip.
fn normalize(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

/// The file's unknown-vocabulary findings, run once per file from `check_units`.
pub(crate) fn unknown_vocabulary(cx: &Cx, allow: &VocabularyAllowlist, out: &mut Vec<Diagnostic>) {
    for comment in cx.tree().comments() {
        if comment.kind != CommentKind::DocBlock {
            continue;
        }
        for tag in scan_docblock(&comment.text) {
            if !carries_a_type(tag.kind) {
                continue;
            }
            // `parse_tag_type` is the same seam the envelopes are read through,
            // so this reports on exactly what gets lowered — and a payload the
            // parser rejects is `phpdoc.unparsable`'s finding, never two.
            let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
            let offset = comment.span.start + tag.tag_span.start;
            // One finding per spelling per tag: `@param foo-bar|list<foo-bar>`
            // is one defect written twice, and the remedy is one edit.
            let mut seen: Vec<String> = Vec::new();
            walk_type(cx, allow, offset, &ty, &mut seen, out);
        }
        // The *declaration* side of the same reservation (ADR-0091 §4.1, issue
        // #472). A hyphenated alias name is refused whole rather than bound as
        // the `foo` an identifier read would truncate `foo-bar` to — and the
        // owner ruling is that the refusal is **reported**, not silent, because
        // a silent one leaves the author with a name that resolves nowhere and
        // no reason why. The refusal is what makes the reading provable: since
        // the alias never binds, the name is still an identifier that denotes
        // nothing, which is exactly what the walk above reports at the use site.
        //
        // **Only where the tag declares anything** (issue #668). PHPStan reads
        // `@phpstan-type` off a class-like's docblock and nowhere else, so the
        // same tag on a function or a method binds no alias there and none here
        // — and a refusal is a claim about a declaration that was attempted.
        // Reporting one where the oracle sees no declaration at all is a finding
        // about a comment, not about the program, which is not what ADR-0091 §6
        // is calibrated for. The use-site walk above is untouched: a hyphenated
        // `@param` denotes nothing wherever it is written.
        if !cx.tree().docblock_heads_a_class_like(comment.span.end) {
            continue;
        }
        for decl in steins_phpdoc::scan_type_aliases(&comment.text) {
            let offset = comment.span.start + decl.at_offset;
            let mut seen: Vec<String> = Vec::new();
            if decl.refused {
                report(cx, allow, offset, &decl.name, &mut seen, out);
                continue;
            }
            // An alias **body** is a type position like any other (issue #669).
            // `@phpstan-type Row foo-bar` floored silently: the body failed to
            // bind, the alias became `Opaque`, and the author was left with a
            // declaration that admits everything and no reason why — while
            // PHPStan says the alias contains an unknown class. Walked through
            // the same two functions the use-site half runs, so the whole
            // allowlist applies and a plugin registration silences a name here
            // exactly as it silences it in a `@param`.
            //
            // Positioned at the tag, not inside the body: `TypeAliasDecl` records
            // the `@`, and a body that wraps across lines has no one offset to
            // point at anyway. `parse_tag_type` for the same reason the use-site
            // walk uses it — a payload the parser rejects is `phpdoc.unparsable`'s
            // finding, never two — and unlike `type_aliases_of` this does not ask
            // whether the body runs to the end of the tag: a spelling that denotes
            // nothing denotes nothing whether or not the declaration binds.
            let steins_phpdoc::TypeAliasBody::Local(body) = &decl.body else { continue };
            let Some(ty) = parse_tag_type(body) else { continue };
            walk_type(cx, allow, offset, &ty, &mut seen, out);
        }
    }
}

/// Whether a tag's payload is a **type position** the reservation speaks for.
/// The assertion family carries one exactly as `@param` does; the tags left out
/// (`@psalm-trace`, the purity and interop envelopes) carry variable names or
/// effect labels, which are not types.
fn carries_a_type(kind: TagKind) -> bool {
    matches!(
        kind,
        TagKind::Param | TagKind::Return | TagKind::Var | TagKind::Throws | TagKind::Assert { .. }
    )
}

/// Every identifier position in one parsed type, reported once each.
///
/// Three node kinds name vocabulary: a bare identifier, a generic's **base**,
/// and a callable's identifier. Array- and object-shape braces spell their own
/// base as a closed enum, so no unrecognized name can reach one.
fn walk_type(
    cx: &Cx,
    allow: &VocabularyAllowlist,
    offset: u32,
    ty: &PType,
    seen: &mut Vec<String>,
    out: &mut Vec<Diagnostic>,
) {
    match &ty.kind {
        PKind::Identifier(name) => report(cx, allow, offset, name, seen, out),
        PKind::Generic { base, .. } => report(cx, allow, offset, base, seen, out),
        PKind::Callable(c) => report(cx, allow, offset, &c.identifier, seen, out),
        _ => {}
    }
    for_each_child_type(ty, &mut |child| walk_type(cx, allow, offset, child, seen, out));
}

/// One identifier, judged against the whole allowlist.
fn report(
    cx: &Cx,
    allow: &VocabularyAllowlist,
    offset: u32,
    name: &str,
    seen: &mut Vec<String>,
    out: &mut Vec<Diagnostic>,
) {
    if !allow.reports(name) {
        return;
    }
    let norm = normalize(name);
    if seen.contains(&norm) {
        return;
    }
    seen.push(norm);
    out.push(hygiene_diag(
        cx,
        PHPDOC_UNKNOWN_VOCABULARY_ID,
        offset,
        format!(
            "`{name}` is not type vocabulary — a class name cannot contain `-`, so the name denotes nothing"
        ),
    ));
}

/// The **allowlist seam** ADR-0091 §4.1 puts between the builtin tables and the
/// plugin channel. What the walk does with a docblock is pinned end to end by
/// `tests/unknown_vocabulary.rs`; what is pinned here is the one decision with
/// no observable surface yet, because no manifest can populate the plugin half.
#[cfg(test)]
mod tests {
    use super::*;

    fn registering(names: &[String]) -> VocabularyAllowlist<'_> {
        VocabularyAllowlist { registered: names }
    }

    /// A registered name is silent; the same name unregistered is not. The
    /// plugin-set dependence of ADR-0091 §4.1, stated as the difference between
    /// two allowlists rather than between two projects — dropping a plugin *is*
    /// this difference, and it is why the id's baseline moves with
    /// configuration and not only with code (ADR-0022).
    #[test]
    fn a_registered_name_is_silent_and_an_unregistered_one_is_not() {
        let registered = ["some-psalm-thing".to_owned()];
        assert!(!registering(&registered).reports("some-psalm-thing"));
        assert!(registering(&[]).reports("some-psalm-thing"));
        // One registration speaks for one name, not for the space around it.
        assert!(registering(&registered).reports("some-psalm-thang"));
    }

    /// A registration is matched the way the lowering tables match: leading `\`
    /// stripped, case-blind, on both sides. A plugin writing `Some-Psalm-Thing`
    /// registers the name a docblock spells `some-psalm-thing`.
    #[test]
    fn a_registration_is_matched_as_the_lowering_tables_match() {
        let registered = ["Some-Psalm-Thing".to_owned()];
        assert!(!registering(&registered).reports("some-psalm-thing"));
        assert!(!registering(&registered).reports("\\SOME-PSALM-THING"));
    }

    /// No registration can take a builtin spelling out of the tables or put a
    /// hyphen-free name into the question: the builtin half decides first, and
    /// it decides both of those.
    #[test]
    fn a_registration_cannot_reach_outside_the_reserved_space() {
        let registered = ["non-empty-string".to_owned(), "Foo".to_owned()];
        assert!(!registering(&registered).reports("non-empty-string"));
        assert!(!registering(&registered).reports("Foo"));
        assert!(!registering(&[]).reports("Foo"));
    }

    /// The channel a project has today contributes nothing, whatever else it
    /// loaded: the manifest carries no type-vocabulary registration kind. This
    /// is the statement that changes when it lands — and the reason the tests
    /// above build their allowlists by hand.
    #[test]
    fn the_plugin_half_is_empty_for_every_project_today() {
        let plugins = PluginFacts::none();
        assert!(VocabularyAllowlist::for_project(&plugins).reports("some-psalm-thing"));
    }
}
