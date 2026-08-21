//! Steins' syntax-tree contract and its Mago parser backend (ADR-0003).
//!
//! # Encapsulation (hard rule)
//!
//! The pinned Mago fork is a dependency of *this crate only* — **no Mago type
//! appears in this crate's public API**. Everything the analyzer sees is the
//! owned, lowered representation here ([`SourceTree`] and its plain-data structs),
//! the seam ADR-0003 requires so parser backends can be swapped freely. Spans are
//! byte offsets, convertible to 1-based line/column via [`SourceTree::position`].

use mago_span::HasSpan;
use mago_syntax::cst::{Identifier, Node, Program, Statement, Trivia, TriviaKind, UseItems};

mod ast;
mod lower_decl;
mod lower_effect;
mod lower_expr;
mod lower_presence;
mod lower_scope;
mod lower_stmt;
pub mod stack_guard;
mod tree;

pub use ast::*;
pub use lower_expr::php_canonical_int_string;
pub use tree::SourceTree;

// ---------------------------------------------------------------------------
// Namespace contexts and name resolution helpers.
// ---------------------------------------------------------------------------

/// Build a [`NameRef`] from a Mago identifier: its raw spelling (leading `\`
/// stripped for fully-qualified names), the qualification [`RefKind`], and the
/// reference's byte offset (for context lookup).
fn name_ref(id: &Identifier<'_>) -> NameRef {
    let kind = match id {
        Identifier::Local(_) => RefKind::Unqualified,
        Identifier::Qualified(_) => RefKind::Qualified,
        Identifier::FullyQualified(_) => RefKind::FullyQualified,
    };
    let raw = bytes_to_string(id.value()).trim_start_matches('\\').to_owned();
    let offset = to_span(id.span()).start;
    // ADR-0049 A8: the `namespace\bar` relative form lexes as a `QualifiedIdentifier`
    // whose first segment is the reserved `namespace` keyword (never a real segment
    // name). Rewrite it to the distinct `Relative` kind, dropping the prefix, so the
    // remainder resolves against the enclosing namespace instead of being appended
    // (the doubled-prefix bug). Case-insensitive: PHP keywords fold case.
    if kind == RefKind::Qualified {
        let first_len = raw.find('\\').unwrap_or(raw.len());
        if raw[..first_len].eq_ignore_ascii_case("namespace") {
            let remainder = raw.get(first_len + 1..).unwrap_or("").to_owned();
            return NameRef { raw: remainder, kind: RefKind::Relative, offset };
        }
    }
    NameRef { raw, kind, offset }
}

/// Build the file's namespace contexts (index 0 = global) and the byte regions
/// each namespace declaration covers. Every `namespace` node in the file becomes
/// one context (its name plus the `use` imports at its body's top level);
/// top-level `use` statements outside any namespace populate the global context.
fn build_contexts(program: &Program<'_>) -> (Vec<NsCtx>, Vec<(u32, u32, usize)>) {
    let mut contexts = vec![NsCtx::global()];
    let mut regions: Vec<(u32, u32, usize)> = Vec::new();

    // Global-context imports: top-level `use` statements (a file with a
    // file-scoped `namespace A;` has none — its statements nest under the node).
    for stmt in program.statements.iter() {
        if let Statement::Use(u) = stmt {
            add_use(u, &mut contexts[0]);
        }
    }

    // One context per namespace declaration, anywhere in the tree. Namespaces do
    // not nest semantically, but a second file-scoped `namespace B;` may sit
    // inside the first's implicit body sequence; a byte offset then falls inside
    // both spans and [`ctx_of`] picks the innermost (latest-starting) region.
    collect_namespaces(&Node::Program(program), &mut contexts, &mut regions);
    (contexts, regions)
}

fn collect_namespaces(
    node: &Node<'_, '_>,
    contexts: &mut Vec<NsCtx>,
    regions: &mut Vec<(u32, u32, usize)>,
) {
    if let Node::Namespace(ns) = node {
        let name = ns
            .name
            .as_ref()
            .map(|id| bytes_to_string(id.value()).trim_start_matches('\\').to_owned())
            .unwrap_or_default();
        let mut ctx = NsCtx { namespace: name, ..NsCtx::global() };
        // `use` imports at the namespace body's top level.
        for stmt in ns.statements().iter() {
            if let Statement::Use(u) = stmt {
                add_use(u, &mut ctx);
            }
        }
        let idx = contexts.len();
        contexts.push(ctx);
        let span = to_span(ns.span());
        regions.push((span.start, span.end, idx));
    }
    for child in children(node) {
        collect_namespaces(&child, contexts, regions);
    }
}

/// Fold one `use` statement's items into a context — every import form: the plain
/// sequence (`use A\B, C\D;`), the typed sequences (`use function a\b;`,
/// `use const A\FOO;`), and the **grouped** forms (`use A\{B, C}`,
/// `use function A\{b, c}`, `use const A\{X, Y}`, and the mixed
/// `use A\{B, function c, const D}`).
///
/// Grouped imports must be lowered because an unresolved import falls back through
/// [`resolve_class_ref`] to the enclosing namespace and can collide with a
/// different class, a false positive (ADR-0049 §6). `use const` items joined the
/// same discipline with issue #198: an unlowered const import would make `FOO`
/// read as `Ns\FOO` and manufacture an absence. Their alias keys are exact-case —
/// see [`NsCtx::const_imports`].
fn add_use(u: &mago_syntax::cst::Use<'_>, ctx: &mut NsCtx) {
    match &u.items {
        UseItems::Sequence(seq) => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                ctx.class_imports.insert(use_item_alias(item), target);
            }
        }
        // `use function a\b;` and `use const A\FOO, B\BAR;` (the latter, issue #198,
        // with exact-case alias keys).
        UseItems::TypedSequence(seq) => {
            let is_fn = seq.r#type.is_function();
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                if is_fn {
                    ctx.fn_imports.insert(use_item_alias(item), target);
                } else {
                    ctx.const_imports.insert(use_item_bound_name(item), target);
                }
            }
        }
        // Grouped `use function A\{b, c}` / `use const A\{X, Y}`: one leading type
        // applies to every item under the `A\` prefix.
        UseItems::TypedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            if list.r#type.is_function() {
                for item in list.items.iter() {
                    ctx.fn_imports.insert(use_item_alias(item), group_target(&prefix, item));
                }
            } else if list.r#type.is_const() {
                for item in list.items.iter() {
                    ctx.const_imports
                        .insert(use_item_bound_name(item), group_target(&prefix, item));
                }
            }
        }
        // Grouped `use A\{B, function c, const D}`: each item carries its own
        // optional type (`None` ⇒ class, `Function` ⇒ function, `Const` ⇒ constant).
        UseItems::MixedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            for mti in list.items.iter() {
                let target = group_target(&prefix, &mti.item);
                match &mti.r#type {
                    None => {
                        ctx.class_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(t) if t.is_function() => {
                        ctx.fn_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(_) => {
                        ctx.const_imports.insert(use_item_bound_name(&mti.item), target);
                    }
                }
            }
        }
    }
}

/// The lowercase-normalized import alias for a `use` item: its explicit `as` alias,
/// else the last segment of the imported name (PHP class/function names are
/// case-insensitive, so the map keys on the lowercased form).
/// Whether a `use` statement binds the (case-sensitive) alias `PHP_VERSION_ID`
/// through any of its **const** item forms (issue #29). The exact-case binding
/// name is the explicit `as` alias, else the imported name's last segment.
fn use_binds_php_version_id(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| bound == "PHP_VERSION_ID")
}

/// The modeled `PREG_*` flag constant names (issue #168) — the four whose values
/// the out-parameter seed resolves. Kept beside the shadow scans that consult it;
/// the values live with the consumer (`steins-infer`), not here.
const PREG_FLAG_CONST_NAMES: &[&str] =
    &["PREG_PATTERN_ORDER", "PREG_SET_ORDER", "PREG_OFFSET_CAPTURE", "PREG_UNMATCHED_AS_NULL"];

/// `use const … as PREG_SET_ORDER` / `use const …\PREG_SET_ORDER` and siblings
/// (issue #168) — see [`use_binds_php_version_id`], whose rules this mirrors for
/// the modeled preg flag constants.
fn use_binds_preg_flag_const(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| PREG_FLAG_CONST_NAMES.contains(&bound))
}

/// Whether a `use` statement `use const`-imports something whose **bound name**
/// (the alias if present, else the last segment) satisfies `wanted`. Constant
/// names are case-sensitive; the match is exact. Const imports are otherwise
/// unlowered (out of scope), so the flags fed from this are the only thing read
/// from them.
fn use_binds_const_named(u: &mago_syntax::cst::Use<'_>, wanted: impl Fn(&str) -> bool) -> bool {
    let item_binds = |item: &mago_syntax::cst::UseItem<'_>| -> bool {
        let bound = match &item.alias {
            Some(a) => bytes_to_string(a.identifier.value),
            None => bytes_to_string(item.name.last_segment()),
        };
        wanted(&bound)
    };
    match &u.items {
        UseItems::TypedSequence(seq) if seq.r#type.is_const() => seq.items.iter().any(item_binds),
        UseItems::TypedList(list) if list.r#type.is_const() => list.items.iter().any(item_binds),
        UseItems::MixedList(list) => list
            .items
            .iter()
            .any(|mti| mti.r#type.as_ref().is_some_and(|t| t.is_const()) && item_binds(&mti.item)),
        _ => false,
    }
}

fn use_item_alias(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
    .to_ascii_lowercase()
}

/// The **exact-case** name a `use` item binds — [`use_item_alias`]'s constant-side
/// twin (issue #198). Same rule (the explicit `as` alias, else the imported name's
/// last segment) with the lowercasing omitted, because constant names are
/// case-sensitive and `use const A\FOO;` binds `FOO`, never `foo`.
fn use_item_bound_name(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
}

/// The full target FQN of a grouped-`use` item: `<prefix>\<item name>`, each side
/// trimmed of a stray leading backslash (grouped items are relative to the prefix).
fn group_target(prefix: &str, item: &mago_syntax::cst::UseItem<'_>) -> String {
    let prefix = prefix.trim_start_matches('\\');
    let name = bytes_to_string(item.name.value());
    let name = name.trim_start_matches('\\');
    format!("{prefix}\\{name}")
}

/// The namespace context enclosing `offset`: the innermost (latest-starting)
/// namespace region containing it, else the global context (index 0).
fn ctx_of<'a>(contexts: &'a [NsCtx], regions: &[(u32, u32, usize)], offset: u32) -> &'a NsCtx {
    let mut best: Option<(u32, usize)> = None;
    for &(start, end, idx) in regions {
        if offset >= start && offset < end && best.is_none_or(|(bstart, _)| start >= bstart) {
            best = Some((start, idx));
        }
    }
    &contexts[best.map_or(0, |(_, idx)| idx)]
}

/// The lowercase-normalized FQN of a declaration named `name` in context `ctx`.
fn fqn_of(ctx: &NsCtx, name: &str) -> String {
    if ctx.namespace.is_empty() {
        name.to_ascii_lowercase()
    } else {
        format!("{}\\{}", ctx.namespace, name).to_ascii_lowercase()
    }
}

/// Resolve a **class** reference to its FQN (case preserved, no leading `\`) in
/// namespace context `ctx`, applying PHP class-name resolution: fully-qualified
/// names pass through; qualified/unqualified names apply `use` class imports on
/// the first segment, else prepend the current namespace. Class references have
/// no global fallback (unlike functions), so this is a pure function of the
/// reference and its context. Shared by [`SourceTree::resolve_class_fqn`] (use-time)
/// and [`RefResolver`] (lowering-time); callers needing the normalized matching
/// key lowercase the case-preserved result.
fn resolve_class_ref(ctx: &NsCtx, r: &NameRef) -> String {
    match r.kind {
        RefKind::FullyQualified => r.raw.clone(),
        RefKind::Qualified => {
            // First segment via class/namespace imports, else current ns.
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            if let Some(target) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{target}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
        RefKind::Unqualified => {
            if let Some(target) = ctx.class_imports.get(&r.raw.to_ascii_lowercase()) {
                target.clone()
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
        // ADR-0049 A8: `namespace\Bar` — the remainder resolves against the enclosing
        // namespace only, no imports (`use` never rebinds a `namespace\`-relative
        // name). In the global namespace it is the remainder itself.
        RefKind::Relative => {
            if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
    }
}

/// Lowering-time namespace resolver for object type hints (ADR-0043). Carries the
/// file's namespace contexts + regions so a class/interface/enum name in a native
/// hint can be resolved to its FQN (case-preserved; lowercased by the caller into
/// the normalized matching key matching [`ClassDecl::fqn`]) at the point of
/// lowering, exactly like the FQN post-pass does for declaration names.
struct RefResolver<'a> {
    contexts: &'a [NsCtx],
    regions: &'a [(u32, u32, usize)],
}

impl RefResolver<'_> {
    /// The case-preserved (source-cased) FQN a class-name reference resolves to,
    /// in the namespace context enclosing its offset. Lowercase the result to get
    /// the normalized matching key.
    fn class_display_fqn(&self, r: &NameRef) -> String {
        resolve_class_ref(ctx_of(self.contexts, self.regions, r.offset), r)
    }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn to_span(span: mago_span::Span) -> Span {
    Span { start: span.start.offset, end: span.end.offset }
}

/// The children of `node`, **or none when the stack is spent** (issue #264).
///
/// Every walker in this file descends through here, so this one function is the
/// whole depth guard for the CST walk: when [`stack_guard::exhausted`] says the
/// remaining headroom is gone, a walker is handed an empty child list and returns
/// the way it would at a leaf. No walker's control flow changes or unwinds, and
/// the parse still produces a (partial) tree, which [`SourceTree::parse`] then
/// reports as a recovered parse error rather than letting the process (or the
/// wasm module) die walking it.
///
/// On every native target the guard is off by default and this is
/// `node.children()` behind one thread-local read; see [`stack_guard`].
fn children<'ast, 'arena>(node: &Node<'ast, 'arena>) -> Vec<Node<'ast, 'arena>> {
    if stack_guard::exhausted() {
        return Vec::new();
    }
    node.children()
}

/// Lower one trivium to a [`Comment`], dropping whitespace trivia (`None`).
fn lower_comment(t: &Trivia<'_>) -> Option<Comment> {
    let kind = match t.kind {
        TriviaKind::SingleLineComment => CommentKind::Line,
        TriviaKind::HashComment => CommentKind::Hash,
        TriviaKind::MultiLineComment => CommentKind::Block,
        TriviaKind::DocBlockComment => CommentKind::DocBlock,
        TriviaKind::WhiteSpace => return None,
    };
    Some(Comment { kind, span: to_span(t.span), text: bytes_to_string(t.value) })
}

fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn strip_dollar(name: String) -> String {
    name.strip_prefix('$').map_or(name.clone(), ToOwned::to_owned)
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}
