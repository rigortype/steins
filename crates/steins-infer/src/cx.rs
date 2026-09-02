//! The project-aware analysis context: [`Cx`] carries the units, the index, the
//! dam and the runtime postures, and its methods are the shared resolution surface
//! — name / class / method resolution, literal folding through the [`Folder`], and
//! the diagnostic builders.
//!
//! [`Folder`]: crate::Folder

use std::collections::{HashMap, HashSet};

use steins_contract::normalize::FinalKeyword;
use steins_domain::{Certainty, Fact, PhpStr, Val};
use steins_phpdoc::Type as PType;
use steins_phpdoc::ast::TypeKind as PKind;
use steins_syntax::{
    ArgValue, CallExpr, ClassDecl, FunctionDecl, MethodDecl, NameRef, NativeType, Param,
    PropertyDecl, RefKind, Scope, ScopeOwner, SourceTree, StmtKind, ValueOp,
};

use crate::fold::Folder;
use crate::fold_args::{UNION_FOLD_COMBINATION_CAP, UNION_FOLD_MEMBER_CAP, concat_cast, is_fold_arg};
use crate::{ID, RETURN_ID, Sym};
use crate::arg_check::render_call;
use crate::builtin_returns::shape_builtin_return_fact;
use crate::cond::{eval_cmp, spaceship_pole};
use crate::contract::{
    CArg, CVal, Envelopes, GenericCarry, InheritanceEdge, IsA, TemplateShadow, template_names_of,
};
use crate::dam::DamFacts;
use crate::descent::nested_call_singleton;
use crate::env::{Descent, Known, Store, Stratum, arg_of_val, val_of};
use crate::project::{Diagnostic, FileUnit, FnResolution, Index, Res, Site};
use crate::purity::PurityOracle;
use crate::return_arms::class_template_names;
use crate::walk::value_stratum;

// ---------------------------------------------------------------------------
// The project-aware analysis context.
// ---------------------------------------------------------------------------

/// Read-only analysis context: the whole project view plus the file currently
/// being analyzed. Cheap to copy (all borrows); descent rebuilds it at the
/// callee's file via [`Cx::at`].
/// The shared empty dam fact for the auxiliary passes (effects/throws/const
/// resolution) that never emit an absence id and so never read the dam. The main
/// analysis pass ([`check_units`]) computes the real whole-universe fact and hands
/// it to [`Cx::new_with`]; these passes use [`Cx::new`], which points here.
///
/// [`check_units`]: crate::check_units
pub(crate) static EMPTY_DAM: std::sync::LazyLock<DamFacts> = std::sync::LazyLock::new(DamFacts::default);

#[derive(Clone, Copy)]
pub(crate) struct Cx<'a> {
    pub(crate) units: &'a [FileUnit<'a>],
    pub(crate) index: &'a Index,
    pub(crate) cur: usize,
    /// The whole-universe runtime-definition dam fact (ADR-0049 §2), a per-run query
    /// answer (ADR-0048). Read by the absence family's conditional-declaration leg
    /// (A2i): a chain containing a conditional declaration re-dams the claim, so it
    /// fires only when the dam is clear. The auxiliary passes point at [`EMPTY_DAM`].
    pub(crate) dam: &'a DamFacts,
    /// The `[runtime] warning-handler` pseudo-constant (ADR-0049 §7 amendment,
    /// ADR-0037 §2 family). `true` = `"abort"` (the owner-confirmed realistic-app
    /// default: a warning handler converts an `E_WARNING` to an exception/halts, so
    /// a *proven* warning is a proven runtime break — warning-grade offset findings
    /// emit). `false` = `"null"`: the application tolerates the warning and continues,
    /// so warning-grade offset findings stay silent (v1 simplification: the
    /// ADR-0050 layer-demotion + value-side `null`/`""` adoption is deferred). The
    /// Error-grade `offset.on-unsupported` object case (not yet implemented) is
    /// posture-independent and would emit under both.
    pub(crate) warning_handler_abort: bool,
    /// The `[runtime] final-keyword` pseudo-constant (issue #234, ADR-0037 §2
    /// family) — what the runtime this project is analyzed for does with `final`.
    /// Read by the declared-receiver lane's intersection leg (issue #238) through
    /// [`steins_contract::normalize::provably_uninhabited`], and by nothing else:
    /// the posture governs *inhabitance*, never a `final` diagnostic (#234's own
    /// out-of-scope list). [`FinalKeyword::Enforced`] is the absence default, so a
    /// project declaring nothing gets the language's own rule.
    pub(crate) final_keyword: FinalKeyword,
    /// The **effective analysis minor** for version-keyed value rules (issue
    /// #28): the target floor when the project declares a target whose range
    /// agrees on the ADR-0049 A12 next-int boundary, `None` when the declared
    /// range straddles it (a boundary-sensitive literal must then decline —
    /// A12's unknown leg, generalized to a range), and the sidecar's runtime
    /// minor when the project declares nothing (the pre-#28 behavior).
    /// Computed once per run by [`effective_php_view`].
    ///
    /// [`effective_php_view`]: crate::effective_php_view
    pub(crate) php_minor: Option<(u16, u16)>,
    /// Whether a catalog-backed is-a verdict used for **arm deletion** must be
    /// demoted to `Unknown` (ADR-0052 A11): some version the analysis is about
    /// — any minor of the declared target range, else the runtime minor — is
    /// not the catalog pin. Computed once per run by [`effective_php_view`].
    ///
    /// [`effective_php_view`]: crate::effective_php_view
    pub(crate) catalog_skew: bool,
    /// The `PHP_VERSION_ID` interval the analysis is about (issue #29), for the
    /// version-guard fold — already `None` when a userland constant of that
    /// name is declared anywhere in the project. See [`PhpView::version_id`].
    ///
    /// [`PhpView::version_id`]: crate::PhpView::version_id
    pub(crate) version_id: Option<(u32, Option<u32>)>,
    /// The whole-project purity answer for the callable-purity obligation
    /// (ADR-0063 P3), or `None` when no purity-bearing callable is spelled anywhere
    /// (and therefore also for every auxiliary pass, which reports no findings of
    /// this family). `None` makes [`Self::provably_impure`] answer `false`, i.e. the
    /// obligation stays silent — the safe side.
    pub(crate) purity: Option<&'a PurityOracle<'a>>,
    /// The PHP target range the project DECLARES, verbatim (issue #73). The
    /// version-keyed value rules read [`Self::php_minor`], which has already
    /// collapsed the range to one effective minor; the ADR-0069 floor's gate needs
    /// the range itself, because it asks a different question — whether the whole
    /// declared range lies at or above one builtin's change boundary. `None` is an
    /// undeclared target, which the floor admits.
    pub(crate) php_target: Option<&'a steins_db::PhpTarget>,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(units: &'a [FileUnit<'a>], index: &'a Index, cur: usize) -> Self {
        Self {
            units,
            index,
            cur,
            dam: &EMPTY_DAM,
            warning_handler_abort: true,
            final_keyword: FinalKeyword::Enforced,
            php_minor: None,
            catalog_skew: false,
            version_id: None,
            purity: None,
            php_target: None,
        }
    }

    /// A context carrying an explicit runtime config (the top-level analysis pass).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with(
        units: &'a [FileUnit<'a>],
        index: &'a Index,
        cur: usize,
        dam: &'a DamFacts,
        warning_handler_abort: bool,
        final_keyword: FinalKeyword,
        php_minor: Option<(u16, u16)>,
        catalog_skew: bool,
        version_id: Option<(u32, Option<u32>)>,
        purity: Option<&'a PurityOracle<'a>>,
        php_target: Option<&'a steins_db::PhpTarget>,
    ) -> Self {
        Self {
            units,
            index,
            cur,
            dam,
            warning_handler_abort,
            final_keyword,
            php_minor,
            catalog_skew,
            version_id,
            purity,
            php_target,
        }
    }

    /// A context pointing at a different file (for cross-file descent); the runtime
    /// config and dam fact are whole-run properties and are inherited unchanged.
    pub(crate) fn at(&self, file: usize) -> Cx<'a> {
        Cx {
            units: self.units,
            index: self.index,
            cur: file,
            dam: self.dam,
            warning_handler_abort: self.warning_handler_abort,
            final_keyword: self.final_keyword,
            php_minor: self.php_minor,
            catalog_skew: self.catalog_skew,
            version_id: self.version_id,
            purity: self.purity,
            php_target: self.php_target,
        }
    }

    /// Whether a callable symbol is **provably** impure (ADR-0063 P3) — the one
    /// question the purity obligation asks. `false` whenever the oracle is absent,
    /// so an auxiliary pass and a project with no purity spelling both stay silent.
    pub(crate) fn provably_impure(&self, sym: &Sym) -> bool {
        self.purity.is_some_and(|p| p.provably_impure(sym))
    }

    /// Whether a **catalog-backed** is-a verdict used for arm deletion must be
    /// demoted to `Unknown` (ADR-0052 A11): the project PHP minor is known and
    /// differs from the catalog pin. When the minor is unknown or matches, catalog
    /// verdicts stand.
    pub(crate) fn a11_demote_catalog(&self) -> bool {
        self.catalog_skew
    }

    /// Whether the class-likes declared in `file` are **member-incomplete**
    /// (ADR-0079 §2.5): the file did not parse, so a recovery point may have
    /// swallowed methods out of a class body it otherwise kept. Asked by the
    /// chain-closure legs where the A14 magic-tag obstacle is also asked. Read off
    /// the dam's site list, so the ADR-0046 §2 vendor presumption applies here too,
    /// and an auxiliary-pass context ([`EMPTY_DAM`]) answers `false`.
    pub(crate) fn member_incomplete(&self, file: usize) -> bool {
        self.dam.file_is_unparsable(self.units[file].path)
    }

    pub(crate) fn tree(&self) -> &'a SourceTree {
        self.units[self.cur].tree
    }
    pub(crate) fn path(&self) -> &'a str {
        self.units[self.cur].path
    }
    pub(crate) fn strict(&self) -> bool {
        self.tree().has_strict_types()
    }

    pub(crate) fn fn_decl(&self, site: Site) -> &'a FunctionDecl {
        &self.units[site.file].tree.functions()[site.index]
    }
    pub(crate) fn class_decl(&self, site: Site) -> (usize, &'a ClassDecl) {
        (site.file, &self.units[site.file].tree.classes()[site.index])
    }

    /// Resolve a class reference (in the current file's context) to its FQN.
    pub(crate) fn class_fqn(&self, r: &NameRef) -> String {
        self.tree().resolve_class_fqn(r)
    }

    /// Whether `fqn` names **no** project class at all. Stricter than
    /// `find_class(..).is_none()`, which also answers "none" for an *ambiguous*
    /// name (two project declarations): that is a project class Steins merely
    /// cannot pick, not an absent one, and the builtin-class catalog must not
    /// speak over it (issue #67 precedence).
    pub(crate) fn class_absent(&self, fqn: &str) -> bool {
        matches!(self.index.resolve_class(fqn), Res::Absent)
    }

    /// Find a class by FQN (case-insensitive), returning its file and decl.
    pub(crate) fn find_class(&self, fqn: &str) -> Option<(usize, &'a ClassDecl)> {
        match self.index.resolve_class(fqn) {
            Res::Unique(site) => Some(self.class_decl(site)),
            _ => None,
        }
    }

    /// The source-cased, namespace-qualified display form of a class FQN (matching
    /// PHPStan, no leading `\`). A project class contributes its declared casing
    /// ([`ClassDecl::display`]); a name no project file declares
    /// ([`Cx::class_absent`], keeping issue #67 precedence) recovers the casing
    /// php-src declares ([`steins_catalog::builtin_class_display`]):
    /// `dumpType(gmp_init($x))` reads `GMP`, as PHPStan does. Unresolved falls back
    /// to the given key with any leading `\` stripped (ADR-0053 §7; builtin casing
    /// closes the ADR-0069 third-amendment residual).
    pub(crate) fn class_display_fqn(&self, fqn: &str) -> String {
        match self.find_class(fqn) {
            Some((_, cd)) if !cd.display.is_empty() => cd.display.clone(),
            _ => {
                if self.class_absent(fqn)
                    && let Some(declared) = steins_catalog::builtin_class_display(fqn)
                {
                    return declared.to_owned();
                }
                fqn.trim_start_matches('\\').to_owned()
            }
        }
    }

    /// The **complete** case set of the enum `fqn` declares, in declaration order
    /// (issue #429) — the one finite domain PHP's runtime enforces, so a Verified
    /// fact (ADR-0037) rather than a docblock claim.
    ///
    /// `None` is the absence discipline (ADR-0049), and it outranks coverage
    /// (ADR-0002): a domain this cannot complete must not exist at all, because
    /// everything downstream reads a finite domain as "these and no others". Four
    /// ways to answer `None`, each a different way of not knowing the whole set:
    ///
    /// * the name resolves to no class, or to more than one ([`Cx::find_class`]
    ///   answers only on `Res::Unique`) — there is no single declaration to read;
    /// * the declaration is not an enum;
    /// * it is **conditionally declared** ([`steins_syntax::ClassDecl::conditional`],
    ///   ADR-0049 A2i) — a sibling branch may declare a different case set under
    ///   the same name, and nothing here can tell which one runs;
    /// * its file did not parse ([`Cx::member_incomplete`], ADR-0079 §2.5) — a
    ///   recovery point may have swallowed a `case` out of the body, which is
    ///   exactly the shape that would turn a missing case into a false exhaustion.
    ///
    /// A case-less `enum E {}` also answers `None`: its domain is genuinely empty,
    /// and an empty seed is indistinguishable from a lane narrowed to nothing.
    /// No enum-typed declaration can hold a value anyway, so the coverage lost is
    /// a shape no program reaches.
    pub(crate) fn enum_case_names(&self, fqn: &str) -> Option<Vec<String>> {
        let (file, cd) = self.find_class(fqn)?;
        if !cd.is_enum || cd.conditional || self.member_incomplete(file) {
            return None;
        }
        let cases: Vec<String> = cd.enum_cases.iter().map(|c| c.name.clone()).collect();
        (!cases.is_empty()).then_some(cases)
    }

    /// Whether `class_fqn` can have **no subclass at all**: `final`, or an enum
    /// (implicitly final). A class the index cannot uniquely resolve answers
    /// `false` — an unseen declaration may extend it.
    ///
    /// The bit that turns a class *name* into a set of runtime values a proof may
    /// quantify over. [`Cx::object_is_type_error`] decides an object of an **exact**
    /// class, so a declared arm spelled `A` is only decidable through it where no
    /// `A` subclass exists to answer differently — a subclass may implement an
    /// interface the native type accepts (issue #537).
    pub(crate) fn class_has_no_subclass(&self, class_fqn: &str) -> bool {
        self.find_class(class_fqn).is_some_and(|(_, cd)| cd.is_final || cd.is_enum)
    }

    /// Whether a `$this` seeded from enclosing class `class_fqn` is provably the
    /// **exact** runtime class (audit G1). A `final` class or an enum has no
    /// subclass, so its `$this` is exact; any other project class is only a lower
    /// bound (some subclass instance may be running the method). A class the index
    /// cannot uniquely resolve is conservatively *not* exact.
    pub(crate) fn this_class_exact(&self, class_fqn: &str) -> bool {
        self.class_has_no_subclass(class_fqn)
    }

    /// The FQN of `class_fqn`'s parent, resolved in the parent's own file ctx.
    pub(crate) fn parent_fqn(&self, class_fqn: &str) -> Option<String> {
        let (file, cd) = self.find_class(class_fqn)?;
        let pref = cd.parent.as_ref()?;
        Some(self.units[file].tree.resolve_class_fqn(pref))
    }

    /// The non-static properties of `class_fqn` including inherited ones (ADR-0036),
    /// walking the parent chain; a derived-class declaration shadows an ancestor's
    /// property of the same name (first-seen wins, own class first). Static
    /// properties are excluded (never heap-tracked). Stops at an unknown/absent
    /// parent or a trait-using class (give up → the props gathered so far).
    pub(crate) fn class_props(&self, class_fqn: &str) -> Vec<&'a PropertyDecl> {
        let mut out: Vec<&'a PropertyDecl> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = class_fqn.to_owned();
        let mut chain_seen: HashSet<String> = HashSet::new();
        loop {
            if !chain_seen.insert(cur.to_ascii_lowercase()) {
                break;
            }
            let Some((file, cd)) = self.find_class(&cur) else { break };
            for p in &cd.properties {
                if p.is_static {
                    continue;
                }
                if seen.insert(p.name.to_ascii_lowercase()) {
                    out.push(p);
                }
            }
            match &cd.parent {
                Some(pref) => cur = self.units[file].tree.resolve_class_fqn(pref),
                None => break,
            }
        }
        out
    }

    /// Whether `prop` is a **class-body hooked** property anywhere in `class_fqn`'s
    /// chain (FP class 16). Those declarations are dropped at lowering — only their
    /// names survive, in [`ClassDecl::hooked_properties`] — so [`Self::class_props`]
    /// cannot answer this and a write to one would otherwise look like a write to an
    /// undeclared slot and record a fact. A hook is arbitrary user code: the stored
    /// value is whatever the `set` hook computes, never the written one. The promoted
    /// spelling stays on the surface and carries `PropertyDecl::hooked` instead.
    pub(crate) fn class_body_hooked(&self, class_fqn: &str, prop: &str) -> bool {
        let mut cur = class_fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(cur.to_ascii_lowercase()) {
                return false;
            }
            let Some((file, cd)) = self.find_class(&cur) else { return false };
            if cd.hooked_properties.iter().any(|h| h == prop) {
                return true;
            }
            match &cd.parent {
                Some(pref) => cur = self.units[file].tree.resolve_class_fqn(pref),
                None => return false,
            }
        }
    }

    /// The `__construct` method resolved through `class_fqn`'s chain (ADR-0036),
    /// for mapping `new` args to promoted-property positions — **with the file that
    /// declared it**, like every other class-member lookup in this crate.
    ///
    /// The file is what a docblock read needs to mean anything (issue #374): an
    /// inherited constructor's `@param` is written against *its own* file's
    /// namespace and `use` scope, and a class reference in it names a different
    /// class — or no class — read anywhere else.
    pub(crate) fn find_ctor(&self, class_fqn: &str) -> Option<(usize, &'a MethodDecl)> {
        let mut cur = class_fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(cur.to_ascii_lowercase()) {
                return None;
            }
            let (file, cd) = self.find_class(&cur)?;
            if let Some(m) = cd.methods.iter().find(|m| m.is_constructor) {
                return Some((file, m));
            }
            cur = self.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
        }
    }

    /// Infer the class-level generic type-argument VALUES a `new Class(args)`
    /// expression carries (ADR-0032 tier 1 propagation feeding tier 3 carry,
    /// issue #10). For each class-level `@template` (declaration order) that binds
    /// to a DIRECT top-level `@param T $p` constructor parameter, the matching
    /// positional argument's resolved value becomes that template's carried value.
    ///
    /// Deliberately not a solver (ADR-0030/0032 "won't build"): only a *bare*
    /// `@param T` occurrence binds a template; a nested/compound occurrence
    /// (`@param array<T>`, `@param T|null`) does not. All-or-nothing: one carried
    /// value per template only when EVERY template resolves at an aligned
    /// positional argument; any gap returns EMPTY, so downstream acceptance answers
    /// `Maybe` rather than a manufactured `No`.
    ///
    /// ADR-0048: pure function of the already-seeded `new` argument trace, no scope
    /// entry state, no global-ordering dependence.
    pub(crate) fn infer_generic_args(
        &self,
        class_fqn: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        store: &Store,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Vec<CVal> {
        let empty = Vec::new();
        let Some((_, cd)) = self.find_class(class_fqn) else { return empty };
        let templates: Vec<String> = cd
            .docblock
            .as_deref()
            .map(steins_phpdoc::scan_template_names)
            .unwrap_or_default()
            .iter()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if templates.is_empty() {
            return empty; // not a generic class — no carry.
        }
        let Some((cfile, ctor)) = self.find_ctor(class_fqn) else { return empty };
        // The constructor's own `@param` envelopes, WITHOUT the class-level template
        // shadow applied: a bare `@param T` must stay readable as the template name
        // `T` here (the shadow that neutralizes it to opaque is a check-site concern).
        //
        // Read in the constructor's **own** file (issue #374). This was the last site
        // still on [`parse_envelopes`], for want of a namespace context: an inherited
        // constructor's `template-type` owner resolved in the subclass's `use` scope
        // would name a different class, so the node kept the `Opaque` floor and bound
        // nothing. `find_ctor` reports the declaring file now, so a `@param
        // template-type<Box<T>, Box, 'T'>` projects `T` — and a projected template
        // name is exactly what the alignment below binds.
        let doc = ctor.docblock.as_deref();
        let Some(ctor_env) = self.envelopes_of(doc, cfile, ctor.span.start) else { return empty };
        let mut out = Vec::with_capacity(templates.len());
        for tmpl in &templates {
            // The single constructor parameter whose `@param` is exactly this
            // template name (a direct, top-level occurrence — no solver).
            let Some(pos) = ctor.params.iter().position(|p| {
                ctor_env.param(&p.name).is_some_and(|pty| {
                    matches!(&pty.kind, PKind::Identifier(n) if n.eq_ignore_ascii_case(tmpl))
                })
            }) else {
                return empty;
            };
            if ctor.params[pos].variadic {
                return empty; // a variadic element breaks positional alignment.
            }
            let Some(arg) = args.get(pos) else { return empty };
            let Some(cv) = self.resolve_cval(arg, env, store, poisoned, folder) else {
                return empty;
            };
            out.push(cv);
        }
        out
    }

    /// Whether `class_fqn` declares any class-level `@template` of its own.
    pub(crate) fn declares_templates(&self, class_fqn: &str) -> bool {
        self.find_class(class_fqn).is_some_and(|(_, cd)| {
            cd.docblock
                .as_deref()
                .is_some_and(|d| !steins_phpdoc::scan_template_names(d).is_empty())
        })
    }

    /// The class-level generic parameterizations a `new Class(args)` expression
    /// carries (ADR-0032 tier 3 + its inheritance-edge amendment, issues #10/#294).
    ///
    /// Two provenances, in fixed precedence:
    ///
    /// 1. **Own templates** — the class declares `@template`s and the `new` site
    ///    proves values for them ([`Self::infer_generic_args`]). Stops here even when
    ///    the value carry comes back empty: a value is the stronger fact whenever
    ///    there is one ("own templates win").
    /// 2. **Inheritance edges** — no own templates, so `@extends Box<int>` /
    ///    `@implements Producer<Dog>` names an *ancestor's* parameterization, one
    ///    edge per tag. Read one level only — following a generic intermediate up
    ///    the chain is a substitution problem this slice does not build.
    ///
    /// An edge is kept only when its base resolves to a class the object provably
    /// is-a, with exactly as many `@template`s as the tag writes arguments; an
    /// arity disagreement is a library-author lint (ADR-0032 keeps this thin) and
    /// is dropped silently.
    pub(crate) fn infer_generic_carry(
        &self,
        class_fqn: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        store: &Store,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Vec<GenericCarry> {
        if self.declares_templates(class_fqn) {
            let vals = self.infer_generic_args(class_fqn, args, env, store, poisoned, folder);
            if vals.is_empty() {
                return Vec::new();
            }
            return vec![GenericCarry {
                owner: class_fqn.to_owned(),
                args: vals.into_iter().map(CArg::Val).collect(),
                site: None,
            }];
        }
        self.inheritance_edges(class_fqn)
    }

    /// The parameterized inheritance edges declared on `class_fqn`'s own docblock,
    /// as owner-keyed carries (ADR-0032 amendment, issue #294). Empty for a class
    /// with no docblock, no such tag, or no tag that survives the checks above.
    pub(crate) fn inheritance_edges(&self, class_fqn: &str) -> Vec<GenericCarry> {
        self.inheritance_edge_types(class_fqn)
            .into_iter()
            .filter_map(|e| self.mint_declared_carry(&e.owner, &e.args, e.site))
            .collect()
    }

    /// Mint one owner-keyed [`GenericCarry`] out of a written parameterization
    /// `Owner<A, …>` — the one minting rule every **declared** carry goes through
    /// (an inheritance edge, ADR-0032's #294 amendment; a declared parameter, its
    /// 2026-08-16 one).
    ///
    /// Positional alignment is sound only against the owner's own `@template` list,
    /// so an arity disagreement mints nothing — the same all-or-nothing rule the
    /// value carry follows, and a library-author lint ADR-0032 keeps thin. `site`
    /// is the `(file, offset)` the argument names were written against, carried so
    /// that a reader lifting one out of the declaration keeps it naming the class it
    /// named. The carry stores the file by its path — the stable identity (issue
    /// #497) — so the per-run index handed in here does not outlive the mint.
    pub(crate) fn mint_declared_carry(
        &self,
        owner_fqn: &str,
        args: &[PType],
        site: (usize, u32),
    ) -> Option<GenericCarry> {
        let declared = class_template_names(self, owner_fqn);
        if declared.is_empty() || declared.len() != args.len() {
            return None;
        }
        Some(GenericCarry {
            owner: owner_fqn.to_owned(),
            args: args.iter().map(|t| CArg::Ty(steins_contract::lower(t))).collect(),
            site: Some((self.units[site.0].path.to_owned(), site.1)),
        })
    }

    /// The same edges [`Self::inheritance_edges`] returns, **before** lowering —
    /// each argument still the phpdoc AST the author wrote.
    ///
    /// One parse serves two readers with different needs (issue #361). The carry
    /// wants a `ContractTy`, because acceptance meets it with `subsumes`. The
    /// declared-side `template-type` projection wants the AST, because it splices
    /// one argument back into an envelope that has not been lowered yet — and a
    /// `ContractTy` could not carry a template identifier through the shadow stages
    /// that still have to run over it. Every gate (a real ancestor, an arity that
    /// aligns with the owner's own `@template` list) is applied here, so both
    /// readers see exactly the edges the amendment admits.
    pub(crate) fn inheritance_edge_types(&self, class_fqn: &str) -> Vec<InheritanceEdge> {
        let Some((file, cd)) = self.find_class(class_fqn) else { return Vec::new() };
        let Some(doc) = cd.docblock.as_deref() else { return Vec::new() };
        let coff = cd.span.start;
        let mut out = Vec::new();
        for tail in steins_phpdoc::scan_inheritance_args(doc) {
            // The tag's tail is raw text; a trailing description is tolerated (the
            // parser reports a consumed prefix), an unparseable one contributes
            // nothing and its siblings are unaffected (ADR-0029).
            let Ok(parsed) = steins_phpdoc::parse_type(&tail) else { continue };
            let PKind::Generic { base, args } = &parsed.ty.kind else { continue };
            if args.is_empty() {
                continue;
            }
            let owner = self.resolve_pclass(file, coff, base);
            // The edge must name a real ancestor: an `@extends` that disagrees with
            // the `extends` in source says nothing trustworthy about this object.
            if self.is_a(class_fqn, &owner) != IsA::Yes {
                continue;
            }
            // Positional alignment is only sound against the owner's own template
            // list — same all-or-nothing rule the value carry follows.
            let declared = self
                .find_class(&owner)
                .and_then(|(_, od)| od.docblock.as_deref())
                .map(steins_phpdoc::scan_template_names)
                .unwrap_or_default();
            if declared.len() != args.len() {
                continue;
            }
            out.push(InheritanceEdge {
                owner,
                args: args.iter().map(|a| a.ty.clone()).collect(),
                site: (file, coff),
            });
        }
        out
    }

    /// Resolve a **function** call reference per PHP name resolution (ADR-0001).
    pub(crate) fn resolve_function(&self, r: &NameRef) -> FnResolution {
        self.resolve_function_with(r, &|n| steins_catalog::effect_labels(n).is_some())
    }

    /// [`Self::resolve_function`] with the effects pass's wider notion of a known
    /// builtin: a name carrying a by-ref out-parameter row
    /// ([`steins_catalog::out_params`]) counts too, even with no unconditional
    /// color and not foldable — `preg_match`/`sort` are exactly that, and P2 is
    /// what gives them something to say. Scoped to the effects pass on purpose: the
    /// same widening would also change the *throws* pass's classification of these
    /// names, left untouched here.
    pub(crate) fn resolve_effect_function(&self, r: &NameRef) -> FnResolution {
        self.resolve_function_with(r, &|n| {
            steins_catalog::effect_labels(n).is_some() || steins_catalog::out_params(n).is_some()
        })
    }

    /// [`Self::resolve_function`] with the ADR-0070 notion of a known builtin: a
    /// name whose argument semantics the catalog can state
    /// ([`steins_catalog::by_value_arg`], three-valued — `None` means "unknown to
    /// the catalog"). Distinct from an effect color or a by-ref row: `trim` has no
    /// out-param row and is still fully described; `sscanf` has neither and stays
    /// unknown. Scoped to the call-argument survival gate only.
    pub(crate) fn resolve_arg_function(&self, r: &NameRef) -> FnResolution {
        self.resolve_function_with(r, &|n| steins_catalog::by_value_arg(n, 0).is_some())
    }

    /// [`Self::resolve_function`] with the higher-order invocation notion of a
    /// known builtin: a name the catalog states a callback-invoking shape for
    /// ([`steins_catalog::invocation_shape`]) — `usort`, `array_map`,
    /// `call_user_func`, etc. Distinct on purpose: those carry neither an effect
    /// color nor an out-param row, so [`Self::resolve_effect_function`] would miss
    /// them.
    ///
    /// Before issue #279's fix, call sites asked [`steins_catalog::invocation_shape`]
    /// directly against the call's raw spelling — blind to a `use function usort as
    /// u;` alias, and blind to shadowing by a project function of the same name.
    /// Routing through [`Self::resolve_function_with`] fixes both: a project
    /// declaration wins per ADR-0001 resolution regardless of alias.
    pub(crate) fn resolve_invoker_function(&self, r: &NameRef) -> FnResolution {
        self.resolve_function_with(r, &|n| steins_catalog::invocation_shape(n).is_some())
    }

    /// Resolution **as if the catalog knew nothing** — so a `User` answer is
    /// exactly "a project declaration shadows this spelling", and nothing else.
    ///
    /// [`Self::resolve_function`] cannot answer that question. Its notion of a
    /// known builtin is `effect_labels`, and its global-fallback arm turns a
    /// user declaration that shadows a *known* name into `Unknown` rather than
    /// `User` — the comment there calls it ambiguous, which it is. A caller
    /// asking "is this shadowed?" by testing `!matches!(…, User(_))` therefore
    /// gets `true` for a shadowed catalogued name and `false` for a shadowed
    /// uncatalogued one: the answer depends on whether the catalog happens to
    /// carry the name, which is not a property of the source at all.
    ///
    /// That coupling was live and reachable. On master a project
    /// `function preg_split($p, $s) {}` beside a `preg_split('/(unclosed/', …)`
    /// call reported `preg.invalid-pattern` against the *user's* function —
    /// `preg_split` is foldable, so the catalog knows it — while the identical
    /// shape around `preg_match` stayed silent only because that name was
    /// uncatalogued. Admitting a name to the fold allowlist would have flipped
    /// its recognizers from respecting a shadow to ignoring one, which is a
    /// coupling no allowlist edit should have.
    ///
    /// Passing a `catalog_knows` that is always false keeps every other rule —
    /// `use function` imports, the namespace-then-global fallback, ambiguity —
    /// and leaves only the project declarations behind.
    pub(crate) fn resolve_shadow(&self, r: &NameRef) -> FnResolution {
        self.resolve_function_with(r, &|_| false)
    }

    pub(crate) fn resolve_function_with(
        &self,
        r: &NameRef,
        catalog_knows: &dyn Fn(&str) -> bool,
    ) -> FnResolution {
        match r.kind {
            RefKind::FullyQualified => {
                let fqn = r.raw.to_ascii_lowercase();
                match self.index.resolve_function(&fqn) {
                    Res::Unique(site) => FnResolution::User(site),
                    Res::Ambiguous => FnResolution::Unknown,
                    Res::Absent => {
                        // `\strlen` — a single-segment global name may be a builtin.
                        if !r.raw.contains('\\') && catalog_knows(&r.raw) {
                            FnResolution::Builtin(fqn)
                        } else {
                            FnResolution::Unknown
                        }
                    }
                }
            }
            RefKind::Qualified => {
                // First segment via class/namespace imports, else current ns.
                let ctx = self.tree().ctx_at(r.offset);
                let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
                let first = &r.raw[..first_len];
                let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                    format!("{t}{}", &r.raw[first_len..])
                } else if ctx.namespace.is_empty() {
                    r.raw.clone()
                } else {
                    format!("{}\\{}", ctx.namespace, r.raw)
                };
                match self.index.resolve_function(&fqn) {
                    Res::Unique(site) => FnResolution::User(site),
                    _ => FnResolution::Unknown,
                }
            }
            RefKind::Unqualified => {
                let ctx = self.tree().ctx_at(r.offset);
                let name = r.raw.to_ascii_lowercase();
                // `use function` import wins outright.
                if let Some(t) = ctx.fn_imports.get(&name) {
                    let target = t.to_ascii_lowercase();
                    return match self.index.resolve_function(&target) {
                        Res::Unique(site) => FnResolution::User(site),
                        Res::Ambiguous => FnResolution::Unknown,
                        // `use function strtolower;` imports the global builtin — same
                        // case the `FullyQualified` arm above answers `Builtin`. Without
                        // this leg an import silenced every catalog answer about the name
                        // (issue #41: phpstan-src's `non-empty-string.php` imports five
                        // string builtins, each losing ADR-0070 argument survival).
                        Res::Absent => {
                            if !target.contains('\\') && catalog_knows(&target) {
                                FnResolution::Builtin(target)
                            } else {
                                FnResolution::Unknown
                            }
                        }
                    };
                }
                let is_builtin = catalog_knows(&name);
                // PHP tries NS\name first (when in a namespace).
                if !ctx.namespace.is_empty() {
                    let ns_fqn = format!("{}\\{}", ctx.namespace, name);
                    match self.index.resolve_function(&ns_fqn) {
                        Res::Unique(site) => return FnResolution::User(site),
                        Res::Ambiguous => return FnResolution::Unknown,
                        Res::Absent => {}
                    }
                }
                // Global fallback (also the whole story in the global namespace).
                match self.index.resolve_function(&name) {
                    Res::Ambiguous => FnResolution::Unknown,
                    Res::Unique(site) => {
                        // A user global that shadows a builtin name is ambiguous.
                        if is_builtin { FnResolution::Unknown } else { FnResolution::User(site) }
                    }
                    Res::Absent => {
                        if is_builtin { FnResolution::Builtin(name) } else { FnResolution::Unknown }
                    }
                }
            }
            // ADR-0049 A8: `namespace\name` resolves against the enclosing namespace
            // only — no `use` imports, no global fallback (undefined `Ns\name` is a
            // fatal error). In the global namespace the candidate is `name` itself.
            RefKind::Relative => {
                let ctx = self.tree().ctx_at(r.offset);
                let name = r.raw.to_ascii_lowercase();
                if ctx.namespace.is_empty() {
                    match self.index.resolve_function(&name) {
                        Res::Ambiguous => FnResolution::Unknown,
                        Res::Unique(site) => {
                            if catalog_knows(&name) { FnResolution::Unknown } else { FnResolution::User(site) }
                        }
                        Res::Absent => {
                            if catalog_knows(&name) { FnResolution::Builtin(name) } else { FnResolution::Unknown }
                        }
                    }
                } else {
                    let fqn = format!("{}\\{}", ctx.namespace.to_ascii_lowercase(), name);
                    match self.index.resolve_function(&fqn) {
                        Res::Unique(site) => FnResolution::User(site),
                        _ => FnResolution::Unknown,
                    }
                }
            }
        }
    }

    /// The site of a **user** function this call resolves to (positional-only),
    /// or `None` for builtins / unknown / dynamic / named-arg calls.
    pub(crate) fn resolve_user_fn(&self, call: &CallExpr) -> Option<Site> {
        if !call.positional_only {
            return None;
        }
        self.resolve_user_fn_any(call)
    }

    /// Resolve the call's user-function target without the positional-only guard
    /// (Gap A): argument-contract lanes bind named arguments and check a mixed
    /// call's positional prefix, so they need this for non-positional calls too.
    /// The binding descent still routes through [`Self::resolve_user_fn`], whose
    /// guard keeps positional-mapping descent off named/spread calls.
    pub(crate) fn resolve_user_fn_any(&self, call: &CallExpr) -> Option<Site> {
        let r = call.callee_ref.as_ref()?;
        match self.resolve_function(r) {
            FnResolution::User(site) => Some(site),
            _ => None,
        }
    }

    /// The unique body scope of the user function at `site`, plus its file.
    pub(crate) fn fn_scope(&self, site: Site) -> Option<(usize, &'a Scope)> {
        let name = &self.fn_decl(site).name;
        let tree = self.units[site.file].tree;
        let mut it = tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(name));
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some((site.file, scope)) }
    }

    /// The unique method body scope for `class_fqn::method` in `file`.
    pub(crate) fn method_scope(&self, file: usize, class_fqn: &str, method: &str) -> Option<&'a Scope> {
        let tree = self.units[file].tree;
        let mut it = tree.scopes().iter().filter(|s| {
            matches!(&s.owner, ScopeOwner::Method { class: c, method: m }
                if c.eq_ignore_ascii_case(class_fqn) && m.eq_ignore_ascii_case(method))
        });
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some(scope) }
    }

    /// The closure/arrow body scope defined at `def_offset` in this file (ADR-0033),
    /// for descent through a proven `$fn()` closure value.
    pub(crate) fn closure_scope(&self, def_offset: u32) -> Option<&'a Scope> {
        self.tree().scopes().iter().find(|s| {
            matches!(&s.owner, ScopeOwner::Closure { def_offset: d } if *d == def_offset)
        })
    }

    /// A display name for an effect [`Sym`] (`f`, `Foo::bar`), using the resolved
    /// declaration's written case where available.
    pub(crate) fn sym_display(&self, sym: &Sym) -> String {
        match sym {
            Sym::Func(fqn) => match self.index.resolve_function(fqn) {
                Res::Unique(site) => self.fn_decl(site).name.clone(),
                _ => fqn.clone(),
            },
            Sym::Method(cfqn, m) => match self.find_class(cfqn) {
                Some((_, cd)) => format!("{}::{}", cd.name, m),
                None => format!("{cfqn}::{m}"),
            },
            Sym::Closure(_, off) => {
                let line = self.tree().position(*off).line;
                format!("closure (line {line})")
            }
        }
    }

    /// Resolve an [`ArgValue`] to a concrete literal, if provable.
    ///
    /// Equivalent to [`Self::resolve_literal_under`] with no live descent — for
    /// call sites (assignment, dump, property checks) not mid-binding. Live-descent
    /// callers must use [`Self::resolve_literal_under`] so the on-stack guard
    /// threads through.
    ///
    /// The second return is the trust stratum (ADR-0052 §5): a fold that consumed
    /// an Asserted project-call summary stays Asserted, never laundering into a
    /// proof-layer premise.
    pub(crate) fn resolve_literal(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<ArgValue> {
        self.resolve_literal_under(value, env, poisoned, folder, None, None)
            .map(|(v, _)| v)
    }

    /// Like [`Self::resolve_literal`], but also returns the trust stratum of the
    /// resolved value (issue #127: fold-arg Asserted must not launder).
    pub(crate) fn resolve_literal_strat(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<(ArgValue, Stratum)> {
        self.resolve_literal_under(value, env, poisoned, folder, None, None)
    }

    /// [`Self::resolve_literal_strat`] with optional live descent and findings sink
    /// (issue #127). Nested project-call descents for fold args emit through `out`
    /// when provided so binding-specific diagnostics are not discarded.
    pub(crate) fn resolve_literal_strat_ex(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
        descent: Option<&mut Descent<'_>>,
        out: Option<&mut Vec<Diagnostic>>,
    ) -> Option<(ArgValue, Stratum)> {
        self.resolve_literal_under(value, env, poisoned, folder, descent, out)
    }

    /// [`Self::resolve_literal`] with an optional live [`Descent`] and findings sink
    /// (issue #127). Returns `(value, stratum)`: for a fold, stratum is `min` over
    /// the resolved fold arguments (including a nested project-call summary's
    /// stratum).
    pub(crate) fn resolve_literal_under(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
        mut descent: Option<&mut Descent<'_>>,
        mut out: Option<&mut Vec<Diagnostic>>,
    ) -> Option<(ArgValue, Stratum)> {
        if poisoned {
            return None;
        }
        match value {
            v if v.is_literal() => Some((v.clone(), Stratum::Verified)),
            ArgValue::Var(name) => {
                let k = env.get(name)?;
                Some((k.singleton()?, k.stratum))
            }
            ArgValue::Call(name, args) => {
                if args.is_empty()
                    && let Some((lit, _line)) = self.resolve_const_fn(name)
                {
                    return Some((lit, Stratum::Verified));
                }
                // Builtin fold (allowlist + project-shadow gate). Project-call
                // arguments of the fold are resolved inside `try_fold_under`
                // (issue #127). A bare project call (`g(1)` as a value) is *not*
                // resolved here — that stays the caller's job (`nested_call_singleton`
                // / `project_call_summary`) so findings still emit on the live `out`.
                self.try_fold_under(name, args, env, poisoned, folder, descent, out)
                    .map(|(lit, _prov, strat)| (lit, strat))
                    // The transfer rung's answers are values too (issue #329): a rung
                    // proving a `Singleton` (`array_slice($a, 1)`) previously had no way
                    // to say so here, so value-position `===`, fold arguments, and
                    // `concat_cast` were blind to it even though a binding hop would see
                    // it — a distinction PHP itself doesn't make. Tried below the fold,
                    // since it's more precise and cheaper. The rung's own stratum comes
                    // back with the value (ADR-0061 §3), so an Asserted subject cannot
                    // launder into Verified via the value road.
                    .or_else(|| {
                        let (fact, strat) = shape_builtin_return_fact(
                            self, folder, name, args, env, None, poisoned,
                        )?;
                        match fact {
                            Fact::Singleton(v) => Some((arg_of_val(&v), strat)),
                            _ => None,
                        }
                    })
            }
            // `$a . $b` (issue #59): proven iff BOTH operands resolve to values whose
            // string cast is total and environment-independent (`concat_cast`); one
            // unresolved operand yields `None`, never a partial string.
            ArgValue::Concat(a, b) => {
                let (l, sl) = self.resolve_literal_under(
                    a, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                let (r, sr) = self.resolve_literal_under(
                    b, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                // `.` joins bytes (ADR-0080): two invalid-UTF-8 halves can still
                // concatenate to a valid string; `from_vec` re-canonicalizes the join.
                let (lb, rb) = (concat_cast(&l)?, concat_cast(&r)?);
                let mut bytes = lb.as_bytes().to_vec();
                bytes.extend_from_slice(rb.as_bytes());
                Some((ArgValue::Str(PhpStr::from_vec(bytes)), sl.min(sr)))
            }
            // A comparison in value position (issue #260): the same `eval_cmp` the
            // condition path runs, over the same candidate value sets — a decided
            // verdict IS the expression's value. Undecided yields `None`; the `bool`
            // floor every comparison deserves is minted one level up at the fact seam,
            // since a literal can't spell it.
            ArgValue::Binary { op: ValueOp::Cmp(cop), lhs, rhs } => {
                let l = self.cmp_candidates_under(
                    lhs, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                let r = self.cmp_candidates_under(
                    rhs, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                // Same derivation as `.` above: result stratum is the operands' min.
                let strat = value_stratum(lhs, env, None).min(value_stratum(rhs, env, None));
                match eval_cmp(*cop, &l, &r, self.php_minor) {
                    Certainty::Yes => Some((ArgValue::Bool(true), strat)),
                    Certainty::No => Some((ArgValue::Bool(false), strat)),
                    Certainty::Maybe => None,
                }
            }
            // A `<=>` in value position (issue #625): the same pole decision the
            // fact seam runs, over the same candidate sets, so a decided
            // spaceship IS the expression's value. Undecided yields `None`; the
            // `int<-1, 1>` floor is minted one level up at the fact seam, since a
            // literal cannot spell a range.
            //
            // `ArgValue::Logical` and `ArgValue::Not` are deliberately NOT here,
            // following `ArgValue::Isset`'s precedent exactly: their evaluators
            // need the walk context (a decided `&&`/`||` records its
            // short-circuited operand dead) that this literal seam does not
            // carry, so they decline here and answer at the fact seam. The cost
            // is the same one `isset` already pays — an operator node used as a
            // COMPARISON operand is undecided, so `isset($x) === true` is `bool`,
            // and now `(true && true) === true` is too.
            ArgValue::Binary { op: ValueOp::Spaceship, lhs, rhs } => {
                let l = self.cmp_candidates_under(
                    lhs, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                let r = self.cmp_candidates_under(
                    rhs, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                )?;
                let strat = value_stratum(lhs, env, None).min(value_stratum(rhs, env, None));
                spaceship_pole(&l, &r, self.php_minor).map(|n| (ArgValue::Int(n), strat))
            }
            // An array is proven iff every element value is proven (keys are fixed
            // at lowering). Folding is never applied to arrays (ADR-0001).
            ArgValue::Array(items) => {
                let mut resolved = Vec::with_capacity(items.len());
                let mut strat = Stratum::Verified;
                for (k, v) in items {
                    let (rv, s) = self.resolve_literal_under(
                        v, env, poisoned, folder, descent.as_deref_mut(), out.as_deref_mut(),
                    )?;
                    strat = strat.min(s);
                    resolved.push((k.clone(), rv));
                }
                Some((ArgValue::Array(resolved), strat))
            }
            _ => None,
        }
    }

    /// The candidate values of a value-position comparison operand (issue #260) —
    /// the value-side twin of `operand_values`, which does this for a guard's
    /// [`CondOperand`]. A bare variable contributes its fact's finite members
    /// (`Singleton`/`OneOf`, per ADR-0031's all-pairs rule); anything else
    /// contributes one candidate from the general value seam. `None` means no
    /// candidates at all.
    ///
    /// [`CondOperand`]: steins_syntax::CondOperand
    pub(crate) fn cmp_candidates_under(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
        descent: Option<&mut Descent<'_>>,
        out: Option<&mut Vec<Diagnostic>>,
    ) -> Option<Vec<ArgValue>> {
        if !poisoned
            && let ArgValue::Var(name) = value
            && let Some(fact) = env.get(name).and_then(|k| k.fact.as_ref())
            && let Some(vals) = fact.finite_members()
        {
            return Some(vals.iter().map(arg_of_val).collect());
        }
        self.resolve_literal_under(value, env, poisoned, folder, descent, out).map(|(v, _)| vec![v])
    }

    /// Try to fold an allowlisted builtin call over **proven** arguments.
    ///
    /// # The env-resolved argument (ADR-0062 S7, the §1 fold gap)
    ///
    /// Each argument is first rendered as the value it provably *is*, through
    /// [`Self::resolve_literal`]: a bound `$a`, a proven array element, a nested
    /// foldable call. An unresolved argument is left exactly as written, so
    /// resolution can only ever add arguments to the fold, never remove one.
    ///
    /// Closes ADR-0062 §1's gap: `$a = ['x', 'y']; count($a)` now folds like
    /// `count(['x', 'y'])`. The allowlist and purity discipline are unchanged —
    /// the fold still runs on the project's own PHP (ADR-0004/0028), so every
    /// order-dependent builtin (`in_array`, `implode`, `count`) is answered by the
    /// real engine over the real array (ADR-0062 §2's value lane), never
    /// re-derived here.
    ///
    /// # Project-call arguments (issue #127)
    ///
    /// A foldable arg that is itself a project call (`strtoupper(g(1))`) resolves
    /// through the T0 summary under the same descent guard as a nested binding —
    /// see [`Self::resolve_literal_under`]. Budget exhaustion widens rather than
    /// partially folding. Result stratum is `min` over resolved arguments, so an
    /// Asserted project-call premise cannot launder into a Verified fold.
    ///
    /// Provenance names the resolved call. Returns `(folded, provenance, stratum)`.
    pub(crate) fn try_fold(
        &self,
        name: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<(ArgValue, String, Stratum)> {
        self.try_fold_under(name, args, env, poisoned, folder, None, None)
    }

    /// Like [`Self::try_fold`], threading a findings sink so nested project-call
    /// descents for fold arguments emit binding-specific diagnostics (issue #127).
    pub(crate) fn try_fold_emit(
        &self,
        name: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
        out: &mut Vec<Diagnostic>,
    ) -> Option<(ArgValue, String, Stratum)> {
        self.try_fold_under(name, args, env, poisoned, folder, None, Some(out))
    }

    /// [`Self::try_fold`] with optional live [`Descent`] and findings sink (issue #127).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_fold_under(
        &self,
        name: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
        mut descent: Option<&mut Descent<'_>>,
        mut out: Option<&mut Vec<Diagnostic>>,
    ) -> Option<(ArgValue, String, Stratum)> {
        // Any project user function sharing this simple name shadows the builtin
        // (or makes it ambiguous) — do not fold. Conservative, never an FP.
        if self.index.has_simple_function(name) {
            return None;
        }
        if !steins_catalog::foldable(name) {
            return None;
        }
        let mut resolved = Vec::with_capacity(args.len());
        let mut arg_strat = Stratum::Verified;
        // No external sink (dump surface, pure resolution) uses a scratch so nested
        // descents stay silent; walk/check paths pass a real `out` so findings under
        // `g(1)` in `strtoupper(g(1))` aren't discarded (issue #127).
        let mut scratch: Vec<Diagnostic> = Vec::new();
        for a in args {
            // Resolve under the live descent so a project-call arg's summary reuses
            // the on-stack guard (issue #127); a non-foldable project call is
            // answered by `nested_call_singleton`.
            let (r, s) = if let Some((v, s)) = self.resolve_literal_under(
                a,
                env,
                poisoned,
                folder,
                descent.as_deref_mut(),
                out.as_deref_mut(),
            ) {
                (v, s)
            } else if let Some((v, s)) = {
                let sink: &mut Vec<Diagnostic> = match out.as_deref_mut() {
                    Some(o) => o,
                    None => &mut scratch,
                };
                // No caller heap is in hand on the fold road (it resolves values, and
                // a fold's arguments are scalars by construction), so the nested
                // descent seeds from an empty one: a `new` written there still crosses
                // — it needs no caller heap — and a heap-bound variable does not.
                // Strictly less knowledge, never wrong knowledge (ADR-0086 §2).
                nested_call_singleton(
                    self,
                    folder,
                    a,
                    env,
                    &Store::default(),
                    poisoned,
                    0,
                    descent.as_deref_mut(),
                    sink,
                )
            } {
                (v, s)
            } else {
                // Unresolved: keep the written form for the gate, stratum from
                // the syntactic arm (env/prop reads only — no summary).
                (a.clone(), value_stratum(a, env, None))
            };
            arg_strat = arg_strat.min(s);
            resolved.push(r);
        }
        // Every argument must be a self-evident value: a scalar literal, or an array
        // literal concrete all the way down and inside the fold budget (issue #39).
        // Checked before `folder.fold` so an over-budget literal is never cloned
        // into the memo.
        if !resolved.iter().all(is_fold_arg) {
            return None;
        }
        // The CALL SITE's calling convention: `declare(strict_types=1)` binds to
        // the file a call is written in, and `tree()` is the file being walked —
        // including when the walk has descended into another file's body, which
        // is PHP's rule exactly.
        let folded = folder.fold(name, &resolved, self.tree().has_strict_types())?;
        Some((folded, format!("folded from {}", render_call(name, &resolved)), arg_strat))
    }

    /// Fold an allowlisted builtin over a **bounded union of constant arguments**,
    /// composing the members' answers into one value fact (issue #74).
    ///
    /// PHPStan's dynamic return extensions accept a constant or a union of
    /// constants, calling the real function once per member and composing results.
    /// [`Self::try_fold`] only admits a single constant tuple, so `$x = $c ? 'a' :
    /// 'b'; strtoupper($x)` widened where PHPStan answers `'A'|'B'` (ADR-0069
    /// amendment). This closes that gap.
    ///
    /// Per-argument resolution ladder: (1) whatever [`Self::resolve_literal`]
    /// proves; (2) failing that, a `Fact::OneOf` env fact every member of which
    /// converts to a foldable argument. Neither resolving declines the whole fold.
    /// A `Singleton` is the one-member case of the same ladder (`intdiv($u, 2)`
    /// works because the literal and union lanes compose in the product).
    ///
    /// Bounds: at most [`UNION_FOLD_MEMBER_CAP`] members per argument,
    /// [`UNION_FOLD_COMBINATION_CAP`] combinations total. Busting either declines
    /// rather than truncates — a union missing a member is a *wrong* domain
    /// (ADR-0002), not a wider one.
    ///
    /// Each member tuple goes back through [`Self::try_fold`], so name gates, the
    /// fold budget, the memo, the issue-#64 integer-width gate, and (in the
    /// browser) the ADR-0066 replay loop all apply unchanged. A widening or
    /// throwing member declines the whole fold — a union that quietly drops its
    /// throwing member is the same wrong domain the cap refuses to mint. Every
    /// combination is still asked before declining, so a replay transport learns
    /// the batch in one round trip.
    ///
    /// Replayability (ADR-0028/0048): pure function of (CST, entry state, fold
    /// memo) — arguments in source order, `Fact::OneOf` canonically sorted, product
    /// walked in one fixed order.
    ///
    /// Returns the composed fact, its stratum, and a provenance string.
    pub(crate) fn try_union_fold(
        &self,
        name: &str,
        args: &[ArgValue],
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<(Fact, Stratum, String)> {
        if poisoned || args.is_empty() {
            return None;
        }
        // Same two name gates as `try_fold`, asked before enumeration so a shadowed
        // or non-allowlisted name costs nothing.
        if self.index.has_simple_function(name) || !steins_catalog::foldable(name) {
            return None;
        }
        let mut lanes: Vec<Vec<ArgValue>> = Vec::with_capacity(args.len());
        let mut combinations: usize = 1;
        for arg in args {
            let members: Vec<ArgValue> = match self.resolve_literal(arg, env, poisoned, folder) {
                Some(lit) => vec![lit],
                None => {
                    let ArgValue::Var(var) = arg else { return None };
                    let Some(Fact::OneOf(vals)) = env.get(var).and_then(|k| k.fact.as_ref()) else {
                        return None;
                    };
                    if vals.len() > UNION_FOLD_MEMBER_CAP {
                        return None;
                    }
                    vals.iter().map(arg_of_val).collect()
                }
            };
            // Same gate as `try_fold`, applied per member: a non-self-evident value
            // (or a busted array budget) takes the whole fold down rather than being
            // quietly dropped.
            if !members.iter().all(is_fold_arg) {
                return None;
            }
            combinations = combinations.checked_mul(members.len())?;
            if combinations > UNION_FOLD_COMBINATION_CAP {
                return None;
            }
            lanes.push(members);
        }
        // One combination is the plain fold, already owned by `resolve_literal`
        // (this rung is only reached after that one declined).
        if combinations < 2 {
            return None;
        }
        // Bounded cartesian product, last argument varying fastest. Every
        // combination is asked even once one has declined — the verdict is
        // unaffected, but the browser's replay transport (ADR-0066) batches
        // unanswered requests per iteration, so this is one round trip instead of
        // one per member; on the native transport the extra questions are bounded
        // and memoized.
        let mut vals: Vec<Val> = Vec::with_capacity(combinations);
        let mut declined = false;
        let mut odometer = vec![0usize; lanes.len()];
        for _ in 0..combinations {
            let combo: Vec<ArgValue> =
                lanes.iter().zip(&odometer).map(|(lane, i)| lane[*i].clone()).collect();
            match self
                .try_fold(name, &combo, env, poisoned, folder)
                .and_then(|(folded, _, _)| val_of(&folded, self.php_minor))
            {
                Some(v) => vals.push(v),
                None => declined = true,
            }
            for k in (0..odometer.len()).rev() {
                odometer[k] += 1;
                if odometer[k] < lanes[k].len() {
                    break;
                }
                odometer[k] = 0;
            }
        }
        if declined {
            return None;
        }
        // `Fact::from_vals`: deduped and sorted, `Singleton` when every member
        // agreed, `OneOf` up to the domain CAP, else the computed widening.
        let fact = Fact::from_vals(vals)?;
        // Stratum (ADR-0048 N2 / ADR-0052 §5): each member answer is engine-Verified,
        // but the input union carries its own trust — min over the arguments, so an
        // Asserted union in gives an Asserted result out.
        let stratum =
            args.iter().fold(Stratum::Verified, |acc, a| acc.min(value_stratum(a, env, None)));
        Some((fact, stratum, format!("folded from {name}() over {combinations} argument combinations")))
    }

    /// Resolve a zero-argument constant function anywhere in the project by its
    /// simple name: unique definition, no params, body exactly `return <lit>`,
    /// scope not poisoned. Returns the literal and the definition line.
    pub(crate) fn resolve_const_fn(&self, name: &str) -> Option<(ArgValue, u32)> {
        let site = self.index.unique_fn_by_simple(name)?;
        let decl = self.fn_decl(site);
        if !decl.params.is_empty() {
            return None;
        }
        let tree = self.units[site.file].tree;
        let mut scopes = tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(&decl.name));
        let scope = scopes.next()?;
        if scopes.next().is_some() || scope.poisoned {
            return None;
        }
        let [stmt] = scope.stmts.as_slice() else { return None };
        let StmtKind::Return { value, .. } = &stmt.kind else { return None };
        if !value.is_literal() {
            return None;
        }
        Some((value.clone(), tree.position(decl.span.start).line))
    }

    /// Build a `type.argument-mismatch` diagnostic (path/line from the current
    /// file — where the call textually is).
    pub(crate) fn diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        provenance: Option<&str>,
        callee: &str,
        param_name: &str,
        ty: &NativeType,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let mode = if self.strict() { "strict" } else { "coercive" };
        let message = match provenance {
            Some(p) => format!(
                "argument {} ({}) to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), p, callee, ty.render(), param_name, mode,
            ),
            None => format!(
                "argument {} to {}() cannot become {} ${} — proven TypeError ({} mode)",
                value.render(), callee, ty.render(), param_name, mode,
            ),
        };
        Diagnostic { id: ID, path: self.path().to_owned(), line: pos.line, column: pos.column, message, facet: None, fix: None }
    }

    /// Build the resource flavour of a `type.argument-mismatch` diagnostic
    /// (ADR-0056 §8) — same id and shape as [`Self::diagnostic`], but the tail
    /// omits the coercion mode: a resource has no coercion path into anything, so
    /// naming the mode would wrongly suggest dropping `strict_types=1` helps.
    pub(crate) fn resource_diagnostic(
        &self,
        offset: u32,
        var: &str,
        callee: &str,
        param_name: &str,
        ty: &NativeType,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let tail = "proven TypeError (a resource coerces to nothing, in either mode)";
        let message = format!(
            "argument ${var} (holds a resource) to {callee}() cannot become {} ${param_name} — {tail}",
            ty.render(),
        );
        Diagnostic {
            id: ID,
            path: self.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        }
    }

    /// Build a `type.return-mismatch` diagnostic. `display` is the owning
    /// function/method name (`f`, `Foo::bar`); `mode` is governed by the owning
    /// file's `declare(strict_types=1)` — the file this `Cx` points at.
    pub(crate) fn return_diagnostic(
        &self,
        offset: u32,
        value: &ArgValue,
        ret: &NativeType,
        display: &str,
    ) -> Diagnostic {
        let pos = self.tree().position(offset);
        let mode = if self.strict() { "strict" } else { "coercive" };
        let message = format!(
            "return {} cannot become {} (return type of {}()) — proven TypeError ({} mode)",
            value.render(),
            ret.render(),
            display,
            mode,
        );
        Diagnostic {
            id: RETURN_ID,
            path: self.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        }
    }

    /// The parameter list of a scope's owning function or method (same file this
    /// `Cx` points at), or `None` for the top-level script scope. Used by the
    /// native-type parameter seeding (Feature B).
    pub(crate) fn scope_params(&self, scope: &Scope) -> Option<&'a [Param]> {
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f = self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                Some(&f.params)
            }
            ScopeOwner::Method { class, method } => {
                let cd = self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                Some(&m.params)
            }
            // A closure/arrow scope carries its own params (no FunctionDecl). Look
            // the scope up in the tree so the borrow has the `'a` project lifetime.
            ScopeOwner::Closure { def_offset } => {
                let s = self.tree().scopes().iter().find(|s| {
                    matches!(&s.owner, ScopeOwner::Closure { def_offset: d } if d == def_offset)
                })?;
                Some(&s.params)
            }
            // A property hook likewise carries its own (issue #544) — including the
            // implicit `$value` of a short-form `set`, which no declaration spells.
            // The owner triple is unique within a file, so it addresses the scope.
            ScopeOwner::PropertyHook { .. } => {
                let s = self.tree().scopes().iter().find(|s| s.owner == scope.owner)?;
                Some(&s.params)
            }
        }
    }

    /// The parsed `@param`/`@return`/assert envelopes off the scope's owning
    /// declaration docblock (function or method), with class-level `@template`
    /// names shadowed for a method (issue #5), or `None` when there is no docblock
    /// / the scope is a closure or top-level. Used by contract-fact seeding
    /// (ADR-0052 §9) to refine the native member list with the declared `@param`.
    pub(crate) fn scope_envelopes(&self, scope: &Scope) -> Option<Envelopes> {
        match &scope.owner {
            // Closures: `None` even though the scope may carry an adopted docblock
            // (issue #128 lit up `@return` only, via `scope_return_phpdoc`) —
            // `@param`/`@throws` on closures stays dark. A property hook adopts no
            // docblock at all yet (issue #544), so it is dark for the same reason.
            ScopeOwner::TopLevel
            | ScopeOwner::Closure { .. }
            | ScopeOwner::PropertyHook { .. } => None,
            ScopeOwner::Function(name) => {
                let f = self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                self.envelopes_of(f.docblock.as_deref(), self.cur, f.span.start)
            }
            ScopeOwner::Method { class, method } => {
                let cd = self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                let mut env = self.envelopes_of(m.docblock.as_deref(), self.cur, m.span.start)?;
                env.shadow_templates(&template_names_of(cd.docblock.as_deref()));
                Some(env)
            }
        }
    }

    /// The `@template` shadow set in force over a scope's *body* (issue #5 applied
    /// to statement-level docblocks): the owning declaration's own template names
    /// plus, for a method, the enclosing class-level ones — same two stages
    /// [`Cx::scope_envelopes`] applies. Empty for top-level and closure scopes.
    pub(crate) fn scope_template_shadow(&self, scope: &Scope) -> TemplateShadow {
        match &scope.owner {
            ScopeOwner::TopLevel | ScopeOwner::Closure { .. } => TemplateShadow::default(),
            // A hook body sits inside the class declaration exactly as a method body
            // does, so the class-level `@template` names shadow there too (issue #5);
            // the hook itself adopts no docblock, so there is no second stage.
            ScopeOwner::PropertyHook { class, .. } => self
                .tree()
                .classes()
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(class))
                .map_or_else(TemplateShadow::default, |cd| {
                    template_names_of(cd.docblock.as_deref())
                }),
            ScopeOwner::Function(name) => self
                .tree()
                .functions()
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(name))
                .map_or_else(TemplateShadow::default, |f| template_names_of(f.docblock.as_deref())),
            ScopeOwner::Method { class, method } => {
                let Some(cd) =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))
                else {
                    return TemplateShadow::default();
                };
                let mut shadow = template_names_of(cd.docblock.as_deref());
                if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
                    shadow.extend(template_names_of(m.docblock.as_deref()));
                }
                shadow
            }
        }
    }

    /// The native return type and display name of a scope's owning function,
    /// method, or closure (the same file this `Cx` points at), or `None` for the
    /// top-level script scope or an owner with no native scalar/union return type.
    pub(crate) fn scope_return(&self, scope: &'a Scope) -> Option<(&'a NativeType, String)> {
        // A generator's declared return type names the `Generator` object the
        // *call* yields, not the values of in-body `return` (those are
        // `Generator::getReturn()`). Checking body returns against `: Generator`
        // is a false positive (issue #128 review).
        if scope.is_generator {
            return None;
        }
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f =
                    self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                f.ret.as_ref().map(|r| (r, f.name.clone()))
            }
            ScopeOwner::Method { class, method } => {
                // `owner.class` is the case-preserved FQN; `ClassDecl.fqn` is
                // lowercase-normalized — compare case-insensitively.
                let cd =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                m.ret.as_ref().map(|r| (r, format!("{}::{}", cd.name, m.name)))
            }
            // Issue #128: closures carry their native `: R` on the scope itself
            // (`Scope::ret_ty`) — same check surface as free functions.
            ScopeOwner::Closure { .. } => {
                scope.ret_ty.as_ref().map(|r| (r, "closure".to_owned()))
            }
            // A `get` hook returns the property's own declared type, which rides on
            // the scope for the same reason a closure's does — no declaration carries
            // it (issue #544). A `set` hook has no `ret_ty` at all.
            ScopeOwner::PropertyHook { class, property, hook } => scope
                .ret_ty
                .as_ref()
                .map(|r| (r, format!("{class}::${property}::{}", hook.as_str()))),
        }
    }

    /// The `@return` phpdoc envelope and display name of a scope's owning
    /// function, method, or closure (same file this `Cx` points at), or `None`
    /// when there is no docblock `@return` (or the scope is top-level).
    pub(crate) fn scope_return_phpdoc(&self, scope: &Scope) -> Option<(PType, String)> {
        // Mirrors [`Cx::scope_return`]'s native guard (issue #142): a generator's
        // declared return type names the `Generator` object the call yields, not
        // in-body `return` values — checking those against `@return Generator` is
        // an FP (issue #128) that guarding only the native side left alive.
        if scope.is_generator {
            return None;
        }
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f =
                    self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                let ret = self.envelopes_of(f.docblock.as_deref(), self.cur, f.span.start)?.ret?;
                Some((ret, f.name.clone()))
            }
            ScopeOwner::Method { class, method } => {
                let cd =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                let mut env = self.envelopes_of(m.docblock.as_deref(), self.cur, m.span.start)?;
                // Class-level `@template` names shadow in this method's `@return` too
                // (issue #5) — the idempotent class-level stage.
                env.shadow_templates(&template_names_of(cd.docblock.as_deref()));
                let ret = env.ret?;
                Some((ret, format!("{}::{}", cd.name, m.name)))
            }
            // Issue #128: closures carry their adopted docblock on the scope itself
            // (`Scope::docblock`) — same `parse_envelopes` grammar as free
            // functions. No enclosing-class `@template` shadowing here (known
            // limitation: a closure inside a templated class could misread a
            // template name as a class name).
            ScopeOwner::Closure { def_offset } => {
                let ret =
                    self.envelopes_of(scope.docblock.as_deref(), self.cur, *def_offset)?.ret?;
                Some((ret, "closure".to_owned()))
            }
            // A hooked property's docblock is the *property's*, and `@var` is what it
            // spells — not `@return`. Adopting it as a `get` hook's return envelope is
            // a separate question (issue #544 leaves it dark).
            ScopeOwner::PropertyHook { .. } => None,
        }
    }
}
