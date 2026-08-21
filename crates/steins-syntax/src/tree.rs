//! [`SourceTree`], the owned lowering of one parsed PHP file: its accessors and
//! [`SourceTree::parse`], the single entry point that runs the Mago parser and hands
//! the CST to the private lowering modules. [`Lowered`] is the file-wide walk's
//! hand-off into `parse`.

use mago_allocator::LocalArena;
use mago_database::file::FileId;
use mago_span::HasSpan;
use mago_syntax::cst::{Access, Expression, Node, Statement};

use crate::ast::{
    AnonClassEdge, ArrayLiteralSite, CallExpr, ClassAliasEdge, ClassDecl, Comment, DynamismKind,
    DynamismSite, ForeachSite, FunctionDecl, GlobalConstDecl, NameRef, NativeType, NsCtx,
    OperandSite, ParseError, Position, ReflectionSite, RetBoundKind, Scope, Span, TypeMember,
    UnsetSeedFacts,
};
use crate::lower_decl::{DocIndex, collect_steins_aliases, lower_classes, walk};
use crate::lower_expr::method_name_of;
use crate::stack_guard;
use crate::{
    RefResolver, build_contexts, collect_array_literal_sites, collect_foreach_sites,
    collect_operand_sites, ctx_of, docblock_before, flatten_top_level, fqn_of, line_starts,
    lower_comment, lower_scopes, resolve_class_ref, to_span, unset_seed_facts,
};

/// An owned, Mago-free lowering of one parsed PHP file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceTree {
    strict_types: bool,
    functions: Vec<FunctionDecl>,
    classes: Vec<ClassDecl>,
    calls: Vec<CallExpr>,
    scopes: Vec<Scope>,
    /// Dynamic-code constructs (`eval`/`include`/`require`) found file-wide (ADR-0046 §2),
    /// used for caller-enumeration obstacle detection.
    dynamism: Vec<DynamismSite>,
    /// Compile-time `class_alias('Target','Alias')` edges found file-wide (ADR-0049 §2),
    /// folded into the project index for existence resolution; a runtime-minted alias is a
    /// [`DynamismKind::ClassAlias`] dam site in [`Self::dynamism`] instead.
    class_alias_edges: Vec<ClassAliasEdge>,
    /// Anonymous-class inheritance edges found file-wide (ADR-0049 A4), used by
    /// declared-receiver descendant closure to detect invisible descendants.
    anon_class_edges: Vec<AnonClassEdge>,
    /// Reflection-driven invocation sites found file-wide (issue #30), report-only —
    /// consumed by `steins doctor`'s posture, nothing decision-making. See [`ReflectionKind`].
    reflection: Vec<ReflectionSite>,
    /// Whether this file declares a userland constant named `PHP_VERSION_ID` — a `const`
    /// (any namespace, name-only/project-conservative) or a literal-named `define()` (issue
    /// #29). Any such declaration disables the engine-constant version-guard fold project-wide.
    php_version_id_declared: bool,
    /// Whether this file `use const`-imports something aliased `PHP_VERSION_ID` (issue #29,
    /// file-scoped, exact-case) — an unqualified use then names the import, declining the fold.
    php_version_id_aliased: bool,
    /// Whether this file declares a userland twin of a modeled `PREG_*` flag constant
    /// (issue #168), same name-only reading as [`Self::php_version_id_declared`].
    preg_flag_const_declared: bool,
    /// Whether this file `use const`-imports something aliased to a modeled `PREG_*` flag
    /// constant (issue #168), mirroring [`Self::php_version_id_aliased`].
    preg_flag_const_aliased: bool,
    /// Every `foreach` statement, lowered to its transform-relevant shape (ADR-0076). Read
    /// only by the loop→`array_map` transform.
    foreach_sites: Vec<ForeachSite>,
    /// Every literal array expression in the file (`[...]`/legacy `array(...)`), in source
    /// order (issue #187), read by the `array.duplicate-key` per-file pass.
    array_literal_sites: Vec<ArrayLiteralSite>,
    // invalid operands (ADR-0078, issue #191)
    /// Every arithmetic/bitwise/shift operator application, in source order, both operands
    /// lowered; read only by the `type.invalid-operand` judge.
    operand_sites: Vec<OperandSite>,
    // end invalid operands (ADR-0078, issue #191)
    /// Class references at the positions that break at run time (ADR-0049 §5 / S4,
    /// widened by issue #182), read by the `class.undefined` per-file pass.
    hard_class_refs: Vec<NameRef>,
    // member absence (ADR-0078, issue #197)
    /// Every property name written anywhere in this file, and whether any write
    /// went through a computed name. See [`SourceTree::property_write_names`].
    property_writes: PropertyWrites,
    // end member absence (ADR-0078, issue #197)
    /// Every global constant declaration in the file (ADR-0078, issue #198) — `const FOO`
    /// outside a class-like, and literal-named `define('FOO', …)`. The project-index leg of
    /// the `constant.undefined` ladder.
    global_const_decls: Vec<GlobalConstDecl>,
    /// Every bare constant fetch (`FOO`, `\FOO`, `Ns\FOO`), in source order (ADR-0078,
    /// issue #198), read by the `constant.undefined` per-file pass. `X::CONST` is a class
    /// constant (issue #197, different namespace) and never appears here, nor do
    /// `true`/`false`/`null`/the magic `__LINE__` family.
    const_refs: Vec<NameRef>,
    parse_errors: Vec<ParseError>,
    // unset pseudo-type (ADR-0087 §4, issue #396)
    /// The `phpdoc.maybe-undefined` candidate reads of the top-level script scope.
    unset_seed_facts: UnsetSeedFacts,
    // end unset pseudo-type (ADR-0087 §4, issue #396)
    /// The comment trivia in the file, in source order (ADR-0023 inline ignores).
    comments: Vec<Comment>,
    /// The namespace contexts of the file; index 0 is always the global context.
    contexts: Vec<NsCtx>,
    /// One `(start, end, ctx_index)` per namespace declaration, mapping a byte offset to
    /// its enclosing namespace context; offsets outside any fall back to global (index 0).
    regions: Vec<(u32, u32, usize)>,
    /// Byte offset of the start of each line (index 0 == line 1).
    line_starts: Vec<u32>,
    text: String,
}

impl SourceTree {
    /// Parse PHP source into the lowered tree. Never panics: parse errors are
    /// recovered and reported via [`SourceTree::parse_errors`].
    #[must_use]
    pub fn parse(source: &str) -> Self {
        // The lowering walkers recurse once per CST node, so expression depth is a stack
        // cost. Headroom is bought at the entry point where possible (issue #246, guard off);
        // the wasm playground (fixed-size shadow stack) keeps the guard, appending a refusal
        // to `parse_errors` instead of overflowing.
        let guard = stack_guard::Scope::enter();
        let arena = LocalArena::new();
        let file_id = FileId::new(b"<steins>");
        let program = mago_syntax::parser::parse_file_content(&arena, file_id, source.as_bytes());

        // File-level `use` imports binding `Steins\Pure`/`Steins\Effect` to a local name, so
        // `#[Pure]`/aliased `#[P]`/`#[Effect(...)]` attributes are recognized.
        let aliases = collect_steins_aliases(&Node::Program(program));

        // Namespace contexts (name + `use` imports) and their byte regions, so every
        // declaration/reference resolves in the right scope.
        let (contexts, regions) = build_contexts(program);

        // Docblock index: every `/** … */` trivium, so a declaration can adopt the one
        // immediately preceding it (whitespace-only gap; ADR-0029).
        let docs = DocIndex::build(source, program);

        // Object type hints (ADR-0043) resolve to their namespace FQN at lowering,
        // like declaration names; the resolver carries the file's ns contexts.
        let rc = RefResolver { contexts: &contexts, regions: &regions };

        let mut lowered = Lowered::default();
        walk(&Node::Program(program), &aliases, &docs, &rc, false, false, &mut lowered);

        let mut classes = lower_classes(&Node::Program(program), &aliases, &docs, &rc);
        let scopes = lower_scopes(program, &contexts, &regions, &docs);

        // Every `foreach`, lowered to its transform-relevant shape (ADR-0076 §4: candidate
        // domain is the whole construct family). The file is the outermost variable scope.
        let mut foreach_sites = Vec::new();
        collect_foreach_sites(
            &Node::Program(program),
            source.len().try_into().unwrap_or(u32::MAX),
            &mut foreach_sites,
        );

        // Every literal array expression, in source order (issue #187): purely syntactic
        // keys-and-spans, the `array.duplicate-key` check's whole evidence.
        let mut array_literal_sites = Vec::new();
        collect_array_literal_sites(&Node::Program(program), &mut array_literal_sites);

        // Every arithmetic/bitwise/shift operator application (ADR-0078, issue #191), both
        // operands lowered; file scope has no enclosing function-like body.
        let mut operand_sites = Vec::new();
        collect_operand_sites(&Node::Program(program), None, &mut operand_sites);

        // Comment trivia (ADR-0023 inline ignores): whitespace trivia is dropped;
        // every comment shape is kept with its raw spelling and span.
        let comments: Vec<Comment> = program.trivia.iter().filter_map(lower_comment).collect();

        // The `unset` pseudo-type's candidate reads (ADR-0087 §4): computed here rather
        // than handed in, because the CST does not outlive this function and the caller
        // that can lower a phpdoc type only exists afterwards. Gated on the word
        // appearing in a docblock at all, so nearly every file pays one substring scan.
        let unset_seed_facts = {
            let mut top: Vec<&Statement<'_>> = Vec::new();
            for s in program.statements.iter() {
                flatten_top_level(s, &mut top);
            }
            unset_seed_facts(&top, source, &comments)
        };

        // Fill the lowercase-normalized FQN on every declaration from the context
        // that encloses its name.
        for f in &mut lowered.functions {
            f.fqn = fqn_of(ctx_of(&contexts, &regions, f.span.start), &f.name);
        }
        for c in &mut classes {
            let ctx = ctx_of(&contexts, &regions, c.span.start);
            c.fqn = fqn_of(ctx, &c.name);
            // ADR-0043 amendment: resolve any recorded `self`/`static`/`parent` return
            // keyword to its bound, synthesizing the method's `ret` as a single-member
            // `Instance` of it. `self`/`static` bind to the enclosing class (minimum-bound
            // lemma); `parent` binds to the resolved `extends` parent (skipped if none). The
            // source-cased display renders the bound class in the diagnostic; the lowercased
            // FQN is the is-a key.
            let self_display = if ctx.namespace.is_empty() {
                c.name.clone()
            } else {
                format!("{}\\{}", ctx.namespace, c.name)
            };
            // Source-cased, namespace-qualified FQN for diagnostic/dump rendering (no
            // leading `\`, matching PHPStan) — same construction the self/static bound uses below.
            c.display = self_display.clone();
            let self_fqn = c.fqn.clone();
            let parent_bound = c.parent.as_ref().map(|p| {
                let display = resolve_class_ref(ctx_of(&contexts, &regions, p.offset), p);
                (display.to_ascii_lowercase(), display)
            });
            for m in &mut c.methods {
                let Some(kw) = m.ret_bound_keyword else { continue };
                let bound = match kw.kind {
                    RetBoundKind::SelfKw | RetBoundKind::Static => {
                        Some((self_fqn.clone(), self_display.clone()))
                    }
                    RetBoundKind::Parent => parent_bound.clone(),
                };
                if let Some((fqn, display)) = bound {
                    m.ret = Some(NativeType {
                        members: vec![TypeMember::Instance { fqn, display }],
                        nullable: kw.nullable,
                    });
                }
            }
        }

        let mut parse_errors: Vec<ParseError> = program
            .errors
            .iter()
            .map(|e| ParseError { message: e.to_string(), span: to_span(e.span()) })
            .collect();

        // A refused walk is a recovered parse error like any other: the checker names it
        // `syntax.unparsable` (ADR-0079, the vocabulary Mago's own `MAX_RECURSION_DEPTH`
        // already uses) and dams the file's other findings. Carries no line (see
        // `stack_guard::REFUSAL`), so it goes first.
        if guard.tripped() {
            let span = Span { start: 0, end: 0 };
            parse_errors.insert(0, ParseError { message: stack_guard::REFUSAL.to_owned(), span });
        }
        drop(guard);

        Self {
            strict_types: lowered.strict_types,
            functions: lowered.functions,
            classes,
            calls: lowered.calls,
            scopes,
            dynamism: lowered.dynamism,
            class_alias_edges: lowered.class_alias_edges,
            anon_class_edges: lowered.anon_class_edges,
            reflection: lowered.reflection,
            php_version_id_declared: lowered.php_version_id_declared,
            php_version_id_aliased: lowered.php_version_id_aliased,
            preg_flag_const_declared: lowered.preg_flag_const_declared,
            preg_flag_const_aliased: lowered.preg_flag_const_aliased,
            foreach_sites,
            array_literal_sites,
            operand_sites,
            hard_class_refs: lowered.hard_class_refs,
            // member absence (ADR-0078, issue #197)
            property_writes: lowered.property_writes,
            // end member absence (ADR-0078, issue #197)
            global_const_decls: lowered.global_const_decls,
            const_refs: lowered.const_refs,
            parse_errors,
            unset_seed_facts,
            comments,
            contexts,
            regions,
            line_starts: line_starts(source),
            text: source.to_owned(),
        }
    }

    /// The namespace context enclosing `offset` (its namespace name and the
    /// `use` imports in scope), for whole-project name resolution.
    #[must_use]
    pub fn ctx_at(&self, offset: u32) -> &NsCtx {
        ctx_of(&self.contexts, &self.regions, offset)
    }

    /// Resolve a class reference to its FQN (case preserved, no leading `\`), applying PHP
    /// class-name resolution: fully-qualified passes through; qualified/unqualified applies
    /// `use` imports on the first segment, else prepends the namespace. No global fallback
    /// (unlike functions), so this is pure syntax — no project index needed. Callers fold case at lookup.
    #[must_use]
    pub fn resolve_class_fqn(&self, r: &NameRef) -> String {
        resolve_class_ref(self.ctx_at(r.offset), r)
    }

    /// Whether the file begins with `declare(strict_types=1)`.
    #[must_use]
    pub const fn has_strict_types(&self) -> bool {
        self.strict_types
    }

    /// The user-defined function declarations found in the file.
    #[must_use]
    pub fn functions(&self) -> &[FunctionDecl] {
        &self.functions
    }

    /// The user-defined class declarations found in the file (interfaces,
    /// traits, and enums are not lowered here).
    #[must_use]
    pub fn classes(&self) -> &[ClassDecl] {
        &self.classes
    }

    /// The function-call expressions found in the file.
    #[must_use]
    pub fn calls(&self) -> &[CallExpr] {
        &self.calls
    }

    /// The analysis scopes (top-level script + one per function body), each with
    /// its linear trace IR and poison flag (ADR-0001 value propagation).
    #[must_use]
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// The dynamic-code constructs (`eval`/`include`/`require`) found file-wide (ADR-0046
    /// §2) — the caller-enumeration obstacles the transform engine consults before claiming
    /// "all callers proven".
    #[must_use]
    pub fn dynamism_sites(&self) -> &[DynamismSite] {
        &self.dynamism
    }

    /// The compile-time `class_alias('Target', 'Alias')` edges found file-wide (ADR-0049
    /// §2), both names given as literals or `X::class`. Folded into the project index for
    /// existence resolution; a runtime-minted alias is a [`DynamismKind::ClassAlias`] dam site instead.
    #[must_use]
    pub fn class_alias_edges(&self) -> &[ClassAliasEdge] {
        &self.class_alias_edges
    }

    /// The anonymous-class inheritance edges found file-wide (ADR-0049 A4). Read by the
    /// declared-receiver lane's descendant closure (S6) to detect an invisible descendant of
    /// a union member (an anon class is never in the class index). Class references at
    /// positions verified to break at run time (ADR-0049 §5/S4, widened by issue #182):
    /// hard-error expressions (`new X`, `X::m()`, `X::CONST`, `X::$prop`), inheritance
    /// (`extends`/`implements`/`use <Trait>`), `catch (X $e)`, and parameter/return/property
    /// native type declarations. Consumed by `class.undefined`; `self`/`static`/`parent`,
    /// dynamic classes, `X::class`, `instanceof`, and docblock positions are excluded.
    #[must_use]
    pub fn hard_class_refs(&self) -> &[NameRef] {
        &self.hard_class_refs
    }

    /// Every global constant declaration the file makes (ADR-0078, issue #198): `const FOO`
    /// outside a class-like (resolved against its namespace) and literal-named `define()`
    /// (always absolute). Folded into the project index as the textual half of the
    /// `constant.undefined` evidence. Conditionality isn't recorded: `if (!defined('X'))
    /// define('X', …)` declares `X` for absence purposes exactly like an unconditional `define`.
    #[must_use]
    pub fn global_const_decls(&self) -> &[GlobalConstDecl] {
        &self.global_const_decls
    }

    /// Every bare constant fetch (`FOO`, `\FOO`, `Ns\FOO`), in source order (ADR-0078, issue
    /// #198) — the finding-position set of `constant.undefined`, exactly as
    /// [`Self::hard_class_refs`] is for `class.undefined`. `X::CONST` (issue #197's
    /// namespace), `true`/`false`/`null`, and `__LINE__`-family are excluded at collection.
    #[must_use]
    pub fn const_refs(&self) -> &[NameRef] {
        &self.const_refs
    }

    /// Every `foreach` statement, in source order, lowered to the shape the loop→
    /// `array_map` transform enumerates (ADR-0076 §4). Purely syntactic.
    #[must_use]
    pub fn foreach_sites(&self) -> &[ForeachSite] {
        &self.foreach_sites
    }

    /// Every literal array expression, in source order (issue #187). Purely syntactic — the
    /// whole evidence the `array.duplicate-key` per-file pass reads.
    #[must_use]
    pub fn array_literal_sites(&self) -> &[ArrayLiteralSite] {
        &self.array_literal_sites
    }

    // member absence (ADR-0078, issue #197)
    /// Every property name this file writes, deduplicated, in source order (ADR-0078, issue
    /// #197). Purely syntactic — no receiver resolved — read project-wide as the
    /// dynamic-property obstacle for `property.undefined`.
    ///
    /// Over-approximation is the point: a write creates a dynamic property, so a class
    /// declaring nothing named `p` can still answer `$o->p` at runtime if another file did
    /// `$o->p = 1` first (deprecated but not an error since PHP 8.2, witnessed clean at
    /// 8.5.9). Resolving which object each write lands on is deferred
    /// (`property.dynamic-write`); this obstacle only costs absence claims for names assigned
    /// somewhere, so a typo like `$user->emial` (written nowhere) survives.
    #[must_use]
    pub fn property_write_names(&self) -> &[String] {
        &self.property_writes.names
    }

    /// Whether this file writes a property through a computed name (`$o->$n = …`, ADR-0078,
    /// issue #197) — such a write can create any name, so one anywhere takes
    /// `property.undefined` off the surface entirely.
    #[must_use]
    pub fn writes_computed_property_name(&self) -> bool {
        self.property_writes.dynamic
    }
    // end member absence (ADR-0078, issue #197)

    // invalid operands (ADR-0078, issue #191)
    /// Every arithmetic/bitwise/shift operator application, in source order (ADR-0078,
    /// issue #191) — ordered by span start for binary search. Operands are lowered, never resolved.
    #[must_use]
    pub fn operand_sites(&self) -> &[OperandSite] {
        &self.operand_sites
    }
    // end invalid operands (ADR-0078, issue #191)

    #[must_use]
    pub fn anonymous_class_edges(&self) -> &[AnonClassEdge] {
        &self.anon_class_edges
    }

    /// The reflection-driven invocation sites found file-wide (issue #30). Poison no scope,
    /// dam no claim — inventoried so a quiet run can say what it declined to follow (a guess; see [`ReflectionKind`]).
    #[must_use]
    pub fn reflection_sites(&self) -> &[ReflectionSite] {
        &self.reflection
    }

    /// Whether the file contains any `eval(...)` construct.
    #[must_use]
    pub fn contains_eval(&self) -> bool {
        self.dynamism.iter().any(|d| matches!(d.kind, DynamismKind::Eval))
    }

    /// The recovered parse errors.
    #[must_use]
    pub fn parse_errors(&self) -> &[ParseError] {
        &self.parse_errors
    }

    /// Whether this file declares a userland constant named `PHP_VERSION_ID`
    /// (issue #29) — see the field docs for the project-wide consequence.
    #[must_use]
    pub fn php_version_id_declared(&self) -> bool {
        self.php_version_id_declared
    }

    /// Whether this file `use const`-imports the alias `PHP_VERSION_ID`
    /// (issue #29) — file-scoped; an unqualified reference here is the import.
    #[must_use]
    pub fn php_version_id_aliased(&self) -> bool {
        self.php_version_id_aliased
    }

    /// Whether this file declares a userland twin of a modeled `PREG_*` flag
    /// constant (issue #168) — see the field docs for the project-wide consequence.
    #[must_use]
    pub fn preg_flag_const_declared(&self) -> bool {
        self.preg_flag_const_declared
    }

    /// Whether this file `use const`-imports the alias of a modeled `PREG_*` flag constant
    /// (issue #168) — file-scoped; an unqualified reference here is the import.
    #[must_use]
    pub fn preg_flag_const_aliased(&self) -> bool {
        self.preg_flag_const_aliased
    }

    /// The comment trivia found in the file, in source order (ADR-0023 inline
    /// `@steins-ignore` channel). Whitespace trivia is not included.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// The `/** … */` docblock immediately preceding `stmt_start` — only whitespace between
    /// its end and `stmt_start` — or `None`. The statement-level analogue of the declaration
    /// adoption rule (ADR-0029), consumed by inline-`@var` cast seeding (ADR-0073). A non-doc
    /// comment in the gap breaks the adjacency, exactly as any code would.
    #[must_use]
    pub fn stmt_docblock(&self, stmt_start: u32) -> Option<&Comment> {
        docblock_before(&self.comments, &self.text, stmt_start)
    }

    /// The candidate reads of the `unset` pseudo-type idiom (ADR-0087 §4, issue #396) —
    /// see [`UnsetSeedFacts`] for why they are candidates and what confirms them.
    #[must_use]
    pub fn unset_seed_facts(&self) -> &UnsetSeedFacts {
        &self.unset_seed_facts
    }

    // untyped surface (ADR-0078, issue #200)
    /// The exact source text a file byte [`Span`] covers, or `None` when the span is out of
    /// range or doesn't land on `char` boundaries.
    ///
    /// The lowered tree records spans, not spellings, for anything whose spelling isn't
    /// itself a modeled fact; this turns such a span back into text — used by the
    /// declaration-reading `untyped.*` family to tell an `array` hint from an `int` one,
    /// which no lowered [`NativeType`] can express (both model to `None`/`Some` for unrelated reasons).
    #[must_use]
    pub fn source_slice(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }
    // end untyped surface (ADR-0078, issue #200)

    /// Whether a docblock trivium ending at `doc_end` has nothing an adoption rule could
    /// attach it to — the negative side of [`Self::stmt_docblock`]'s grammar (issue #186,
    /// `phpdoc.misplaced-var`).
    ///
    /// Answered from the text, not the lowered trace: a statement inside a construct the
    /// trace keeps opaque (loop body, `try`, `switch` arm) has no [`Stmt`] to query, so this
    /// instead asks whether any construct can follow at all — skipping whitespace from
    /// `doc_end` lands on EOF, a closing `}`, or another comment. A `?>` close tag is
    /// deliberately not a proof: `<?php /** @var View $v */ ?>` is a legal annotation, not
    /// rot. `true` proves non-adoption; `false` only means "something follows".
    #[must_use]
    pub fn docblock_adopts_nothing(&self, doc_end: u32) -> bool {
        let Some(rest) = self.text.get(doc_end as usize..) else { return true };
        let rest = rest.trim_start();
        rest.is_empty() || rest.starts_with('}') || rest.starts_with("/*")
    }

    /// Whether `$name` occurs anywhere in the file before `offset` — a deliberately crude
    /// textual probe, used by `phpdoc.stale-var` to ask whether a named variable plausibly
    /// exists at all (issue #186). Counts every occurrence alike (parameter, assignment
    /// target, `use` capture, `foreach` binding, plain read, even a docblock mention) since
    /// the question is existence, not liveness; the window is the whole file prefix, a
    /// superset where every over-match only produces more silence. Match is token-exact at
    /// both ends: `$ec` doesn't match `$echo`, `$$echo` doesn't match `$echo`.
    #[must_use]
    pub fn variable_mentioned_before(&self, name: &str, offset: u32) -> bool {
        if name.is_empty() {
            return false;
        }
        let Some(prefix) = self.text.get(..offset as usize) else { return false };
        let bytes = prefix.as_bytes();
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let needle = format!("${name}");
        let mut from = 0usize;
        while let Some(rel) = prefix[from..].find(&needle) {
            let at = from + rel;
            let after = at + needle.len();
            let before_ok = at == 0 || bytes[at - 1] != b'$';
            let after_ok = bytes.get(after).is_none_or(|&b| !ident(b));
            if before_ok && after_ok {
                return true;
            }
            from = at + 1;
        }
        false
    }

    /// Whether everything on `offset`'s line before `offset` is whitespace — the token at
    /// `offset` is its line's first non-whitespace. Drives `@steins-ignore` placement
    /// (ADR-0023): a leading comment suppresses the next line, a trailing one its own.
    #[must_use]
    pub fn is_line_leading(&self, offset: u32) -> bool {
        let line_idx = self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let end = (offset as usize).min(self.text.len());
        self.text.get(line_start..end).is_none_or(|s| s.trim().is_empty())
    }

    /// Resolve a byte offset to a 1-based line/column (column counted in
    /// Unicode scalar values).
    #[must_use]
    pub fn position(&self, offset: u32) -> Position {
        let line_idx = self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let end = (offset as usize).min(self.text.len());
        let column = self.text.get(line_start..end).map_or(0, |s| s.chars().count());
        Position { line: line_idx as u32 + 1, column: column as u32 + 1 }
    }

    /// The source text a span covers, or `None` when out of bounds or off a character
    /// boundary. The one way to read the file's own words back out of the tree; its first
    /// consumer, `type.return-missing` (issue #199), quotes a declared return type as
    /// written (`: array`/`: mixed`/`: self` all lower to no [`NativeType`], yet PHP's
    /// `TypeError` does name them).
    #[must_use]
    pub fn text_at(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }

    /// Widen a statement `span` to its whole physical line(s) when nothing else shares them:
    /// with only whitespace before `span.start` and after `span.end` on their lines, the
    /// returned span starts at the line start and swallows the trailing newline (CRLF
    /// included), so deleting it leaves no blank gutter line (steins-edit's docblock tag
    /// deletion discipline). A span sharing a line with anything else comes back unchanged,
    /// so a deletion removes only the statement.
    #[must_use]
    pub fn whole_line_span(&self, span: Span) -> Span {
        let bytes = self.text.as_bytes();
        let line_idx = self.line_starts.partition_point(|&s| s <= span.start).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let leading_blank = self
            .text
            .get(line_start..span.start as usize)
            .is_some_and(|s| s.chars().all(char::is_whitespace));
        if !leading_blank {
            return span;
        }
        // Skip horizontal whitespace (and a CR) after the span, then require the
        // line to actually end there — at a newline, or at end of file.
        let mut end = span.end as usize;
        while bytes.get(end).is_some_and(|&b| b == b' ' || b == b'\t' || b == b'\r') {
            end += 1;
        }
        match bytes.get(end) {
            Some(&b'\n') => Span { start: line_start as u32, end: (end + 1) as u32 },
            None => Span { start: line_start as u32, end: end as u32 },
            Some(_) => span,
        }
    }
}

#[derive(Default)]
pub(crate) struct Lowered {
    pub(crate) strict_types: bool,
    pub(crate) functions: Vec<FunctionDecl>,
    pub(crate) calls: Vec<CallExpr>,
    pub(crate) dynamism: Vec<DynamismSite>,
    pub(crate) class_alias_edges: Vec<ClassAliasEdge>,
    pub(crate) anon_class_edges: Vec<AnonClassEdge>,
    /// Reflection-driven invocation sites (issue #30) — report-only.
    pub(crate) reflection: Vec<ReflectionSite>,
    /// Issue #29: see [`SourceTree::php_version_id_declared`].
    pub(crate) php_version_id_declared: bool,
    /// Issue #29: see [`SourceTree::php_version_id_aliased`].
    pub(crate) php_version_id_aliased: bool,
    /// Issue #168: see [`SourceTree::preg_flag_const_declared`].
    pub(crate) preg_flag_const_declared: bool,
    /// Issue #168: see [`SourceTree::preg_flag_const_aliased`].
    pub(crate) preg_flag_const_aliased: bool,
    /// Issue #182 / ADR-0049 §5/S4: see [`SourceTree::hard_class_refs`].
    pub(crate) hard_class_refs: Vec<NameRef>,
    // member absence (ADR-0078, issue #197)
    pub(crate) property_writes: PropertyWrites,
    // end member absence (ADR-0078, issue #197)
    pub(crate) global_const_decls: Vec<GlobalConstDecl>,
    pub(crate) const_refs: Vec<NameRef>,
}

// member absence (ADR-0078, issue #197)
/// Every property name a file writes, plus whether it writes one under a runtime-computed
/// name (ADR-0078, issue #197) — storage behind [`SourceTree::property_write_names`] /
/// [`SourceTree::writes_computed_property_name`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct PropertyWrites {
    /// The written names, deduplicated, as written (property names are case-sensitive in PHP).
    pub(crate) names: Vec<String>,
    /// `true` when the file writes a property through a runtime-computed name (`$o->$n = …`).
    /// Such a write can create any name, so one anywhere takes the whole id off the surface.
    pub(crate) dynamic: bool,
}

impl PropertyWrites {
    /// Record a property-write lvalue. A `$o->p`/`$this->p`/`$a->b->c` target contributes
    /// its last name; a computed selector sets [`Self::dynamic`]; anything else contributes nothing.
    pub(crate) fn push_lvalue(&mut self, lvalue: &Expression<'_>) {
        let Expression::Access(Access::Property(pa)) = lvalue.unparenthesized() else {
            return;
        };
        match method_name_of(&pa.property) {
            Some(name) => {
                if !self.names.contains(&name) {
                    self.names.push(name);
                }
            }
            None => self.dynamic = true,
        }
    }
}
// end member absence (ADR-0078, issue #197)
