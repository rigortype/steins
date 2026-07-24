//! The inference engine — now whole-project (cross-file) resolution.
//!
//! It implements the proof-layer diagnostics (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch`, plus the
//! effect-envelope checks. A call to a **user-defined function or method
//! resolved anywhere in the project** that passes a **literal** argument which
//! **provably** raises a runtime `TypeError` under PHP 8.1+ semantics
//! (ADR-0011), honoring the calling file's `declare(strict_types=1)`, is
//! flagged. Everything not provable is silent.
//!
//! Name resolution follows PHP semantics conservatively (ADR-0001): fully-
//! qualified / qualified / unqualified names resolve against a project symbol
//! index ([`steins_db::project_index`]) plus the builtin catalog, with
//! `use` imports and the namespace/global fallback applied. Ambiguous symbols
//! (duplicate FQN, builtin-shadowing) are never resolved — silent.
//!
//! The single-file entry points ([`check`], [`check_file`], [`diagnostics`])
//! run over a one-file project, so every same-file soundness guard keeps
//! working unchanged; [`check_project`] / [`annotate_project`] run over many.

pub mod dam;
pub mod promote;
pub mod suppress;

pub use dam::{DamFacts, DamKind, DamSite, dam_facts};
pub use suppress::{
    DIAGNOSTIC_IDS, DIAGNOSTIC_REGISTRY, FACET_ORIGIN, Facet, InlineOutcome, Layer, Origin,
    SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, apply_inline_ignores, declared_facet, layer,
    pattern_is_known, pattern_matches,
};

use std::collections::{HashMap, HashSet};

use steins_contract::ContractTy;
use steins_contract::normalize;
use steins_db::{Db, DeclSite, Project, ProjectIndex, Resolve, SourceFile, parse, project_index};
use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};
use steins_syntax::CallExpr;
use steins_syntax::Span;
use steins_syntax::{
    ArgValue, ArrayKey, Callee, CatchClause, ClassDecl, ClosureRef, CmpOp, CondExpr, CondOperand,
    EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl, MatchArmT, MethodDecl, NameRef,
    NativeType, NormKey, Param, PropertyDecl, Receiver, RefKind, ScalarType, Scope, ScopeOwner, SourceTree,
    StaticClass, Stmt, StmtKind, ThrowKind, ThrowOrigin, TypeMember, Visibility, normalize_array,
    php_canonical_int_string,
};

use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, TypeKind as PKind};
use steins_phpdoc::{AssertKind, TagKind, Type as PType, parse_type, scan_docblock};

/// The registry id for the `type.argument-mismatch` proof-layer check (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

/// The registry id for the `type.return-mismatch` proof-layer check (ADR-0022):
/// a function/method whose return type is a native scalar/union and one of its
/// (trace-visible) `return <literal>;` statements provably raises a `TypeError`.
pub const RETURN_ID: &str = "type.return-mismatch";

/// The registry id for the phpdoc declared-contract param check (ADR-0030 relation
/// #1): a proven value flowing into a parameter with a `@param` phpdoc envelope
/// that it provably does **not** inhabit under contract (set) acceptance — no
/// coercion (a numeric string `"5"` does not satisfy `int` here). Distinct from the
/// runtime relation ([`ID`]); phpdoc types are never enforced at runtime.
pub const PARAM_MISMATCH_ID: &str = "phpdoc.param-mismatch";

/// The registry id for the phpdoc declared-contract return check (ADR-0030): a
/// proven `return <value>;` that provably does not inhabit the `@return` envelope
/// under contract acceptance.
pub const RETURN_MISMATCH_ID: &str = "phpdoc.return-mismatch";

/// The registry id for the branch-sensitive null-dereference proof (ADR-0031
/// stage 1): a method call whose receiver variable is **proven `null`** on the
/// current path (e.g. inside `if ($u === null) { $u->name(); }`) — a guaranteed
/// runtime `Error` ("Call to a member function on null"). Only a *`Singleton(null)`*
/// receiver fires; a `OneOf` that merely *includes* null is `Maybe` → silent.
pub const CALL_ON_NULL_ID: &str = "call.on-null";

/// The registry id for the native property-type check (ADR-0036): a proven value
/// assigned to a native-typed property provably raises a `TypeError` under the
/// assigning file's strict mode (`$x->p = "abc"` on `int $p`).
pub const PROP_MISMATCH_ID: &str = "type.property-mismatch";

/// The registry id for the phpdoc `@var` property-contract check (ADR-0036/0030):
/// a proven or abstract value assigned to a property provably does not inhabit its
/// `@var` contract type (definite `No` only; no double-report where native fired).
pub const PHPDOC_PROP_MISMATCH_ID: &str = "phpdoc.property-mismatch";

/// The registry id for the readonly-reassignment proof (ADR-0036): a second proven
/// write to a `readonly` property on one path — a guaranteed runtime `Error`.
pub const READONLY_REASSIGNED_ID: &str = "readonly.reassigned";

/// The registry id for the effect-envelope check (ADR-0005/0022): a function
/// declared `#[\Steins\Pure]` / `#[\Steins\Effect(...)]` whose inferred effects
/// exceed the declared envelope (ADR-0018 prefix subsumption).
pub const EFFECT_ID: &str = "effect.envelope-exceeded";

/// The one unified trinary judgment (ADR-0031), defined in `steins-domain`
/// and re-exported here: condition evaluation in the branch walk, phpdoc
/// contract acceptance (ADR-0030), and the domain's own fact queries all
/// speak the same `Certainty`.
pub use steins_domain::Certainty;

/// The registry id for the unknown-effect-label check (ADR-0018/0022): a declared
/// `#[\Steins\Effect(...)]` label that is not in the label registry
/// ([`steins_catalog::is_known_label`]) — a typo or an unregistered private label.
pub const UNKNOWN_LABEL_ID: &str = "effect.unknown-label";

/// The registry id for the `@throws` envelope check (ADR-0040/0007): a **checked**
/// exception that **provably escapes** (`Yes`) a function/method whose docblock
/// declares `@throws`, and is a subclass of **none** of the declared classes. Only
/// proven escapes report; `Maybe`-escape and unknown-hierarchy stay silent (the
/// consumer-inverted safe side of ADR-0040).
pub const THROW_UNDECLARED_ID: &str = "throw.undeclared";

/// The registry id for the Liskov throw-widening check (ADR-0033/0040 rule 4): an
/// override/implementation whose declared `@throws` names a checked class that is
/// a subclass of none of the parent method's declared `@throws` classes. Fires
/// only when **both** sides declare `@throws`; `Maybe` resolution stays silent.
pub const THROW_LISKOV_ID: &str = "throw.liskov-widened";

/// The registry id for the Liskov effect-widening check (ADR-0033 point 5): a
/// project method whose **proven** inferred effects exceed the effect envelope
/// (`#[\Steins\Pure]` / `#[\Steins\Effect(...)]`) declared on the abstraction it
/// overrides or implements (a parent class or interface method). Implementations
/// may be purer, never less pure; the exhaustiveness-tainted (unknown) remainder
/// stays silent — only the proven subset judges.
pub const EFFECT_LISKOV_ID: &str = "effect.liskov-widened";

// ---------------------------------------------------------------------------
// The finding-breadth family (ADR-0049): absence-proof ids. Each lights up in its
// own stage (S2–S6); `call.undefined-method` is live (S2), the rest are registered
// ahead of emission. A not-yet-emitted id lives in `REGISTERED_NOT_YET_EMITTED`,
// not `ALL_EMITTABLE_IDS`, until its emit site lands; the totality test binds them.
// ---------------------------------------------------------------------------

/// The registry id for the undefined-function check (ADR-0049 §3, proof layer): a
/// call to a function no candidate FQN defines and the sidecar reports not-found,
/// with the dam clear. Emitted from S4.
pub const CALL_UNDEFINED_FUNCTION_ID: &str = "call.undefined-function";

/// The registry id for the undefined-method check (ADR-0049 §4, proof layer): a
/// method call on a proven-exact receiver whose fully-enumerated hierarchy defines
/// no such method, with no `__call`/trait obstacle. Emitted from S2.
pub const CALL_UNDEFINED_METHOD_ID: &str = "call.undefined-method";

/// The registry id for the undefined-class check (ADR-0049 §5, proof layer): a
/// class reference at a hard-error position (`new`, static call, class-const /
/// static-property fetch) whose FQN is absent from the index, the builtin
/// hierarchy, and the sidecar, with the dam clear. Emitted from S4.
pub const CLASS_UNDEFINED_ID: &str = "class.undefined";

/// The registry id for the too-few-arguments check (ADR-0049 §6, proof layer): a
/// uniquely-resolved call passing fewer positional arguments than the target's
/// required parameters (always `ArgumentCountError`). Emitted from S5.
pub const CALL_TOO_FEW_ARGUMENTS_ID: &str = "call.too-few-arguments";

/// The registry id for the too-many-arguments check (ADR-0049 §6, proof layer):
/// extra arguments to an **internal** non-variadic target (userland silently
/// ignores them — never a finding). Emitted from S5.
pub const CALL_TOO_MANY_ARGUMENTS_ID: &str = "call.too-many-arguments";

/// The registry id for the unknown-named-argument check (ADR-0049 §6, proof
/// layer): a named argument binding no parameter of a resolved non-variadic target
/// (fatal `Error`). Emitted from S5.
pub const CALL_UNKNOWN_NAMED_ARGUMENT_ID: &str = "call.unknown-named-argument";

/// The registry id for the missing-offset check (ADR-0049 §7, proof layer): a read
/// of a key provably absent from a proven container value (`Undefined array key`).
/// Emitted from S3.
pub const OFFSET_MISSING_ID: &str = "offset.missing";

/// The registry id for the offset-on-unsupported check (ADR-0049 §7, proof layer):
/// an offset read on a proven non-offsetable base (object → fatal `Error`;
/// scalar/null → warning). Emitted from S3.
pub const OFFSET_ON_UNSUPPORTED_ID: &str = "offset.on-unsupported";

/// The registry id for the declared-receiver undefined-method check (ADR-0049 §8,
/// **contract** layer): a method absent on a phpdoc-declared receiver narrowed by
/// branch analysis, under descendant closure. Emitted from S6.
pub const PHPDOC_UNDEFINED_METHOD_ID: &str = "phpdoc.undefined-method";

// ---------------------------------------------------------------------------
// The dump surface (ADR-0053): the **debug** layer's three ids — requested
// introspection, an "answered question" (§1). All three carry `Layer::Debug` and
// are registered ahead of emission (the S1 pattern): they live in
// `REGISTERED_NOT_YET_EMITTED` through the D1 groundwork and move to
// `ALL_EMITTABLE_IDS` when their emitter lands (the explicit pair at D3, `var_dump`
// at D4). No emit site exists yet — D1 is zero behavior.
// ---------------------------------------------------------------------------

/// The registry id for the explicit `PHPStan\dumpType($e)` dump (ADR-0053 §2, debug
/// layer): renders the walk's best knowledge of `$e` at the call position (the trust
/// order — proven value beats membership beats declared arms). Fail-level, fixed:
/// the named function does not exist at runtime, so a committed call is a guaranteed
/// fatal (§3). Emitted from D3.
pub const DEBUG_TYPE_ID: &str = "debug.type";

/// The registry id for the explicit `PHPStan\dumpPhpDocType($e)` dump (ADR-0053 §2,
/// debug layer): renders the **contract-fact arm list** (the declared envelope as
/// narrowed by guards) — the declared-side view. Fail-level, fixed (§3). Emitted
/// from D3.
pub const DEBUG_PHPDOC_TYPE_ID: &str = "debug.phpdoc-type";

/// The registry id for the default-on `var_dump($e)` dump (ADR-0053 §2, debug
/// layer): one `debug.type`-shaped report per argument expression. Warn-level,
/// fixed — exit-neutral forever (§3), profile-disableable (§4). Emitted from D4.
pub const DEBUG_VAR_DUMP_ID: &str = "debug.var-dump";

/// The resolved FQN of `PHPStan\dumpType` (ADR-0053 §2), lowercase-normalized and
/// leading-`\`-stripped — the case-insensitive matching key (PHP function names are
/// case-insensitive).
pub const DUMP_TYPE_FQN: &str = "phpstan\\dumptype";

/// The resolved FQN of `PHPStan\dumpPhpDocType` (ADR-0053 §2), lowercase-normalized.
pub const DUMP_PHPDOC_TYPE_FQN: &str = "phpstan\\dumpphpdoctype";

/// The reserved dump-family FQNs (ADR-0053 §5): the explicit pair, recognized
/// **unconditionally by resolved FQN** — definition-insensitive (a userland
/// definition of the name does not stand recognition down) and case-insensitive.
///
/// **S4 carve-out (ADR-0053 §6), recorded here so it cannot drift:** the future
/// `call.undefined-function` recognizer (S4, not yet landed) MUST consult this set
/// and exclude a call whose resolved FQN matches — a recognized dump already reds CI
/// at that site with a fail-level `debug.type` whose message says what to do, so a
/// second `call.undefined-function` finding for one deletable line is noise. When S4
/// lands, its emitter reads `DUMP_FQNS` (or calls [`is_dump_family_fqn`]) directly,
/// so the exclusion is one source of truth. A pinned fixture in `tests/dump_surface.rs`
/// (`dump_pair_is_recognized_by_resolved_fqn`) guards the recognizer meanwhile.
pub const DUMP_FQNS: &[&str] = &[DUMP_TYPE_FQN, DUMP_PHPDOC_TYPE_FQN];

/// Whether `fqn` (lowercase-normalized, leading `\` stripped) is a reserved
/// dump-family FQN (ADR-0053 §5/§6). The single predicate the dump recognizer and
/// the future S4 carve-out share.
#[must_use]
pub fn is_dump_family_fqn(fqn: &str) -> bool {
    DUMP_FQNS.contains(&fqn)
}

/// Every id constant that reaches a `Diagnostic { id: … }` construction site — the
/// canonical enumeration of what the emitters can produce (ADR-0050 §2 totality).
///
/// **Invariant, checked by the workspace totality test** (`tests/registry.rs`):
/// this list and [`DIAGNOSTIC_REGISTRY`] are the same set, both directions — so a
/// new emitter whose id is added here but not registered (or the reverse) fails to
/// build the tests. Adding a `*_ID` constant and emitting it therefore *forces*
/// both a registry entry (with a layer) and an entry here. The two live in
/// different files on purpose: the registry carries the layer attribute, this
/// carries "is emitted", and the test binds them.
///
/// `SUPPRESS_UNMATCHED_ID` / `SUPPRESS_UNKNOWN_ID` are emitted from
/// [`suppress`] and so are covered via the registry side of the test.
pub const ALL_EMITTABLE_IDS: &[&str] = &[
    ID,
    RETURN_ID,
    CALL_ON_NULL_ID,
    PROP_MISMATCH_ID,
    READONLY_REASSIGNED_ID,
    PARAM_MISMATCH_ID,
    RETURN_MISMATCH_ID,
    PHPDOC_PROP_MISMATCH_ID,
    THROW_UNDECLARED_ID,
    THROW_LISKOV_ID,
    EFFECT_ID,
    EFFECT_LISKOV_ID,
    UNKNOWN_LABEL_ID,
    // The finding-breadth flagship, lit up at ADR-0049 S2 (the first absence id to
    // fire). Its emitter is `check_undefined_method`; the rest of the family stays in
    // `REGISTERED_NOT_YET_EMITTED` until its own stage.
    CALL_UNDEFINED_METHOD_ID,
    // The offset family, lit up at ADR-0049 S3 (`check_offset_read`): a value-domain
    // proof over proven container values under the read-context whitelist.
    OFFSET_MISSING_ID,
    OFFSET_ON_UNSUPPORTED_ID,
    // The declared-receiver lane, lit up at ADR-0049 S6 (`check_phpdoc_undefined_method`):
    // the contract-layer method-absence claim over N4's narrowed contract-arm lists,
    // under per-arm descendant closure.
    PHPDOC_UNDEFINED_METHOD_ID,
    // The userland arity arms, lit up at ADR-0049 S5 (`check_arity`): too-few and
    // unknown-named on a uniquely-resolved userland function or a proven-exact
    // receiver's method/constructor/static. The too-many arm (internal targets
    // only) and the internal-target arity stay in `REGISTERED_NOT_YET_EMITTED`
    // until the reflect slice (M2).
    CALL_TOO_FEW_ARGUMENTS_ID,
    CALL_UNKNOWN_NAMED_ARGUMENT_ID,
    // The dump surface's ids (ADR-0053), all lit up from `emit_dumps` (the walk's
    // call-handling arm): the explicit pair `debug.type` / `debug.phpdoc-type` (D3),
    // recognized by resolved FQN, and `debug.var-dump` (D4), recognized by the PHP
    // fallback rule — default-on, one report per argument.
    DEBUG_TYPE_ID,
    DEBUG_PHPDOC_TYPE_ID,
    DEBUG_VAR_DUMP_ID,
    suppress::SUPPRESS_UNMATCHED_ID,
    suppress::SUPPRESS_UNKNOWN_ID,
];

/// Ids that are **registered ahead of emission** (ADR-0049 S1 groundwork): the
/// finding-breadth family's ids exist in [`DIAGNOSTIC_REGISTRY`] — so
/// `@steins-ignore` can name them and their layer is pinned — but no emitter
/// produces them yet. Each later stage (S2–S6) that lights up an id **moves** it
/// from here into [`ALL_EMITTABLE_IDS`].
///
/// The totality test (`tests/registry.rs`) enforces the reconciliation so it
/// cannot rot silently: every registered id must be in `ALL_EMITTABLE_IDS ∪
/// REGISTERED_NOT_YET_EMITTED`, the two lists are **disjoint** (an id emitted for
/// the first time must leave this list — it cannot be both), and every id here must
/// actually be registered. An emitted id that is not in `ALL_EMITTABLE_IDS` still
/// fails the forward-totality check exactly as before — this list only carves out
/// the not-yet-emitted registry entries, never the emitted-but-unregistered defect.
pub const REGISTERED_NOT_YET_EMITTED: &[&str] = &[
    CALL_UNDEFINED_FUNCTION_ID,
    // CALL_UNDEFINED_METHOD_ID lit up at S2 — now in ALL_EMITTABLE_IDS.
    CLASS_UNDEFINED_ID,
    // CALL_TOO_FEW_ARGUMENTS_ID / CALL_UNKNOWN_NAMED_ARGUMENT_ID lit up at S5 (the
    // userland arms) — now in ALL_EMITTABLE_IDS. The too-many arm fires for
    // INTERNAL targets only (userland too-many runs clean — never a finding), so it
    // waits for the reflect slice (M2).
    CALL_TOO_MANY_ARGUMENTS_ID,
    // OFFSET_MISSING_ID / OFFSET_ON_UNSUPPORTED_ID lit up at S3 — now in ALL_EMITTABLE_IDS.
    // PHPDOC_UNDEFINED_METHOD_ID lit up at S6 — now in ALL_EMITTABLE_IDS.
    // The dump surface's debug ids (ADR-0053) all lit up: the explicit pair at D3 and
    // `debug.var-dump` at D4 — all now in ALL_EMITTABLE_IDS.
];

/// The maximum depth of interprocedural argument-binding descent (Feature B).
///
/// ADR-0009 makes inference cutoffs a first-class budget discipline: a chain of
/// calls propagating a literal is followed at most this many frames deep, after
/// which the descent stops with **no** diagnostic (a cutoff names itself as
/// silence, never a manufactured finding). Direct and indirect recursion is
/// caught earlier by the on-stack binding set; this bound guards against merely
/// long, non-cyclic chains.
pub const MAX_BINDING_DEPTH: usize = 8;

/// The one-line coverage-posture notice (ADR-0004): printed to stderr when a run
/// executes as the sound subset because the PHP sidecar is unavailable.
pub const SOUND_SUBSET_NOTICE: &str =
    "note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted";

// ---------------------------------------------------------------------------
// Folding seam (ADR-0004 / ADR-0024). Unchanged from the per-file slice.
// ---------------------------------------------------------------------------

/// Something that can fold a builtin call to a concrete literal value, and (from
/// ADR-0049 S2) answer the runtime boot surface for the absence-proof family.
pub trait Folder {
    /// Fold `name(args...)` to a literal, or `None` to widen.
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue>;

    /// Whether the absence-proof family (ADR-0049) may fire **at all** this run.
    /// `true` only when a live PHP sidecar is answering the boot surface *and* no
    /// runtime-redefinition extension (`uopz`/`runkit7`/`Componere`, ADR-0049 A9)
    /// is loaded — with any such extension present, no absence claim holds. The
    /// default is `false`: the sound subset (ADR-0004) keeps every absence id
    /// silent when there is no sidecar to ask (A2ii's honest consequence — the
    /// homonym question has no textual answer).
    fn absence_family_available(&mut self) -> bool {
        false
    }

    /// Ask the project's own PHP whether `fqn` is a resident builtin/extension
    /// class-like — the ADR-0049 A2ii **homonym** leg. `Some(true)` — a boot-surface
    /// homonym stands, so the textual twin may be dead code shadowed by the loaded
    /// class (silence); `Some(false)` — definitively absent from the boot surface;
    /// `None` — unanswerable (no sidecar / a mid-run failure ⇒ Unknown ⇒ silence).
    /// The default is `None` (the sound subset). `fqn` is the index's
    /// lowercase-normalized form; PHP's class-existence predicates are
    /// case-insensitive, so the lowercased name is a faithful query.
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        let _ = fqn;
        None
    }

    /// Ask the project's own PHP whether `fqn` is a resident builtin/extension
    /// **function** — the arity family's A2ii homonym leg (ADR-0049 §6). A user
    /// function that shares a name with a boot-surface function is only bound to
    /// the indexed signature when the userland declaration actually executes (the
    /// `function_exists`-guarded polyfill shadowed by a loaded extension is the
    /// live counterexample); `Some(true)` therefore forces silence. `Some(false)`
    /// — definitively absent from the boot surface; `None` — unanswerable (no
    /// sidecar / a mid-run failure ⇒ silence). The default is `None`. `fqn` is the
    /// index's lowercase-normalized form; PHP function names are case-insensitive,
    /// so the lowercased name is a faithful query.
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        let _ = fqn;
        None
    }

    /// The project's own PHP `(major, minor)` from the sidecar `env()` — the
    /// ADR-0052 A11 version-skew input. `None` (the default / sound subset) when no
    /// sidecar answers: an unknown minor is treated as "no detectable skew", so the
    /// catalog pin stands and arm deletion behaves exactly as it did before A11.
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        None
    }
}

/// The runtime-redefinition extensions that void the absence family (ADR-0049 A9):
/// with any of them loaded, a defined class can gain a method and a missing name
/// can be minted at runtime, so no absence claim holds. Matched case-insensitively
/// against the sidecar's loaded-extension list.
const MONKEY_PATCH_EXTENSIONS: &[&str] = &["uopz", "runkit7", "runkit", "componere"];

/// The sound-subset folder: never folds anything. This is what the salsa
/// [`diagnostics`] query uses, keeping that query deterministic.
pub struct NoFold;

impl Folder for NoFold {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
}

/// A [`Folder`] backed by a lazily-spawned PHP [`Sidecar`], with a per-run memo
/// so a repeated `(name, args)` never triggers duplicate IPC.
pub struct SidecarFolder {
    sidecar: Option<Sidecar>,
    memo: HashMap<(String, Vec<ArgValue>), Option<ArgValue>>,
    disabled: bool,
    spawn_failed: bool,
    notified: bool,
    /// Cached ADR-0049 A9 verdict: whether the absence family is available (a live
    /// sidecar and no monkey-patch extension). Computed once from the sidecar's
    /// `env` and then memoized — a whole-run property (ADR-0048 query answer).
    absence_available: Option<bool>,
    /// Per-FQN memo of the A2ii homonym oracle so a repeated chain class never
    /// triggers duplicate `reflect` IPC.
    boot_surface_memo: HashMap<String, Option<bool>>,
    /// Per-FQN memo of the arity family's function-homonym oracle (ADR-0049 §6),
    /// the function-namespace analogue of [`Self::boot_surface_memo`].
    boot_surface_fn_memo: HashMap<String, Option<bool>>,
    /// Memoized project PHP `(major, minor)` from the sidecar `env()` (ADR-0052
    /// A11) — a whole-run query answer. `Some(None)` records "asked, unanswerable".
    php_minor: Option<Option<(u16, u16)>>,
}

impl SidecarFolder {
    /// Create a folder. `disabled` (the CLI's `--no-php`) makes it a permanent
    /// no-op that never spawns PHP.
    #[must_use]
    pub fn new(disabled: bool) -> Self {
        Self {
            sidecar: None,
            memo: HashMap::new(),
            disabled,
            spawn_failed: false,
            notified: true, // suppress our own notice; only spawn-failure re-arms it.
            absence_available: None,
            boot_surface_memo: HashMap::new(),
            boot_surface_fn_memo: HashMap::new(),
            php_minor: None,
        }
    }

    /// Create an enabled folder that will emit the sound-subset notice itself if
    /// it cannot spawn PHP.
    #[must_use]
    pub fn enabled() -> Self {
        Self { notified: false, ..Self::new(false) }
    }

    /// Ensure a live sidecar, or record that we cannot have one.
    fn ensure_sidecar(&mut self) -> Option<&mut Sidecar> {
        if self.disabled || self.spawn_failed {
            return None;
        }
        if self.sidecar.is_none() {
            match Sidecar::spawn() {
                Ok(sc) => self.sidecar = Some(sc),
                Err(_) => {
                    self.spawn_failed = true;
                    if !self.notified {
                        eprintln!("{SOUND_SUBSET_NOTICE}");
                        self.notified = true;
                    }
                    return None;
                }
            }
        }
        self.sidecar.as_mut()
    }
}

impl Folder for SidecarFolder {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        let key = (name.to_owned(), args.to_vec());
        if let Some(cached) = self.memo.get(&key) {
            return cached.clone();
        }
        let folded = self.ensure_sidecar().and_then(|sc| {
            let fargs: Vec<FoldArg> = args.iter().filter_map(arg_to_fold).collect();
            if fargs.len() != args.len() {
                return None;
            }
            match sc.fold(name, &fargs) {
                FoldResult::Value(v) => fold_value_to_arg(&v),
                FoldResult::Throw { .. } | FoldResult::Widen { .. } => None,
            }
        });
        self.memo.insert(key, folded.clone());
        folded
    }

    fn absence_family_available(&mut self) -> bool {
        if let Some(cached) = self.absence_available {
            return cached;
        }
        // No live sidecar ⇒ the family is silent (the ADR-0004 sound subset covers
        // it — A2ii). Otherwise consult the loaded-extension list once (A9).
        let verdict = match self.ensure_sidecar().and_then(Sidecar::env) {
            Some(env) => !env.extensions.iter().any(|e| {
                MONKEY_PATCH_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m))
            }),
            None => false,
        };
        self.absence_available = Some(verdict);
        verdict
    }

    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        if let Some(cached) = self.boot_surface_memo.get(fqn) {
            return *cached;
        }
        let answer = self
            .ensure_sidecar()
            .and_then(|sc| sc.reflect(fqn))
            .map(|r| r.class_like_exists);
        self.boot_surface_memo.insert(fqn.to_owned(), answer);
        answer
    }

    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        if let Some(cached) = self.boot_surface_fn_memo.get(fqn) {
            return *cached;
        }
        let answer = self
            .ensure_sidecar()
            .and_then(|sc| sc.reflect(fqn))
            .map(|r| r.function_exists);
        self.boot_surface_fn_memo.insert(fqn.to_owned(), answer);
        answer
    }

    fn php_minor(&mut self) -> Option<(u16, u16)> {
        if let Some(cached) = self.php_minor {
            return cached;
        }
        // Parse the sidecar-reported `php_version` (`"8.5.8"`) to `(major, minor)`;
        // an unparseable / absent report stays `None` (no detectable skew — A11).
        let answer = self.ensure_sidecar().and_then(Sidecar::env).and_then(|e| parse_php_minor(&e.php_version));
        self.php_minor = Some(answer);
        answer
    }
}

/// Parse a PHP version string (`"8.5.8"`, `"8.5.8-dev"`) to `(major, minor)`.
/// `None` when the first two dotted components are not both integers.
fn parse_php_minor(v: &str) -> Option<(u16, u16)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor_part = it.next()?;
    // A minor like `5` or `5-dev`: take the leading digit run.
    let minor: u16 = minor_part.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    Some((major, minor))
}

/// Convert a literal [`ArgValue`] to a [`FoldArg`]; non-literals yield `None`.
fn arg_to_fold(arg: &ArgValue) -> Option<FoldArg> {
    match arg {
        ArgValue::Int(v) => Some(FoldArg::Int(*v)),
        ArgValue::Float(v) => Some(FoldArg::Float(*v)),
        ArgValue::Str(v) => Some(FoldArg::Str(v.clone())),
        ArgValue::Bool(v) => Some(FoldArg::Bool(*v)),
        ArgValue::Null => Some(FoldArg::Null),
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::New(..)
        | ArgValue::Array(_)
        | ArgValue::Ternary { .. }
        | ArgValue::Closure(_)
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        | ArgValue::Coalesce(..)
        | ArgValue::OffsetRead { .. }
        // Object-world values (ADR-0043) are not fold arguments — unproven, == Other.
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::Other => None,
    }
}

/// Convert a folded value back to a literal [`ArgValue`].
fn fold_value_to_arg(value: &FoldValue) -> Option<ArgValue> {
    Some(match value {
        FoldValue::Int(v) => ArgValue::Int(*v),
        FoldValue::Float(v) => ArgValue::Float(*v),
        FoldValue::Str(v) => ArgValue::Str(v.clone()),
        FoldValue::Bool(v) => ArgValue::Bool(*v),
        FoldValue::Null => ArgValue::Null,
    })
}

/// Whether a diagnostic path lies inside a `vendor/` directory (ADR-0015).
///
/// Vendor code is fully indexed and inferred (shapes/values/effects flow
/// through it), but its diagnostics are off by default: a finding whose path has
/// a `vendor` **directory component** — a top-level `vendor/…` or any nested
/// `…/vendor/…` — is vendor. The match is on whole path components (split on
/// both `/` and `\`), so a sibling like `vendor_proj/` or a file named
/// `vendor.php` is *not* vendor. The trailing filename can never equal `vendor`
/// (it carries a `.php` extension), so a bare component test is exact.
pub fn is_vendor_path(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "vendor")
}

/// A proof-layer finding. Kept deliberately flat so the CLI can render text or
/// JSON without knowing anything about the analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub id: &'static str,
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
    /// The registry-declared facet this finding carries (ADR-0050 §4), or `None`
    /// for ids that declare no facet. v1: `Some(Facet::Origin(_))` on
    /// `throw.undeclared` only, computed at emit time from walk-local data (the
    /// measurement note's same-file-plus-own-origin rule). Additive — the
    /// `--format json` output shows it as an extra key only when present, and the
    /// value never participates in a check's inference behavior. It *does* take
    /// part in equality/hash, but harmlessly: two findings that were previously
    /// equal share an origin file+offset and so compute the same facet.
    pub facet: Option<Facet>,
}

// ---------------------------------------------------------------------------
// The project view: the files analyzed together and their symbol index.
// ---------------------------------------------------------------------------

/// One file in the analyzed project: its diagnostic path and its lowered tree.
/// The tree owns everything else the analysis needs (functions, classes,
/// scopes, positions, namespace contexts).
#[derive(Clone, Copy)]
pub struct FileUnit<'a> {
    pub path: &'a str,
    pub tree: &'a SourceTree,
}

/// A declaration's position within the project view: the file's index in the
/// [`FileUnit`] slice, and the declaration's index in that file's list.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Site {
    file: usize,
    index: usize,
}

/// The outcome of resolving an FQN against the in-memory project index.
#[derive(Clone, Copy)]
enum Res {
    Absent,
    Unique(Site),
    Ambiguous,
}

/// The project symbol index in the analysis's own `Site` terms (a file *index*,
/// not a salsa handle). Built either directly from the [`FileUnit`] slice
/// (single-file / test paths) or adapted from the salsa [`ProjectIndex`]
/// (the db-backed [`check_project`] path — so the tracked query is the authority
/// on incrementality, ADR-0009).
#[derive(Default)]
struct Index {
    functions: HashMap<String, Site>,
    ambiguous_functions: HashSet<String>,
    classes: HashMap<String, Site>,
    ambiguous_classes: HashSet<String>,
    fn_by_simple: HashMap<String, Vec<Site>>,
}

impl Index {
    /// Build the index straight from the file units (mirrors the db query).
    fn from_units(units: &[FileUnit]) -> Self {
        let mut idx = Index::default();
        for (fi, u) in units.iter().enumerate() {
            for (i, f) in u.tree.functions().iter().enumerate() {
                let site = Site { file: fi, index: i };
                idx.fn_by_simple.entry(f.name.to_ascii_lowercase()).or_default().push(site);
                insert_unique(&mut idx.functions, &mut idx.ambiguous_functions, &f.fqn, site);
            }
            for (i, c) in u.tree.classes().iter().enumerate() {
                let site = Site { file: fi, index: i };
                insert_unique(&mut idx.classes, &mut idx.ambiguous_classes, &c.fqn, site);
            }
        }
        // Literal `class_alias` edges (ADR-0049 §2 / A2iii) fold in after every
        // textual decl, mirroring the db-backed `project_index`. Targets resolve
        // against the textual snapshot (order-independent, ADR-0048); collisions
        // (alias vs textual, or two aliases for one name) demote to `Ambiguous`.
        let mut resolved: Vec<(String, Site)> = Vec::new();
        for u in units {
            for edge in u.tree.class_alias_edges() {
                if idx.ambiguous_classes.contains(&edge.target_fqn) {
                    continue;
                }
                if let Some(&target) = idx.classes.get(&edge.target_fqn) {
                    resolved.push((edge.alias_fqn.clone(), target));
                }
            }
        }
        for (alias_fqn, target) in resolved {
            insert_unique(&mut idx.classes, &mut idx.ambiguous_classes, &alias_fqn, target);
        }
        idx
    }

    /// Adapt the salsa [`ProjectIndex`] to `Site`s, using `pos` to map each
    /// [`SourceFile`] to its position in the (identically ordered) unit slice.
    fn from_db(db_index: &ProjectIndex, pos: &HashMap<SourceFile, usize>) -> Self {
        let site = |ds: &DeclSite| Site { file: pos[&ds.file], index: ds.index };
        let mut idx = Index::default();
        for (fqn, ds) in db_index.functions() {
            idx.functions.insert(fqn.clone(), site(ds));
        }
        for (fqn, ds) in db_index.classes() {
            idx.classes.insert(fqn.clone(), site(ds));
        }
        idx.ambiguous_functions = db_index.ambiguous_functions().clone();
        idx.ambiguous_classes = db_index.ambiguous_classes().clone();
        for (simple, sites) in db_index.fn_by_simple() {
            idx.fn_by_simple.insert(simple.clone(), sites.iter().map(site).collect());
        }
        idx
    }

    fn resolve_function(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_functions.contains(&key) {
            Res::Ambiguous
        } else {
            self.functions.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    fn resolve_class(&self, fqn: &str) -> Res {
        let key = fqn.to_ascii_lowercase();
        if self.ambiguous_classes.contains(&key) {
            Res::Ambiguous
        } else {
            self.classes.get(&key).copied().map_or(Res::Absent, Res::Unique)
        }
    }

    fn unique_fn_by_simple(&self, simple: &str) -> Option<Site> {
        match self.fn_by_simple.get(&simple.to_ascii_lowercase()) {
            Some(sites) if sites.len() == 1 => Some(sites[0]),
            _ => None,
        }
    }

    fn has_simple_function(&self, simple: &str) -> bool {
        self.fn_by_simple.contains_key(&simple.to_ascii_lowercase())
    }
}

/// Whether a function-call reference resolves to a **user** function defined in
/// the project (as opposed to a builtin, or an unresolved/ambiguous name),
/// applying PHP name resolution against the salsa [`ProjectIndex`]. Public so
/// tooling (`xtask freq`) can exclude userland cross-file calls from the
/// builtin-frequency ranking. A name is "userland" here if the project uniquely
/// defines it at any candidate FQN the reference could denote — the
/// builtin-shadow nuance the checker applies is irrelevant to this question.
#[must_use]
pub fn resolves_to_user_function(index: &ProjectIndex, tree: &SourceTree, r: &NameRef) -> bool {
    let unique = |fqn: &str| matches!(index.resolve_function(fqn), Resolve::Unique(_));
    match r.kind {
        RefKind::FullyQualified => unique(&r.raw.to_ascii_lowercase()),
        RefKind::Qualified => {
            let ctx = tree.ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            unique(&fqn)
        }
        RefKind::Unqualified => {
            let ctx = tree.ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            if let Some(t) = ctx.fn_imports.get(&name) {
                return unique(&t.to_ascii_lowercase());
            }
            if !ctx.namespace.is_empty() && unique(&format!("{}\\{}", ctx.namespace, name)) {
                return true;
            }
            unique(&name)
        }
    }
}

/// Insert `fqn → site`, demoting to ambiguity on any collision.
fn insert_unique(
    map: &mut HashMap<String, Site>,
    ambiguous: &mut HashSet<String>,
    fqn: &str,
    site: Site,
) {
    if ambiguous.contains(fqn) {
        return;
    }
    if map.remove(fqn).is_some() {
        ambiguous.insert(fqn.to_owned());
    } else {
        map.insert(fqn.to_owned(), site);
    }
}

/// How an unqualified/qualified/FQ **function** call resolves (ADR-0001).
enum FnResolution {
    /// A user function defined in the project (its declaration site).
    User(Site),
    /// A catalogued builtin — no user body, but folding/effect labels apply.
    Builtin,
    /// Ambiguous or unresolved — skip everything (no check, no fold, no effect
    /// classification). The silent side.
    Unknown,
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// The proof-layer diagnostics for one file, as a memoized salsa query (sound
/// subset — [`NoFold`], no PHP). Analyzes the file as a one-file project.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, &mut NoFold, false, true)
}

/// The folding-aware check for one file (run **outside** salsa; ADR-0004),
/// analyzed as a one-file project.
#[must_use]
pub fn check_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, folder, false, true)
}

/// The folding-aware check for a whole **project** (ADR-0009/0015): every file
/// in `project` is analyzed as one unit, so cross-file calls, class chains, and
/// effects resolve. Resolution is driven by the salsa [`project_index`] query.
#[must_use]
pub fn check_project(db: &dyn Db, project: Project, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_project_with_runtime(db, project, folder, false, true)
}

/// [`check_project`] with the `[runtime]` pseudo-constants declared (ADR-0052 §5,
/// ADR-0049 §7): `zend_assertions` promotes `assert($expr)` narrowing to the
/// `Verified` stratum; `warning_handler_abort` (the `warning-handler` posture) is
/// `true` for the default `"abort"` — proven warning-grade offset findings emit —
/// and `false` for `"null"`, which silences them. The default entry point
/// ([`check_project`]) passes `(false, true)`: the safe production defaults.
#[must_use]
pub fn check_project_with_runtime(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    zend_assertions: bool,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos);
    check_units(&units, &index, folder, zend_assertions, warning_handler_abort)
}

/// The pure single-file check (sound subset). Kept for unit tests and callers
/// that never execute PHP. `functions` is accepted for signature stability; the
/// tree's own function list is authoritative.
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
    check_with(tree, functions, path, &mut NoFold)
}

/// The folding-aware single-file check core, analyzed as a one-file project.
#[must_use]
pub fn check_with(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<Diagnostic> {
    let _ = functions; // authoritative list comes from `tree.functions()`
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, folder, false, true)
}

/// The pure single-file check with the `[runtime]` pseudo-constants declared
/// (ADR-0052 §5): `zend_assertions` promotes `assert($expr)` narrowing to the
/// `Verified` stratum. Kept for tests exercising the runtime-config path.
#[must_use]
pub fn check_runtime(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    zend_assertions: bool,
) -> Vec<Diagnostic> {
    let _ = functions;
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, &mut NoFold, zend_assertions, true)
}

/// The single-file check with a folder **and** the full `[runtime]` config
/// (`zend_assertions`, `warning_handler_abort`). Kept for tests that must exercise
/// both a live folder (the offset family is gated on [`Folder::absence_family_available`],
/// ADR-0049 A9) and a chosen `warning-handler` posture (ADR-0049 §7).
#[must_use]
pub fn check_full(
    tree: &SourceTree,
    path: &str,
    folder: &mut dyn Folder,
    zend_assertions: bool,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(&units, &index, folder, zend_assertions, warning_handler_abort)
}

/// The project checking core: direct + propagation passes over every file's
/// calls and scopes, then the one project-wide effects pass.
fn check_units(
    units: &[FileUnit],
    index: &Index,
    folder: &mut dyn Folder,
    zend_assertions: bool,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    // The whole-universe dam fact (ADR-0049 §2): one query answer per run, shared by
    // every file's context. Consumed by the absence family's conditional-decl leg.
    let dam = dam_facts(units);

    // The project PHP minor (ADR-0052 A11): one sidecar `env()` query answer per run,
    // shared by every file's context; drives the catalog version-skew demotion.
    let php_minor = folder.php_minor();

    for fi in 0..units.len() {
        let cx = Cx::new_with(units, index, fi, &dam, zend_assertions, warning_handler_abort, php_minor);

        // --- Propagation pass FIRST: it walks every scope and, as a side
        // product, proves dead regions (decided branches, unreachable tails) —
        // the env-free direct pass below must not report inside them
        // (live-path discipline, ADR-0002/0031). Binding descents contribute
        // nothing here: their deadness is per-binding, not universal. ---------
        let mut dead_spans: Vec<Span> = Vec::new();
        for scope in cx.tree().scopes() {
            analyze_scope(
                &cx,
                folder,
                scope,
                HashMap::new(),
                Store::default(),
                None,
                None,
                None,
                Some(&mut dead_spans),
                &mut out,
            );
        }

        // --- Direct pass: literal / array / `new` arguments at every function
        // call site (env-free; propagation adds `$var`/folded resolution). Native
        // scalar checks and the phpdoc declared-contract check both run here; a
        // site where the native check fired is skipped by the phpdoc check (no
        // double-report; ADR-0030). Calls in proven-dead regions are skipped. ---
        let empty_env: HashMap<String, Known> = HashMap::new();
        let empty_classes: Store = Store::default();
        for call in cx.tree().calls() {
            if in_dead(&dead_spans, call.span.start) {
                continue;
            }
            let Some(site) = cx.resolve_user_fn(call) else { continue };
            let decl = cx.fn_decl(site);
            let envelopes = parse_envelopes(decl.docblock.as_deref());
            for (i, arg) in call.args.iter().enumerate() {
                let Some(param) = decl.params.get(i) else { break };
                if param.variadic {
                    break;
                }
                if param.by_ref {
                    continue;
                }
                let mut native_fired = false;
                // Env-free resolution: a literal, a proven object (`new` / enum
                // case), or a resolved class constant (ADR-0043 stage 3). At file
                // scope there is no enclosing class for `self`/`parent`.
                if let Some(ty) = param.ty.as_ref()
                    && let Some(checkable) = cx.resolve_static_value(&arg.value, None)
                    && is_type_error(&cx, ty, &checkable)
                {
                    out.push(cx.diagnostic(
                        arg.span.start,
                        &checkable,
                        None,
                        &decl.name,
                        &param.name,
                        ty,
                    ));
                    native_fired = true;
                }
                // The direct pass owns env-free arg kinds (literal / array / `new`,
                // plus enum-case / class-const object values — ADR-0043 stage 4);
                // `$var`/`call()` resolution — and their phpdoc check — belong to the
                // propagation pass, so the two never both fire on one arg.
                let env_free = arg.value.is_literal()
                    || matches!(
                        arg.value,
                        ArgValue::Array(_) | ArgValue::New(..) | ArgValue::EnumCase(..) | ArgValue::ClassConst(..)
                    );
                if !native_fired
                    && env_free
                    && let Some(env) = &envelopes
                {
                    check_phpdoc_param(
                        &cx,
                        folder,
                        env,
                        param,
                        site.file,
                        decl.span.start,
                        &decl.name,
                        arg.span.start,
                        &arg.value,
                        &empty_env,
                        &empty_classes,
                        false,
                        false, // in_descent — the direct pass is never a descent
                        &mut out,
                    );
                }
                // Callable-signature variance (issue #11): a closure / first-class
                // callable argument against a signature-bearing `callable(...)`
                // @param. Env-free (a closure's declared signature is a static CST
                // fact), so the direct pass owns it — no overlap with the
                // propagation pass, which owns `$var`/`call()` arg kinds.
                if let ArgValue::Closure(closure) = &arg.value
                    && let Some(env) = &envelopes
                {
                    check_callable_arg(&cx, env, param, &decl.name, arg.span.start, closure, &mut out);
                }
            }
        }

    }

    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(units, index));

    // --- Throw system (ADR-0040/0007): `@throws` envelope + Liskov. ----------
    out.extend(throw_diagnostics(units, index));

    dedup(&mut out);
    out
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}

// ---------------------------------------------------------------------------
// `annotate` facts (ADR-0020): the Rigor-style margin — proven facts only.
// ---------------------------------------------------------------------------

/// One proven fact the `annotate` margin can print against a source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactKind {
    Effects { labels: Vec<String>, exhaustive: bool },
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
            FactKind::Effects { labels, exhaustive } => {
                let mut parts = labels.clone();
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
    annotate_units(&units, &index, 0, folder)
}

/// Salsa-fed single-file annotate.
#[must_use]
pub fn annotate_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<LineFact> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    annotate_units(&units, &index, 0, folder)
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
    let index = Index::from_db(db_index, &pos);
    let Some(target_idx) = handles.iter().position(|&f| f == target) else {
        return Vec::new();
    };
    annotate_units(&units, &index, target_idx, folder)
}

/// Compute the annotate facts for `target` file within a project view.
fn annotate_units(
    units: &[FileUnit],
    index: &Index,
    target: usize,
    folder: &mut dyn Folder,
) -> Vec<LineFact> {
    let mut facts: Vec<LineFact> = Vec::new();

    // 1. Effects (and throws) on each declaration line in the target file.
    for s in effect_summary_units(units, index, target) {
        let throws_present = !s.throws.is_empty() || !s.throws_exhaustive;
        facts.push(LineFact {
            line: s.line,
            kind: FactKind::Effects { labels: s.labels, exhaustive: s.exhaustive },
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
            &mut sink,
        );
    }

    // 3. Findings on the target file (project-wide check, filtered by path).
    let target_path = units[target].path;
    for d in check_units(units, index, folder, false, true) {
        if d.path == target_path {
            facts.push(LineFact { line: d.line, kind: FactKind::Finding { id: d.id } });
        }
    }

    facts.sort_by_key(|f| f.line);
    facts
}

// ---------------------------------------------------------------------------
// Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
// ---------------------------------------------------------------------------

/// One proven effect a unit carries, with the provenance a transitive `via`
/// message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectFinding {
    label: String,
    origin: String,
    line: u32,
    /// The path of the file the origin lives in, so a transitive `via` message
    /// can name the other file when the effect arises cross-file.
    path: String,
}

/// A node in the unified project effect call graph — a free function (keyed by
/// FQN) or a class method (keyed by class FQN + method name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Sym {
    Func(String),
    Method(String, String),
    /// A closure/arrow body (ADR-0033), keyed by file path + definition-site
    /// offset (closures are same-file, so this key is stable within a project).
    Closure(String, u32),
}

/// One unit's fixpoint result: its proven effect findings and exhaustiveness.
#[derive(Debug, Clone, Default)]
struct EffectSet {
    findings: HashSet<EffectFinding>,
    exhaustive: bool,
}

/// Resolve a [`CallbackRef`] to its effect [`Sym`], for the [`Sym::Closure`] key.
/// A named callback resolving to a builtin/unknown returns `None` (the caller
/// handles those inline).
fn callback_effect_edge(cx: &Cx, cbref: &steins_syntax::CallbackRef) -> Option<Sym> {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => Some(Sym::Closure(cx.path().to_owned(), *off)),
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => Some(Sym::Func(cx.fn_decl(site).fqn.clone())),
            FnResolution::Builtin | FnResolution::Unknown => None,
        },
    }
}

/// Wire a resolved callback into the effect graph (ADR-0033): a closure or user
/// function becomes an edge; a builtin callback contributes its catalog findings
/// directly; an unknown callback taints exhaustiveness (`…?`).
fn add_callback_effects(
    cx: &Cx,
    cbref: &steins_syntax::CallbackRef,
    span: steins_syntax::Span,
    d: &mut HashSet<EffectFinding>,
    e: &mut HashSet<Sym>,
    ex: &mut bool,
) {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => {
            e.insert(Sym::Closure(cx.path().to_owned(), *off));
        }
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                e.insert(Sym::Func(cx.fn_decl(site).fqn.clone()));
            }
            FnResolution::Builtin => {
                for f in builtin_findings(name.simple(), span, cx.tree(), cx.path()) {
                    d.insert(f);
                }
            }
            FnResolution::Unknown => *ex = false,
        },
    }
}

/// The unified effect fixpoint for **every** function and method in the whole
/// project, keyed by [`Sym`] (FQN-based, so cross-file edges match).
fn compute_effects(units: &[FileUnit], index: &Index) -> HashMap<Sym, EffectSet> {
    // Each effect unit with the file it lives in and its enclosing class FQN.
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [EffectOrigin],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit { sym: Sym::Func(f.fqn.clone()), file: fi, class_fqn: None, origins: &f.effect_origins });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.effect_origins,
                });
            }
        }
        // Closure/arrow bodies are effect nodes too (ADR-0033) — a HigherOrder /
        // Callback edge into one carries the callback's proven effects.
        for scope in u.tree.scopes() {
            if let ScopeOwner::Closure { def_offset } = &scope.owner {
                ulist.push(Unit {
                    sym: Sym::Closure(u.path.to_owned(), *def_offset),
                    file: fi,
                    class_fqn: None,
                    origins: &scope.effect_origins,
                });
            }
        }
    }

    let mut direct: HashMap<Sym, HashSet<EffectFinding>> = HashMap::new();
    let mut edges: HashMap<Sym, HashSet<Sym>> = HashMap::new();
    let mut exhaustive: HashMap<Sym, bool> = HashMap::new();
    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        let d = direct.entry(unit.sym.clone()).or_default();
        let e = edges.entry(unit.sym.clone()).or_default();
        let ex = exhaustive.entry(unit.sym.clone()).or_insert(true);
        for origin in unit.origins {
            match origin {
                EffectOrigin::Call { name, span } => match cx.resolve_function(name) {
                    FnResolution::User(site) => {
                        e.insert(Sym::Func(cx.fn_decl(site).fqn.clone()));
                    }
                    FnResolution::Builtin => {
                        for f in builtin_findings(name.simple(), *span, cx.tree(), cx.path()) {
                            d.insert(f);
                        }
                    }
                    // Ambiguous / unresolved: effects unknown → non-exhaustive.
                    FnResolution::Unknown => *ex = false,
                },
                EffectOrigin::Output { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "output".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: cx.tree().position(span.start).line,
                        path: cx.path().to_owned(),
                    });
                }
                EffectOrigin::Exit { keyword, span } => {
                    d.insert(EffectFinding {
                        label: "exit".to_owned(),
                        origin: (*keyword).to_owned(),
                        line: cx.tree().position(span.start).line,
                        path: cx.path().to_owned(),
                    });
                }
                EffectOrigin::MethodCall { receiver, method, .. } => {
                    match resolve_effect_edge(&cx, unit.class_fqn.as_deref(), receiver, method) {
                        Some(callee) => {
                            e.insert(callee);
                        }
                        None => *ex = false,
                    }
                }
                // A higher-order call: the callback's effects join the caller's, or
                // the base call resolves normally for a non-invoker callee (ADR-0033).
                EffectOrigin::HigherOrder { callee, callbacks, arg_count, span } => {
                    match steins_catalog::invocation_shape(callee.simple()) {
                        Some(shape) => {
                            // The invoker's own base is effect-pure (the catalog
                            // gives it no colors); its effect IS the callback's.
                            if shape.callback_param < *arg_count {
                                match callbacks.iter().find(|(p, _)| *p == shape.callback_param) {
                                    Some((_, cbref)) => {
                                        add_callback_effects(&cx, cbref, *span, d, e, ex);
                                    }
                                    // Callback slot filled by an unresolvable value.
                                    None => *ex = false,
                                }
                            }
                        }
                        // Not a known invoker: the callee is a normal edge; the
                        // callback arg is just data (its own body, if user, owns it).
                        None => match cx.resolve_function(callee) {
                            FnResolution::User(site) => {
                                e.insert(Sym::Func(cx.fn_decl(site).fqn.clone()));
                            }
                            FnResolution::Builtin => {
                                for f in builtin_findings(callee.simple(), *span, cx.tree(), cx.path()) {
                                    d.insert(f);
                                }
                            }
                            FnResolution::Unknown => *ex = false,
                        },
                    }
                }
                // A `$fn()` resolved to a body-local closure — its effects join.
                EffectOrigin::Callback { cbref, span } => {
                    add_callback_effects(&cx, cbref, *span, d, e, ex);
                }
                EffectOrigin::Opaque { .. } => *ex = false,
            }
        }
    }

    // Fixpoint: effects(u) = direct(u) ∪ ⋃ effects(callees); exhaustive taints.
    let syms: Vec<Sym> = ulist.iter().map(|u| u.sym.clone()).collect();
    let mut findings: HashMap<Sym, HashSet<EffectFinding>> = direct;
    loop {
        let mut changed = false;
        for sym in &syms {
            let callees: Vec<Sym> = edges.get(sym).into_iter().flatten().cloned().collect();
            let mut incoming: Vec<EffectFinding> = Vec::new();
            let mut callee_taint = false;
            for c in &callees {
                if let Some(ce) = findings.get(c) {
                    incoming.extend(ce.iter().cloned());
                }
                if exhaustive.get(c).copied() == Some(false) {
                    callee_taint = true;
                }
            }
            let set = findings.entry(sym.clone()).or_default();
            for ef in incoming {
                changed |= set.insert(ef);
            }
            if callee_taint && exhaustive.get(sym).copied() != Some(false) {
                exhaustive.insert(sym.clone(), false);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    syms.into_iter()
        .map(|s| {
            let f = findings.remove(&s).unwrap_or_default();
            let ex = exhaustive.get(&s).copied().unwrap_or(true);
            (s, EffectSet { findings: f, exhaustive: ex })
        })
        .collect()
}

/// One line of the `annotate` effect margin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSummary {
    pub symbol: String,
    pub line: u32,
    pub labels: Vec<String>,
    pub exhaustive: bool,
    /// The inferred escaping throw classes (ADR-0040), sorted; empty when none.
    pub throws: Vec<String>,
    /// Whether the throw set is exhaustive (no dynamic/unresolved taint).
    pub throws_exhaustive: bool,
}

/// The proven effect set of every concrete function/method in a single file
/// (ADR-0020 annotate margin). Analyzed as a one-file project.
#[must_use]
pub fn effect_summary(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
) -> Vec<EffectSummary> {
    let _ = (functions, classes);
    let units = [FileUnit { path: "", tree }];
    let index = Index::from_units(&units);
    effect_summary_units(&units, &index, 0)
}

/// The proven effect set of every concrete function/method in the `target` file.
#[must_use]
fn effect_summary_units(units: &[FileUnit], index: &Index, target: usize) -> Vec<EffectSummary> {
    let effects = compute_effects(units, index);
    let throws = compute_throws(units, index);
    let tree = units[target].tree;
    let sorted_labels = |sym: &Sym| -> Vec<String> {
        let mut labels: Vec<String> = effects
            .get(sym)
            .into_iter()
            .flat_map(|e| e.findings.iter().map(|f| f.label.clone()))
            .collect();
        labels.sort();
        labels.dedup();
        labels
    };
    let exhaustive = |sym: &Sym| effects.get(sym).is_none_or(|e| e.exhaustive);
    // Escaping throw classes (Yes or Maybe escape) as compact simple names.
    let throw_classes = |sym: &Sym| -> Vec<String> {
        let mut cs: Vec<String> = throws
            .get(sym)
            .into_iter()
            .flat_map(|t| t.facts.keys().map(|f| last_segment(&f.class).to_owned()))
            .collect();
        cs.sort();
        cs.dedup();
        cs
    };
    let throws_exhaustive = |sym: &Sym| throws.get(sym).is_none_or(|t| t.exhaustive);

    let mut out = Vec::new();
    for f in tree.functions() {
        let sym = Sym::Func(f.fqn.clone());
        out.push(EffectSummary {
            symbol: f.name.clone(),
            line: tree.position(f.span.start).line,
            labels: sorted_labels(&sym),
            exhaustive: exhaustive(&sym),
            throws: throw_classes(&sym),
            throws_exhaustive: throws_exhaustive(&sym),
        });
    }
    for c in tree.classes() {
        for m in &c.methods {
            if m.is_abstract {
                continue;
            }
            let sym = Sym::Method(c.fqn.clone(), m.name.clone());
            out.push(EffectSummary {
                symbol: format!("{}::{}", c.name, m.name),
                line: tree.position(m.span.start).line,
                labels: sorted_labels(&sym),
                exhaustive: exhaustive(&sym),
                throws: throw_classes(&sym),
                throws_exhaustive: throws_exhaustive(&sym),
            });
        }
    }
    out
}

/// Effect-envelope diagnostics for the whole project (proven violations only).
fn effect_diagnostics(units: &[FileUnit], index: &Index) -> Vec<Diagnostic> {
    // Fast path: no envelope anywhere → nothing to check.
    let any_envelope = units.iter().any(|u| {
        u.tree.functions().iter().any(|f| f.effect_envelope.is_some())
            || u.tree.classes().iter().any(|c| c.methods.iter().any(|m| m.effect_envelope.is_some()))
    });
    if !any_envelope {
        return Vec::new();
    }

    let effects = compute_effects(units, index);
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let Some(env) = &f.effect_envelope else { continue };
            report_unit(&mut out, &cx, None, &f.name, env, &f.effect_origins, &effects);
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                if let Some(env) = &m.effect_envelope {
                    let display = format!("{}::{}", c.name, m.name);
                    report_unit(
                        &mut out,
                        &cx,
                        Some(&c.fqn),
                        &display,
                        env,
                        &m.effect_origins,
                        &effects,
                    );
                }
                // Liskov (ADR-0033 point 5): a concrete implementation whose PROVEN
                // effects exceed an abstraction's effect envelope. Interfaces carry
                // no bodies, so only concrete class methods are judged.
                if !c.is_interface && !m.is_abstract {
                    emit_effect_liskov(&mut out, &cx, c, m, &effects);
                }
            }
        }
    }
    out
}

/// Emit `effect.liskov-widened` when a concrete method's PROVEN inferred effects
/// exceed the effect envelope declared on an abstraction it overrides/implements
/// (a parent class or interface method — ADR-0033 point 5). Only the proven part
/// judges: the exhaustiveness-tainted (unknown) remainder stays silent.
fn emit_effect_liskov(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    class: &ClassDecl,
    m: &MethodDecl,
    effects: &HashMap<Sym, EffectSet>,
) {
    let abstractions = collect_abstraction_effects(cx, class, &m.name);
    if abstractions.is_empty() {
        return;
    }
    let sym = Sym::Method(class.fqn.clone(), m.name.clone());
    let Some(set) = effects.get(&sym) else { return };
    // The impl's proven effect labels (deduplicated, sorted for stable output).
    let mut proven: Vec<&str> = set.findings.iter().map(|f| f.label.as_str()).collect();
    proven.sort_unstable();
    proven.dedup();
    if proven.is_empty() {
        return;
    }
    for (abs_display, labels) in abstractions {
        for label in &proven {
            if !exceeds(&labels, label) {
                continue; // within the abstraction's envelope (purer OK)
            }
            let clause = if labels.is_empty() {
                "#[\\Steins\\Pure]".to_owned()
            } else {
                let quoted: Vec<String> = labels.iter().map(|l| format!("'{l}'")).collect();
                format!("#[\\Steins\\Effect({})]", quoted.join(", "))
            };
            let pos = cx.tree().position(m.span.start);
            let msg = format!(
                "{}::{}() has proven effect {label} but {abs_display}::{}() (its abstraction) is declared {clause} — Liskov effect widening",
                class.name, m.name, m.name
            );
            out.push(Diagnostic {
                id: EFFECT_LISKOV_ID,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: msg,
                facet: None,
            });
        }
    }
}

/// Every abstraction carrier of `method` with a declared effect envelope: the
/// nearest parent CLASS declaring it, plus each interface the class
/// implements/extends (transitively) declaring it — `(display, envelope labels)`.
fn collect_abstraction_effects(cx: &Cx, class: &ClassDecl, method: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    // Nearest parent class with an effect envelope on this method.
    if let Some((display, labels)) = nearest_parent_effect(cx, class, method) {
        out.push((display, labels));
    }
    // Implemented/extended interfaces declaring the method with an envelope.
    for (display, _file, im) in interface_abstraction_methods(cx, class, method) {
        if let Some(env) = &im.effect_envelope {
            out.push((display, env.labels.clone()));
        }
    }
    out
}

/// The nearest ancestor CLASS (walking `extends`, non-interfaces) declaring
/// `method` with an effect envelope — `(class name, envelope labels)`.
fn nearest_parent_effect(cx: &Cx, class: &ClassDecl, method: &str) -> Option<(String, Vec<String>)> {
    let mut cur = class.parent.as_ref().map(|p| cx.class_fqn(p))?;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let (file, cd) = cx.find_class(&cur)?;
        if cd.is_interface {
            return None;
        }
        if let Some(pm) = cd.methods.iter().find(|pm| pm.name.eq_ignore_ascii_case(method))
            && let Some(env) = &pm.effect_envelope
        {
            return Some((cd.name.clone(), env.labels.clone()));
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// Emit the diagnostics for one declared-envelope unit (ADR-0005/0018).
fn report_unit(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    class_fqn: Option<&str>,
    display: &str,
    envelope: &EffectEnvelope,
    origins: &[EffectOrigin],
    effects: &HashMap<Sym, EffectSet>,
) {
    // 1. Unknown declared labels (one diagnostic each, at the attribute span).
    for label in &envelope.labels {
        if steins_catalog::is_known_label(label) {
            continue;
        }
        let suggestion = steins_catalog::nearest_label(label)
            .map(|s| format!(" — did you mean '{s}'?"))
            .unwrap_or_default();
        let msg = format!(
            "unknown effect label '{label}' in #[\\Steins\\Effect] on {display}(){suggestion}"
        );
        let pos = cx.tree().position(envelope.span.start);
        out.push(Diagnostic {
            id: UNKNOWN_LABEL_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: msg,
            facet: None,
        });
    }

    // 2. Envelope-exceeded violations.
    let labels = &envelope.labels;
    for origin in origins {
        match origin {
            EffectOrigin::Call { name, span } => match cx.resolve_function(name) {
                FnResolution::User(site) => {
                    let callee = Sym::Func(cx.fn_decl(site).fqn.clone());
                    emit_transitive(out, cx, &callee, effects, span.start, display, labels);
                }
                FnResolution::Builtin => {
                    for f in builtin_findings(name.simple(), *span, cx.tree(), cx.path()) {
                        if exceeds(labels, &f.label) {
                            let prefix = format!("{}() has effect {}", name.simple(), f.label);
                            out.push(exceeded_diag(cx, span.start, &prefix, display, labels, &f.label));
                        }
                    }
                }
                FnResolution::Unknown => {}
            },
            EffectOrigin::Output { keyword, span } if exceeds(labels, "output") => {
                let prefix = format!("{keyword} has effect output");
                out.push(exceeded_diag(cx, span.start, &prefix, display, labels, "output"));
            }
            EffectOrigin::Exit { keyword, span } if exceeds(labels, "exit") => {
                let prefix = format!("{keyword} has effect exit");
                out.push(exceeded_diag(cx, span.start, &prefix, display, labels, "exit"));
            }
            EffectOrigin::MethodCall { receiver, method, span } => {
                if let Some(callee) = resolve_effect_edge(cx, class_fqn, receiver, method) {
                    emit_transitive(out, cx, &callee, effects, span.start, display, labels);
                }
            }
            // A higher-order call (the array_map redemption): a resolvable callback
            // at the shape's callback param contributes its effects with the
            // callback's own origin in the provenance (ADR-0033). A non-invoker
            // callee resolves as a normal edge.
            EffectOrigin::HigherOrder { callee, callbacks, arg_count, span } => {
                match steins_catalog::invocation_shape(callee.simple()) {
                    Some(shape) => {
                        if shape.callback_param < *arg_count
                            && let Some((_, cbref)) =
                                callbacks.iter().find(|(p, _)| *p == shape.callback_param)
                        {
                            report_callback(out, cx, cbref, effects, span.start, display, labels);
                        }
                    }
                    None => {
                        if let FnResolution::User(site) = cx.resolve_function(callee) {
                            let cs = Sym::Func(cx.fn_decl(site).fqn.clone());
                            emit_transitive(out, cx, &cs, effects, span.start, display, labels);
                        } else if let FnResolution::Builtin = cx.resolve_function(callee) {
                            for f in builtin_findings(callee.simple(), *span, cx.tree(), cx.path()) {
                                if exceeds(labels, &f.label) {
                                    let prefix = format!("{}() has effect {}", callee.simple(), f.label);
                                    out.push(exceeded_diag(cx, span.start, &prefix, display, labels, &f.label));
                                }
                            }
                        }
                    }
                }
            }
            // A `$fn()` resolved to a body-local closure — report its effects.
            EffectOrigin::Callback { cbref, span } => {
                report_callback(out, cx, cbref, effects, span.start, display, labels);
            }
            EffectOrigin::Output { .. } | EffectOrigin::Exit { .. } => {}
            EffectOrigin::Opaque { .. } => {}
        }
    }
}

/// Emit envelope-exceeded violations for a resolved callback (ADR-0033): a
/// closure/user callback's transitive effects, or a builtin callback's catalog
/// effect, each named with the callback in the provenance.
fn report_callback(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    cbref: &steins_syntax::CallbackRef,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    labels: &[String],
) {
    if let Some(sym) = callback_effect_edge(cx, cbref) {
        emit_transitive(out, cx, &sym, effects, offset, display, labels);
    } else if let steins_syntax::CallbackRef::Named(name) = cbref
        && let FnResolution::Builtin = cx.resolve_function(name)
    {
        for f in builtin_findings(name.simple(), steins_syntax::Span { start: offset, end: offset }, cx.tree(), cx.path()) {
            if exceeds(labels, &f.label) {
                let prefix = format!("{}() has effect {}", name.simple(), f.label);
                out.push(exceeded_diag(cx, offset, &prefix, display, labels, &f.label));
            }
        }
    }
}

/// Emit each proven effect of `callee` not subsumed by the envelope as a
/// transitive violation, naming the ultimate origin.
fn emit_transitive(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    callee: &Sym,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    labels: &[String],
) {
    let callee_display = cx.sym_display(callee);
    let mut fs: Vec<&EffectFinding> =
        effects.get(callee).map(|e| &e.findings).into_iter().flatten().collect();
    fs.sort_by(|a, b| (a.line, &a.label, &a.origin).cmp(&(b.line, &b.label, &b.origin)));
    for ef in fs {
        if !exceeds(labels, &ef.label) {
            continue;
        }
        // Name the file when the ultimate origin arises in a different file than
        // the declared-envelope unit being reported (cross-file provenance).
        let loc = if ef.path == cx.path() {
            format!("line {}", ef.line)
        } else {
            format!("{} line {}", ef.path, ef.line)
        };
        let prefix =
            format!("{callee_display}() has effect {} (via {} at {loc})", ef.label, ef.origin);
        out.push(exceeded_diag(cx, offset, &prefix, display, labels, &ef.label));
    }
}

/// Whether an inferred `effect_label` **exceeds** the declared `labels`.
fn exceeds(labels: &[String], effect_label: &str) -> bool {
    !labels.iter().any(|l| steins_catalog::subsumes(l, effect_label))
}

/// Build an `effect.envelope-exceeded` diagnostic.
fn exceeded_diag(
    cx: &Cx,
    offset: u32,
    prefix: &str,
    display: &str,
    labels: &[String],
    exceeding_label: &str,
) -> Diagnostic {
    let clause = if labels.is_empty() {
        "#[\\Steins\\Pure]".to_owned()
    } else {
        let quoted: Vec<String> = labels.iter().map(|l| format!("'{l}'")).collect();
        format!(
            "#[\\Steins\\Effect({})] — {exceeding_label} exceeds the envelope",
            quoted.join(", ")
        )
    };
    let msg = format!("{prefix}, but {display}() is declared {clause}");
    let pos = cx.tree().position(offset);
    Diagnostic { id: EFFECT_ID, path: cx.path().to_owned(), line: pos.line, column: pos.column, message: msg, facet: None }
}

/// The proven effect findings a builtin `name` carries (empty for pure or
/// uncatalogued builtins).
fn builtin_findings(
    name: &str,
    span: steins_syntax::Span,
    tree: &SourceTree,
    path: &str,
) -> Vec<EffectFinding> {
    match steins_catalog::effect_labels(name) {
        Some(labels) => {
            let line = tree.position(span.start).line;
            labels
                .iter()
                .map(|&label| EffectFinding {
                    label: label.to_owned(),
                    origin: name.to_owned(),
                    line,
                    path: path.to_owned(),
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// Resolve a method-call effect origin to the unit it edges to (project-wide).
fn resolve_effect_edge(
    cx: &Cx,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    method: &str,
) -> Option<Sym> {
    let (start, exact) = match receiver {
        EffectRecv::This | EffectRecv::SelfKw => (enclosing?.to_owned(), false),
        EffectRecv::Parent => (cx.parent_fqn(enclosing?)?, true),
        EffectRecv::ClassName(name) => (cx.class_fqn(name), true),
    };
    let Resolution::Found(r) = resolve_in_chain(cx, &start, method) else { return None };
    if r.method.visibility == Visibility::Private
        && !enclosing.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
    {
        return None;
    }
    if !exact {
        let declaring_final = r.declaring_class.is_final;
        if !(r.method.is_final || r.method.visibility == Visibility::Private || declaring_final) {
            return None;
        }
    }
    Some(Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()))
}

// ---------------------------------------------------------------------------
// Throw system (ADR-0040 damming / ADR-0007 checked accounting). Runs alongside
// the effect fixpoint over the same resolved call graph: `throws(f) = escaping
// own-throws(f) ∪ ⋃ filter(throws(callee), caller-guards)`, monotone to a
// fixpoint, with a throw-exhaustiveness bit tainted by dynamic/unresolved calls
// and opaque throws (mirroring effects).
// ---------------------------------------------------------------------------

/// One throw fact a unit can raise, with the provenance a `via` message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThrowFact {
    /// The thrown class, resolved to an FQN in its origin file's context.
    class: String,
    /// Display for the throwing construct (`new RuntimeException`, `intdiv()`).
    origin: String,
    /// The file the origin lives in (for cross-file position/provenance).
    origin_file: usize,
    /// The origin construct's span start in `origin_file`.
    offset: u32,
    line: u32,
    path: String,
}

/// A unit's throw fixpoint result: the set of throws that **escape** it (each
/// with an escape [`Certainty`] — only `Yes`/`Maybe` are stored; `No`/absorbed
/// throws never enter), plus whether the set is exhaustive (ADR-0040).
#[derive(Debug, Clone, Default)]
struct ThrowSet {
    facts: HashMap<ThrowFact, Certainty>,
    exhaustive: bool,
}

/// Wire a resolved callback's throws into the throw graph (ADR-0033), filtered by
/// the call site's `guards`: a closure/user callback is an edge; a builtin
/// callback contributes its curated throws directly; an unknown callback taints.
#[allow(clippy::too_many_arguments)]
fn add_callback_throws(
    cx: &Cx,
    file: usize,
    cbref: &steins_syntax::CallbackRef,
    span: steins_syntax::Span,
    guards: &[Vec<CatchClause>],
    d: &mut HashMap<ThrowFact, Certainty>,
    e: &mut Vec<(Sym, Vec<Vec<CatchClause>>)>,
    x: &mut bool,
) {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => {
            e.push((Sym::Closure(cx.path().to_owned(), *off), guards.to_vec()));
        }
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), guards.to_vec()));
            }
            FnResolution::Builtin => {
                if let Some(classes) = steins_catalog::builtin_throws(name.simple()) {
                    for c in classes {
                        let esc = escape_through_guards(cx, c, guards);
                        if esc == Certainty::No {
                            continue;
                        }
                        let line = cx.tree().position(span.start).line;
                        let fact = ThrowFact {
                            class: (*c).to_owned(),
                            origin: format!("{}()", name.simple()),
                            origin_file: file,
                            offset: span.start,
                            line,
                            path: cx.path().to_owned(),
                        };
                        let slot = d.entry(fact).or_insert(Certainty::No);
                        *slot = slot.or(esc);
                    }
                }
            }
            FnResolution::Unknown => *x = false,
        },
    }
}

/// `sub <: super` through the project inheritance chain **and** the builtin
/// exception table (ADR-0040), as a [`Certainty`]: `Yes` when the chain reaches
/// `super`; `No` when the chain is fully known (terminates at a project root or a
/// builtin root like `Throwable`) without reaching it; `Maybe` when the chain
/// leaves both the project and the builtin table (an unknown external class —
/// the FP-safe middle).
fn throw_subtype(cx: &Cx, sub_fqn: &str, sup_fqn: &str) -> Certainty {
    let sup = sup_fqn.trim_start_matches('\\');
    let mut cur = sub_fqn.trim_start_matches('\\').to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if cur.eq_ignore_ascii_case(sup) {
            return Certainty::Yes;
        }
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Certainty::Maybe; // cycle → give up
        }
        if let Some((file, cd)) = cx.find_class(&cur) {
            match &cd.parent {
                Some(pref) => cur = cx.units[file].tree.resolve_class_fqn(pref),
                None => return Certainty::No, // known project root, no match
            }
        } else if let Some(parent) = steins_catalog::builtin_exception_parent(&cur) {
            cur = parent.to_owned();
        } else if cur.eq_ignore_ascii_case("Throwable") {
            return Certainty::No; // known builtin root, no match
        } else {
            return Certainty::Maybe; // unknown external class — chain incomplete
        }
    }
}

/// Whether a thrown `class` is **checked** for envelope purposes (ADR-0007):
/// `No` (unchecked) for the `Error` / `LogicException` families, `Yes` when
/// provably neither, `Maybe` when the hierarchy is unknown (checked-but-Maybe —
/// envelope-silent, exhaustiveness-tainting).
fn throw_checked(cx: &Cx, class: &str) -> Certainty {
    let unchecked = throw_subtype(cx, class, "Error").or(throw_subtype(cx, class, "LogicException"));
    match unchecked {
        Certainty::Yes => Certainty::No,
        Certainty::No => Certainty::Yes,
        Certainty::Maybe => Certainty::Maybe,
    }
}

/// Whether one catch `clause` absorbs a thrown `sub` class (ADR-0040): `Yes` when
/// a caught member is provably a supertype; `Maybe` when a member might be (chain
/// leaves known territory) or the clause has an unnameable caught member; `No`
/// when no member can catch it.
fn clause_absorbs(cx: &Cx, sub: &str, clause: &CatchClause) -> Certainty {
    let mut r = if clause.has_unresolvable { Certainty::Maybe } else { Certainty::No };
    for cref in &clause.classes {
        let d = cx.class_fqn(cref);
        r = r.or(throw_subtype(cx, sub, &d));
        if r == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    r
}

/// The escape [`Certainty`] of a `Yes`-arriving throw of `sub` past an ordered
/// (innermost-first) guard stack: `No` once a guard provably absorbs it; `Maybe`
/// if a guard might; else `Yes` (ADR-0040 damming, envelope-consumer side).
fn escape_through_guards(cx: &Cx, sub: &str, guards: &[Vec<CatchClause>]) -> Certainty {
    let mut maybe = false;
    for guard in guards {
        let mut absorb = Certainty::No;
        for clause in guard {
            absorb = absorb.or(clause_absorbs(cx, sub, clause));
            if absorb == Certainty::Yes {
                break;
            }
        }
        match absorb {
            Certainty::Yes => return Certainty::No,
            Certainty::Maybe => maybe = true,
            Certainty::No => {}
        }
    }
    if maybe { Certainty::Maybe } else { Certainty::Yes }
}

/// The unified throw fixpoint for every function/method in the project, keyed by
/// [`Sym`] (shared with the effect graph).
fn compute_throws(units: &[FileUnit], index: &Index) -> HashMap<Sym, ThrowSet> {
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [ThrowOrigin],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit { sym: Sym::Func(f.fqn.clone()), file: fi, class_fqn: None, origins: &f.throw_origins });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.throw_origins,
                });
            }
        }
        // Closure/arrow bodies are throw nodes too (ADR-0033).
        for scope in u.tree.scopes() {
            if let ScopeOwner::Closure { def_offset } = &scope.owner {
                ulist.push(Unit {
                    sym: Sym::Closure(u.path.to_owned(), *def_offset),
                    file: fi,
                    class_fqn: None,
                    origins: &scope.throw_origins,
                });
            }
        }
    }

    type Edge = (Sym, Vec<Vec<CatchClause>>);
    let mut direct: HashMap<Sym, HashMap<ThrowFact, Certainty>> = HashMap::new();
    let mut edges: HashMap<Sym, Vec<Edge>> = HashMap::new();
    let mut ex: HashMap<Sym, bool> = HashMap::new();
    let mut sym_file: HashMap<Sym, usize> = HashMap::new();

    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        sym_file.insert(unit.sym.clone(), unit.file);
        let d = direct.entry(unit.sym.clone()).or_default();
        let e = edges.entry(unit.sym.clone()).or_default();
        let x = ex.entry(unit.sym.clone()).or_insert(true);
        let add_fact = |class: String, origin: String, span: steins_syntax::Span, cert: Certainty, d: &mut HashMap<ThrowFact, Certainty>| {
            if cert == Certainty::No {
                return;
            }
            let line = cx.tree().position(span.start).line;
            let fact = ThrowFact {
                class,
                origin,
                origin_file: unit.file,
                offset: span.start,
                line,
                path: cx.path().to_owned(),
            };
            let slot = d.entry(fact).or_insert(Certainty::No);
            *slot = slot.or(cert);
        };
        for origin in unit.origins {
            match &origin.kind {
                ThrowKind::New(class) => {
                    let d_fqn = cx.class_fqn(class);
                    let esc = escape_through_guards(&cx, &d_fqn, &origin.guards);
                    let display = format!("new {}", last_segment(&d_fqn));
                    add_fact(d_fqn, display, origin.span, esc, d);
                }
                ThrowKind::Rethrow { caught, has_unresolvable } => {
                    for cref in caught {
                        let d_fqn = cx.class_fqn(cref);
                        let esc = escape_through_guards(&cx, &d_fqn, &origin.guards);
                        let display = format!("rethrow {}", last_segment(&d_fqn));
                        add_fact(d_fqn, display, origin.span, esc, d);
                    }
                    if *has_unresolvable {
                        *x = false;
                    }
                }
                ThrowKind::Call(name) => match cx.resolve_function(name) {
                    FnResolution::User(site) => {
                        e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), origin.guards.clone()));
                    }
                    FnResolution::Builtin => {
                        if let Some(classes) = steins_catalog::builtin_throws(name.simple()) {
                            for c in classes {
                                let esc = escape_through_guards(&cx, c, &origin.guards);
                                add_fact((*c).to_owned(), format!("{}()", name.simple()), origin.span, esc, d);
                            }
                        }
                    }
                    FnResolution::Unknown => *x = false,
                },
                ThrowKind::MethodCall { receiver, method } => {
                    match resolve_effect_edge(&cx, unit.class_fqn.as_deref(), receiver, method) {
                        Some(callee) => e.push((callee, origin.guards.clone())),
                        None => *x = false,
                    }
                }
                // A resolved callback's throws propagate through this call site's
                // guards (ADR-0033): a closure/user callback is an edge; a builtin
                // callback contributes its curated throws; unknown taints.
                ThrowKind::Callback { cbref } => {
                    add_callback_throws(&cx, unit.file, cbref, origin.span, &origin.guards, d, e, x);
                }
                ThrowKind::HigherOrder { callee, callbacks, arg_count } => {
                    match steins_catalog::invocation_shape(callee.simple()) {
                        Some(shape) => {
                            if shape.callback_param < *arg_count {
                                match callbacks.iter().find(|(p, _)| *p == shape.callback_param) {
                                    Some((_, cbref)) => add_callback_throws(
                                        &cx, unit.file, cbref, origin.span, &origin.guards, d, e, x,
                                    ),
                                    None => *x = false,
                                }
                            }
                        }
                        None => match cx.resolve_function(callee) {
                            FnResolution::User(site) => {
                                e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), origin.guards.clone()));
                            }
                            FnResolution::Builtin => {
                                if let Some(classes) = steins_catalog::builtin_throws(callee.simple()) {
                                    for c in classes {
                                        let esc = escape_through_guards(&cx, c, &origin.guards);
                                        add_fact((*c).to_owned(), format!("{}()", callee.simple()), origin.span, esc, d);
                                    }
                                }
                            }
                            FnResolution::Unknown => *x = false,
                        },
                    }
                }
                ThrowKind::Taint => *x = false,
            }
        }
    }

    // Fixpoint: propagate callee throws through each call site's guards.
    let syms: Vec<Sym> = ulist.iter().map(|u| u.sym.clone()).collect();
    let mut facts = direct;
    loop {
        let mut changed = false;
        for sym in &syms {
            let file = sym_file[sym];
            let cx = Cx::new(units, index, file);
            let sym_edges: Vec<Edge> = edges.get(sym).cloned().unwrap_or_default();
            for (callee, guards) in &sym_edges {
                if ex.get(callee).copied() == Some(false) && ex.get(sym).copied() != Some(false) {
                    ex.insert(sym.clone(), false);
                    changed = true;
                }
                let callee_facts: Vec<(ThrowFact, Certainty)> =
                    facts.get(callee).into_iter().flatten().map(|(f, c)| (f.clone(), *c)).collect();
                for (fact, cert) in callee_facts {
                    let esc = escape_through_guards(&cx, &fact.class, guards);
                    let nc = cert.and(esc);
                    if nc == Certainty::No {
                        continue;
                    }
                    let slot = facts.entry(sym.clone()).or_default();
                    match slot.get(&fact).copied() {
                        Some(prev) => {
                            let merged = prev.or(nc);
                            if merged != prev {
                                slot.insert(fact, merged);
                                changed = true;
                            }
                        }
                        None => {
                            slot.insert(fact, nc);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    syms.into_iter()
        .map(|s| {
            let f = facts.remove(&s).unwrap_or_default();
            let x = ex.get(&s).copied().unwrap_or(true);
            (s, ThrowSet { facts: f, exhaustive: x })
        })
        .collect()
}

/// The last `\`-segment of an FQN (for a compact throw display).
fn last_segment(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// The declared `@throws` class FQNs of one docblock, resolved in the file's
/// context at `offset` (ADR-0040 envelope opt-in). Accepts bare class names and
/// unions of them; anything else contributes nothing. Empty ⇒ no envelope.
fn declared_throws(cx: &Cx, offset: u32, docblock: Option<&str>) -> Vec<String> {
    let Some(text) = docblock else { return Vec::new() };
    let mut out = Vec::new();
    for tag in scan_docblock(text) {
        if tag.kind != TagKind::Throws {
            continue;
        }
        let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
        collect_class_names(&ty, &mut |name| {
            let fqn = resolve_class_name(cx, offset, name);
            if !out.contains(&fqn) {
                out.push(fqn);
            }
        });
    }
    out
}

/// Resolve a phpdoc class name to an FQN in the current file at `offset`.
fn resolve_class_name(cx: &Cx, offset: u32, name: &str) -> String {
    let raw = name.trim_start_matches('\\').to_owned();
    let kind = if name.starts_with('\\') {
        RefKind::FullyQualified
    } else if raw.contains('\\') {
        RefKind::Qualified
    } else {
        RefKind::Unqualified
    };
    cx.tree().resolve_class_fqn(&NameRef { raw, kind, offset })
}

/// Visit each plain class-name identifier in a phpdoc type that is a class name
/// or a union of class names; non-class members are ignored (no envelope).
fn collect_class_names(ty: &PType, f: &mut dyn FnMut(&str)) {
    match &ty.kind {
        PKind::Identifier(name) => f(name),
        PKind::Union { types, .. } => {
            for t in types {
                collect_class_names(t, f);
            }
        }
        PKind::Nullable(inner) => collect_class_names(inner, f),
        _ => {}
    }
}

/// The whole-project throw diagnostics: `throw.undeclared` envelope escapes and
/// `throw.liskov-widened` overrides (ADR-0040/0033).
fn throw_diagnostics(units: &[FileUnit], index: &Index) -> Vec<Diagnostic> {
    // Fast path: nothing to check without a `@throws` tag anywhere.
    let any_throws = units.iter().any(|u| {
        let has = |d: Option<&str>| d.is_some_and(|t| t.contains("@throws") || t.contains("throws"));
        u.tree.functions().iter().any(|f| f.docblock.as_deref().is_some_and(|t| t.contains("throws")))
            || u.tree.classes().iter().any(|c| {
                c.methods.iter().any(|m| has(m.docblock.as_deref()))
            })
    });
    if !any_throws {
        return Vec::new();
    }

    let throws = compute_throws(units, index);
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let declared = declared_throws(&cx, f.span.start, f.docblock.as_deref());
            if declared.is_empty() {
                continue;
            }
            let sym = Sym::Func(f.fqn.clone());
            emit_undeclared(&mut out, &cx, index, units, &sym, &f.name, &declared, &throws, &f.throw_origins);
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let declared = declared_throws(&cx, m.span.start, m.docblock.as_deref());
                let display = format!("{}::{}", c.name, m.name);
                if !declared.is_empty() {
                    let sym = Sym::Method(c.fqn.clone(), m.name.clone());
                    emit_undeclared(&mut out, &cx, index, units, &sym, &display, &declared, &throws, &m.throw_origins);
                }
                // Liskov: an override/impl whose declared throws widen the parent's.
                emit_liskov(&mut out, &cx, c, m, &declared);
            }
        }
    }
    out
}

/// Emit `throw.undeclared` for each checked, proven-escaping throw of `sym` not
/// covered by its declared `@throws` set.
#[allow(clippy::too_many_arguments)]
fn emit_undeclared(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    index: &Index,
    units: &[FileUnit],
    sym: &Sym,
    display: &str,
    declared: &[String],
    throws: &HashMap<Sym, ThrowSet>,
    decl_origins: &[ThrowOrigin],
) {
    let Some(set) = throws.get(sym) else { return };
    let declared_list = declared.iter().map(|d| last_segment(d).to_owned()).collect::<Vec<_>>().join("|");
    let mut facts: Vec<(&ThrowFact, Certainty)> = set.facts.iter().map(|(f, c)| (f, *c)).collect();
    facts.sort_by(|a, b| (a.0.origin_file, a.0.offset, &a.0.class).cmp(&(b.0.origin_file, b.0.offset, &b.0.class)));
    for (fact, cert) in facts {
        if cert != Certainty::Yes {
            continue; // Maybe-escape is silent (ADR-0040)
        }
        if throw_checked(cx, &fact.class) != Certainty::Yes {
            continue; // unchecked or unknown-hierarchy — never counts
        }
        // Covered iff a subclass of some declared class (Yes through chain).
        let covered = declared.iter().any(|d| throw_subtype(cx, &fact.class, d) != Certainty::No);
        if covered {
            continue; // Yes (covered) or Maybe (unproven) → silent
        }
        let ocx = Cx::new(units, index, fact.origin_file);
        let pos = ocx.tree().position(fact.offset);
        let simple = last_segment(&fact.class);
        let msg = format!(
            "{simple} can escape {display}() but is not declared (@throws {declared_list}) — proven escape"
        );
        // The `origin` facet (ADR-0050 §4), productionizing the measurement note's
        // rule: DIRECT iff the escaping throw's origin is in the annotated
        // declaration's OWN body — same file as the declaration (`cx.cur`) *and* a
        // member of its own scanned `throw_origins` — else PROPAGATED (it arrived up
        // a call edge). The origin offset is a unique file byte position and
        // `throw_origins` is scoped to this one declaration's body, so the
        // same-file-plus-own-origin test is exact even when a callee shares the file.
        let origin = if fact.origin_file == cx.cur
            && decl_origins.iter().any(|o| o.span.start == fact.offset)
        {
            Origin::Direct
        } else {
            Origin::Propagated
        };
        out.push(Diagnostic {
            id: THROW_UNDECLARED_ID,
            path: fact.path.clone(),
            line: pos.line,
            column: pos.column,
            message: msg,
            facet: Some(Facet::Origin(origin)),
        });
    }
}

/// Emit `throw.liskov-widened` when a child method's declared `@throws` names a
/// checked class covered by none of the nearest ancestor method's declared
/// `@throws` (both sides must declare; `Maybe` resolution is silent).
fn emit_liskov(out: &mut Vec<Diagnostic>, cx: &Cx, class: &ClassDecl, m: &MethodDecl, child_declared: &[String]) {
    if child_declared.is_empty() {
        return;
    }
    // Every abstraction carrier of this method: the nearest parent class declaring
    // `@throws`, plus every implemented/extended interface declaring it (ADR-0033).
    for (abs_display, abs_declared) in collect_abstraction_throws(cx, class, &m.name) {
        if abs_declared.is_empty() {
            continue;
        }
        let abs_list = abs_declared.iter().map(|d| last_segment(d)).collect::<Vec<_>>().join("|");
        for c in child_declared {
            // A child-declared class widens iff it is a subclass of NO abstraction class.
            let covered = abs_declared.iter().any(|p| throw_subtype(cx, c, p) != Certainty::No);
            if covered {
                continue;
            }
            let pos = cx.tree().position(m.span.start);
            let msg = format!(
                "{} is declared thrown by {}::{}() but {abs_display}::{}() (its abstraction) declares only @throws {abs_list} — Liskov widening",
                last_segment(c), class.name, m.name, m.name
            );
            out.push(Diagnostic {
                id: THROW_LISKOV_ID,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: msg,
                facet: None,
            });
        }
    }
}

/// Every abstraction carrier of `method` with a declared `@throws` envelope: the
/// nearest parent CLASS declaring it (existing behavior), plus each interface the
/// class implements/extends (transitively) declaring it (ADR-0033 Liskov).
fn collect_abstraction_throws(cx: &Cx, class: &ClassDecl, method: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    if let Some(p) = nearest_parent_throws(cx, class, method) {
        out.push(p);
    }
    for (display, file, im) in interface_abstraction_methods(cx, class, method) {
        let icx = Cx::new(cx.units, cx.index, file);
        let declared = declared_throws(&icx, im.span.start, im.docblock.as_deref());
        if !declared.is_empty() {
            out.push((display, declared));
        }
    }
    out
}

/// The nearest ancestor class (walking `extends`, non-interfaces only) that
/// declares a method named `method` with a `@throws` docblock, returning its class
/// name and declared set.
fn nearest_parent_throws(cx: &Cx, class: &ClassDecl, method: &str) -> Option<(String, Vec<String>)> {
    let mut cur = class.parent.as_ref().map(|p| cx.class_fqn(p))?;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let (file, cd) = cx.find_class(&cur)?;
        // An interface reached via `parent` (an interface's `extends`) is handled by
        // the interface walker, not the parent-class chain.
        if cd.is_interface {
            return None;
        }
        if let Some(pm) = cd.methods.iter().find(|pm| pm.name.eq_ignore_ascii_case(method)) {
            let pcx = Cx::new(cx.units, cx.index, file);
            let declared = declared_throws(&pcx, pm.span.start, pm.docblock.as_deref());
            if !declared.is_empty() {
                return Some((cd.name.clone(), declared));
            }
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// Every interface method (in an implemented/extended interface, transitively) a
/// class's `method` implements — `(interface display, file, &MethodDecl)`
/// (ADR-0033 Liskov). BFS over `implements` (and each interface's own
/// `parent`/`implements` extends chain); dedup by interface FQN.
fn interface_abstraction_methods<'a>(
    cx: &Cx<'a>,
    class: &ClassDecl,
    method: &str,
) -> Vec<(String, usize, &'a MethodDecl)> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Seed with the class's directly-implemented interfaces.
    let mut queue: Vec<String> = class.implements.iter().map(|r| cx.class_fqn(r)).collect();
    while let Some(fqn) = queue.pop() {
        if !seen.insert(fqn.to_ascii_lowercase()) {
            continue;
        }
        let Some((file, id)) = cx.find_class(&fqn) else { continue };
        if !id.is_interface {
            continue; // only interfaces are abstraction carriers here
        }
        if let Some(im) = id.methods.iter().find(|im| im.name.eq_ignore_ascii_case(method)) {
            out.push((id.name.clone(), file, im));
        }
        // An interface's extended interfaces (parent + implements) are abstractions too.
        let itree = cx.units[file].tree;
        if let Some(p) = &id.parent {
            queue.push(itree.resolve_class_fqn(p));
        }
        for r in &id.implements {
            queue.push(itree.resolve_class_fqn(r));
        }
    }
    out
}

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
static EMPTY_DAM: std::sync::LazyLock<DamFacts> = std::sync::LazyLock::new(DamFacts::default);

#[derive(Clone, Copy)]
struct Cx<'a> {
    units: &'a [FileUnit<'a>],
    index: &'a Index,
    cur: usize,
    /// The whole-universe runtime-definition dam fact (ADR-0049 §2), a per-run query
    /// answer (ADR-0048). Read by the absence family's conditional-declaration leg
    /// (A2i): a chain containing a conditional declaration re-dams the claim, so it
    /// fires only when the dam is clear. The auxiliary passes point at [`EMPTY_DAM`].
    dam: &'a DamFacts,
    /// The `[runtime] zend-assertions = "enabled"` pseudo-constant (ADR-0052 §5,
    /// ADR-0037 §2 precedent): when the boot truth declares assertions enabled,
    /// `assert($expr)` narrowing rises to the `Verified` stratum. `false` (the safe
    /// production default — `zend.assertions=-1`) keeps it `Asserted`.
    zend_assertions: bool,
    /// The `[runtime] warning-handler` pseudo-constant (ADR-0049 §7 amendment,
    /// ADR-0037 §2 family). `true` = `"abort"` (the owner-confirmed realistic-app
    /// default: a warning handler converts an `E_WARNING` to an exception / halts, so
    /// a *proven* warning is a proven runtime break — warning-grade offset findings
    /// emit). `false` = `"null"`: the application tolerates the warning and continues,
    /// so warning-grade offset findings leave the proof surface and stay silent (v1
    /// simplification: the ADR-0050 layer-demotion + value-side `null`/`""` adoption
    /// is deferred; v1 either emits under "abort" or silences under "null"). The
    /// Error-grade `offset.on-unsupported` object case (deferred in this slice) is
    /// posture-independent and would emit under both.
    warning_handler_abort: bool,
    /// The project's own PHP `(major, minor)`, as reported by the sidecar `env()`
    /// (ADR-0052 amendment A11), or `None` when no sidecar answered (the sound
    /// default — no reported minor means no detectable skew, so the catalog pin is
    /// trusted, exactly as before A11). Compared against
    /// [`steins_catalog::PINNED_PHP`] to decide whether a catalog-backed is-a
    /// verdict used for **arm deletion** must be demoted to `Unknown`.
    php_minor: Option<(u16, u16)>,
}

impl<'a> Cx<'a> {
    fn new(units: &'a [FileUnit<'a>], index: &'a Index, cur: usize) -> Self {
        Self {
            units,
            index,
            cur,
            dam: &EMPTY_DAM,
            zend_assertions: false,
            warning_handler_abort: true,
            php_minor: None,
        }
    }

    /// A context carrying an explicit runtime config (the top-level analysis pass).
    fn new_with(
        units: &'a [FileUnit<'a>],
        index: &'a Index,
        cur: usize,
        dam: &'a DamFacts,
        zend_assertions: bool,
        warning_handler_abort: bool,
        php_minor: Option<(u16, u16)>,
    ) -> Self {
        Self { units, index, cur, dam, zend_assertions, warning_handler_abort, php_minor }
    }

    /// A context pointing at a different file (for cross-file descent); the runtime
    /// config and dam fact are whole-run properties and are inherited unchanged.
    fn at(&self, file: usize) -> Cx<'a> {
        Cx {
            units: self.units,
            index: self.index,
            cur: file,
            dam: self.dam,
            zend_assertions: self.zend_assertions,
            warning_handler_abort: self.warning_handler_abort,
            php_minor: self.php_minor,
        }
    }

    /// Whether a **catalog-backed** is-a verdict used for arm deletion must be
    /// demoted to `Unknown` (ADR-0052 A11): the project PHP minor is known and
    /// differs from the catalog pin. When the minor is unknown or matches, catalog
    /// verdicts stand.
    fn a11_demote_catalog(&self) -> bool {
        self.php_minor.is_some_and(|m| m != steins_catalog::PINNED_PHP)
    }

    fn tree(&self) -> &'a SourceTree {
        self.units[self.cur].tree
    }
    fn path(&self) -> &'a str {
        self.units[self.cur].path
    }
    fn strict(&self) -> bool {
        self.tree().has_strict_types()
    }

    fn fn_decl(&self, site: Site) -> &'a FunctionDecl {
        &self.units[site.file].tree.functions()[site.index]
    }
    fn class_decl(&self, site: Site) -> (usize, &'a ClassDecl) {
        (site.file, &self.units[site.file].tree.classes()[site.index])
    }

    /// Resolve a class reference (in the current file's context) to its FQN.
    fn class_fqn(&self, r: &NameRef) -> String {
        self.tree().resolve_class_fqn(r)
    }

    /// Find a class by FQN (case-insensitive), returning its file and decl.
    fn find_class(&self, fqn: &str) -> Option<(usize, &'a ClassDecl)> {
        match self.index.resolve_class(fqn) {
            Res::Unique(site) => Some(self.class_decl(site)),
            _ => None,
        }
    }

    /// Whether a `$this` seeded from enclosing class `class_fqn` is provably the
    /// **exact** runtime class (audit G1). A `final` class or an enum has no
    /// subclass, so its `$this` is exact; any other project class is only a lower
    /// bound (some subclass instance may be running the method). A class the index
    /// cannot uniquely resolve is conservatively *not* exact.
    fn this_class_exact(&self, class_fqn: &str) -> bool {
        self.find_class(class_fqn).is_some_and(|(_, cd)| cd.is_final || cd.is_enum)
    }

    /// The FQN of `class_fqn`'s parent, resolved in the parent's own file ctx.
    fn parent_fqn(&self, class_fqn: &str) -> Option<String> {
        let (file, cd) = self.find_class(class_fqn)?;
        let pref = cd.parent.as_ref()?;
        Some(self.units[file].tree.resolve_class_fqn(pref))
    }

    /// The non-static properties of `class_fqn` including inherited ones (ADR-0036),
    /// walking the parent chain; a derived-class declaration shadows an ancestor's
    /// property of the same name (first-seen wins, own class first). Static
    /// properties are excluded (never heap-tracked). Stops at an unknown/absent
    /// parent or a trait-using class (give up → the props gathered so far).
    fn class_props(&self, class_fqn: &str) -> Vec<&'a PropertyDecl> {
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

    /// The `__construct` method resolved through `class_fqn`'s chain (ADR-0036),
    /// for mapping `new` args to promoted-property positions.
    fn find_ctor(&self, class_fqn: &str) -> Option<&'a MethodDecl> {
        let mut cur = class_fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(cur.to_ascii_lowercase()) {
                return None;
            }
            let (file, cd) = self.find_class(&cur)?;
            if let Some(m) = cd.methods.iter().find(|m| m.is_constructor) {
                return Some(m);
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
    /// Deliberately **not a solver** (ADR-0030/0032 "won't build"): a template is
    /// bound only from a *bare* `@param T` occurrence at a constructor parameter; a
    /// nested or compound occurrence (`@param array<T>`, `@param T|null`) does not
    /// bind it. The result is **all-or-nothing**: one carried value per template
    /// only when EVERY template resolved to a proven value at an aligned positional
    /// argument; any gap (a template with no direct parameter, a missing/unprovable
    /// argument, a variadic in the way) returns EMPTY. An empty carry is the honest
    /// floor — downstream acceptance answers `Maybe` on the argument half, never a
    /// manufactured `No`, and the positional args↔templates alignment stays sound.
    ///
    /// ADR-0048: this is a pure function of the already-seeded `new` argument trace
    /// (§2 replayable from the scope walk), touches no scope entry state (§3), and
    /// carries no global-ordering dependence (§4).
    fn infer_generic_args(
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
        let Some(ctor) = self.find_ctor(class_fqn) else { return empty };
        // The constructor's own `@param` envelopes, WITHOUT the class-level template
        // shadow applied: a bare `@param T` must stay readable as the template name
        // `T` here (the shadow that neutralizes it to opaque is a check-site concern).
        let Some(ctor_env) = parse_envelopes(ctor.docblock.as_deref()) else { return empty };
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

    /// Resolve a **function** call reference per PHP name resolution (ADR-0001).
    fn resolve_function(&self, r: &NameRef) -> FnResolution {
        let catalog_knows = |n: &str| steins_catalog::effect_labels(n).is_some();
        match r.kind {
            RefKind::FullyQualified => {
                let fqn = r.raw.to_ascii_lowercase();
                match self.index.resolve_function(&fqn) {
                    Res::Unique(site) => FnResolution::User(site),
                    Res::Ambiguous => FnResolution::Unknown,
                    Res::Absent => {
                        // `\strlen` — a single-segment global name may be a builtin.
                        if !r.raw.contains('\\') && catalog_knows(&r.raw) {
                            FnResolution::Builtin
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
                    return match self.index.resolve_function(&t.to_ascii_lowercase()) {
                        Res::Unique(site) => FnResolution::User(site),
                        _ => FnResolution::Unknown,
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
                        if is_builtin { FnResolution::Builtin } else { FnResolution::Unknown }
                    }
                }
            }
        }
    }

    /// The site of a **user** function this call resolves to (positional-only),
    /// or `None` for builtins / unknown / dynamic / named-arg calls.
    fn resolve_user_fn(&self, call: &CallExpr) -> Option<Site> {
        if !call.positional_only {
            return None;
        }
        let r = call.callee_ref.as_ref()?;
        match self.resolve_function(r) {
            FnResolution::User(site) => Some(site),
            _ => None,
        }
    }

    /// The unique body scope of the user function at `site`, plus its file.
    fn fn_scope(&self, site: Site) -> Option<(usize, &'a Scope)> {
        let name = &self.fn_decl(site).name;
        let tree = self.units[site.file].tree;
        let mut it = tree.scopes().iter().filter(|s| s.function_name.as_deref() == Some(name));
        let scope = it.next()?;
        if it.next().is_some() { None } else { Some((site.file, scope)) }
    }

    /// The unique method body scope for `class_fqn::method` in `file`.
    fn method_scope(&self, file: usize, class_fqn: &str, method: &str) -> Option<&'a Scope> {
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
    fn closure_scope(&self, def_offset: u32) -> Option<&'a Scope> {
        self.tree().scopes().iter().find(|s| {
            matches!(&s.owner, ScopeOwner::Closure { def_offset: d } if *d == def_offset)
        })
    }

    /// A display name for an effect [`Sym`] (`f`, `Foo::bar`), using the resolved
    /// declaration's written case where available.
    fn sym_display(&self, sym: &Sym) -> String {
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
    fn resolve_literal(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<ArgValue> {
        if poisoned {
            return None;
        }
        match value {
            v if v.is_literal() => Some(v.clone()),
            ArgValue::Var(name) => env.get(name).and_then(Known::singleton),
            ArgValue::Call(name, args) => {
                if args.is_empty()
                    && let Some((lit, _line)) = self.resolve_const_fn(name)
                {
                    return Some(lit);
                }
                self.try_fold(name, args, folder).map(|(lit, _prov)| lit)
            }
            // An array is proven iff every element value is proven (keys are fixed
            // at lowering). Folding is never applied to arrays (ADR-0001).
            ArgValue::Array(items) => {
                let mut resolved = Vec::with_capacity(items.len());
                for (k, v) in items {
                    let rv = self.resolve_literal(v, env, poisoned, folder)?;
                    resolved.push((k.clone(), rv));
                }
                Some(ArgValue::Array(resolved))
            }
            _ => None,
        }
    }

    /// Try to fold an allowlisted builtin call over literal arguments.
    fn try_fold(
        &self,
        name: &str,
        args: &[ArgValue],
        folder: &mut dyn Folder,
    ) -> Option<(ArgValue, String)> {
        // Any project user function sharing this simple name shadows the builtin
        // (or makes it ambiguous) — do not fold. Conservative, never an FP.
        if self.index.has_simple_function(name) {
            return None;
        }
        if !steins_catalog::foldable(name) {
            return None;
        }
        if !args.iter().all(ArgValue::is_literal) {
            return None;
        }
        let folded = folder.fold(name, args)?;
        Some((folded, format!("folded from {}", render_call(name, args))))
    }

    /// Resolve a zero-argument constant function anywhere in the project by its
    /// simple name: unique definition, no params, body exactly `return <lit>`,
    /// scope not poisoned. Returns the literal and the definition line.
    fn resolve_const_fn(&self, name: &str) -> Option<(ArgValue, u32)> {
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
    fn diagnostic(
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
        Diagnostic { id: ID, path: self.path().to_owned(), line: pos.line, column: pos.column, message, facet: None }
    }

    /// Build a `type.return-mismatch` diagnostic. `display` is the owning
    /// function/method name (`f`, `Foo::bar`); `mode` is governed by the owning
    /// file's `declare(strict_types=1)` — the file this `Cx` points at.
    fn return_diagnostic(
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
        }
    }

    /// The parameter list of a scope's owning function or method (same file this
    /// `Cx` points at), or `None` for the top-level script scope. Used by the
    /// native-type parameter seeding (Feature B).
    fn scope_params(&self, scope: &Scope) -> Option<&'a [Param]> {
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
        }
    }

    /// The parsed `@param`/`@return`/assert envelopes off the scope's owning
    /// declaration docblock (function or method), with class-level `@template`
    /// names shadowed for a method (issue #5), or `None` when there is no docblock
    /// / the scope is a closure or top-level. Used by contract-fact seeding
    /// (ADR-0052 §9) to refine the native member list with the declared `@param`.
    fn scope_envelopes(&self, scope: &Scope) -> Option<Envelopes> {
        match &scope.owner {
            ScopeOwner::TopLevel | ScopeOwner::Closure { .. } => None,
            ScopeOwner::Function(name) => {
                let f = self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                parse_envelopes(f.docblock.as_deref())
            }
            ScopeOwner::Method { class, method } => {
                let cd = self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                let mut env = parse_envelopes(m.docblock.as_deref())?;
                env.shadow_templates(&template_names_of(cd.docblock.as_deref()));
                Some(env)
            }
        }
    }

    /// The native return type and display name of a scope's owning function or
    /// method (the same file this `Cx` points at), or `None` for the top-level
    /// script scope or an owner with no native scalar/union return type.
    fn scope_return(&self, scope: &Scope) -> Option<(&'a NativeType, String)> {
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
            // Closure return-type checking is deferred this slice (documented).
            ScopeOwner::Closure { .. } => None,
        }
    }

    /// The `@return` phpdoc envelope and display name of a scope's owning function
    /// or method (same file this `Cx` points at), or `None` when there is no
    /// docblock `@return` (or the scope is top-level).
    fn scope_return_phpdoc(&self, scope: &Scope) -> Option<(PType, String)> {
        match &scope.owner {
            ScopeOwner::TopLevel => None,
            ScopeOwner::Function(name) => {
                let f =
                    self.tree().functions().iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                let ret = parse_envelopes(f.docblock.as_deref())?.ret?;
                Some((ret, f.name.clone()))
            }
            ScopeOwner::Method { class, method } => {
                let cd =
                    self.tree().classes().iter().find(|c| c.fqn.eq_ignore_ascii_case(class))?;
                let m = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))?;
                let mut env = parse_envelopes(m.docblock.as_deref())?;
                // Class-level `@template` names shadow in this method's `@return` too
                // (issue #5) — the idempotent class-level stage.
                env.shadow_templates(&template_names_of(cd.docblock.as_deref()));
                let ret = env.ret?;
                Some((ret, format!("{}::{}", cd.name, m.name)))
            }
            ScopeOwner::Closure { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The env now stores the full four-layer `steins_domain::Fact` (ADR-0035) — the
// finished algebra lives in `steins-domain`; this crate only converts to/from
// the trace IR's `ArgValue` at the two seams below and calls the domain's joins,
// membership, and trinary queries. Stage-2 abstract facts (`Refined`/`General`)
// now flow through the env exactly like the finite layers: they resolve no
// *value* (only a `Singleton` does), but they carry knowledge for guard
// refinements (ADR-0031 stage 2), native-type seeding, and contract-on-fact
// acceptance (ADR-0030).
// ---------------------------------------------------------------------------

/// The domain value-fact (four layers), aliased as `Fact` throughout the walk.
use steins_domain::Fact;
use steins_domain::{Base, IntRange, Key as VKey, Refinement, StrPreds, Val};

/// The conversion seam **into** the domain: a literal (or fully-literal array)
/// [`ArgValue`] to a domain [`Val`]. Array keys carry PHP key-normalization in
/// insertion order (reusing [`normalize_array`], matching [`VKey`]). Any
/// non-literal element (or a non-literal `ArgValue`) yields `None` — the fact is
/// dropped (the safe side).
fn val_of(arg: &ArgValue) -> Option<Val> {
    match arg {
        ArgValue::Int(i) => Some(Val::Int(*i)),
        ArgValue::Float(f) => Some(Val::Float(*f)),
        ArgValue::Str(s) => Some(Val::Str(s.clone())),
        ArgValue::Bool(b) => Some(Val::Bool(*b)),
        ArgValue::Null => Some(Val::Null),
        ArgValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (k, v) in normalize_array(items) {
                let key = match k {
                    NormKey::Int(i) => VKey::Int(i),
                    NormKey::Str(s) => VKey::Str(s),
                };
                out.push((key, val_of(&v)?));
            }
            Some(Val::Array(out))
        }
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::New(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Coalesce(..)
        | ArgValue::Closure(_)
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        // An offset read is never a proven `Val` — the walk judges it separately
        // (ADR-0049 §7); it manufactures no fact here (the safe side).
        | ArgValue::OffsetRead { .. }
        // Object-world values (ADR-0043): not domain `Val`s — unproven, == Other.
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::Other => None,
    }
}

/// The conversion seam **out of** the domain: a concrete [`Val`] back to the
/// trace IR's [`ArgValue`]. Total (the domain's `Val` is exactly the concrete
/// subset of `ArgValue`), so proven-value consumers (native truth table, folding
/// args, descent binding) keep receiving an `ArgValue` as before.
fn arg_of_val(v: &Val) -> ArgValue {
    match v {
        Val::Int(i) => ArgValue::Int(*i),
        Val::Float(f) => ArgValue::Float(*f),
        Val::Str(s) => ArgValue::Str(s.clone()),
        Val::Bool(b) => ArgValue::Bool(*b),
        Val::Null => ArgValue::Null,
        Val::Array(items) => ArgValue::Array(
            items
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        VKey::Int(i) => ArrayKey::Int(*i),
                        VKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, arg_of_val(v))
                })
                .collect(),
        ),
    }
}

/// Render a domain [`Val`] for a message/margin **byte-for-byte** identically to
/// the existing [`ArgValue::render`] (float `5.0` form, `['a', 'b']` arrays,
/// double-quoted scalars) — it simply routes through the shared renderer, so a
/// `Singleton` fact renders exactly as its `ArgValue` always did.
fn render_val(v: &Val) -> String {
    arg_of_val(v).render()
}

/// A domain `Singleton` fact from a literal/array [`ArgValue`], or `None` when
/// the value is not representable (a non-literal) — the fact is then dropped.
fn singleton_fact(arg: &ArgValue) -> Option<Fact> {
    val_of(arg).map(Fact::Singleton)
}

/// The target of a proven closure value (ADR-0033): an anonymous closure/arrow
/// scope (by definition-site byte offset), or a first-class callable naming a free
/// function.
#[derive(Clone)]
enum ClosureTarget {
    /// An anonymous closure/arrow scope addressed by its `def_offset`.
    Scope(u32),
    /// A first-class callable of a named free function (`strtolower(...)`).
    Named(NameRef),
}

/// A proven closure value carried in the env (ADR-0033). Normal value discipline
/// applies: a reassignment/invalidation drops the whole [`Known`], so the closure
/// dies exactly like any other value. The by-value capture **snapshot** is taken
/// at closure-creation time (the definition-site env), which is the semantically
/// correct PHP by-value capture — a later mutation of the captured variable does
/// not change what the closure sees.
#[derive(Clone)]
struct ClosureVal {
    target: ClosureTarget,
    /// The by-value captured variable facts, snapshotted at creation.
    captures: Vec<(String, Fact)>,
    /// The closure definition line, for descent provenance.
    def_line: u32,
}

/// Whether two closure targets denote the same closure (same anonymous scope, or
/// the same named function) — for join survival.
fn closure_target_eq(a: &ClosureTarget, b: &ClosureTarget) -> bool {
    match (a, b) {
        (ClosureTarget::Scope(x), ClosureTarget::Scope(y)) => x == y,
        (ClosureTarget::Named(x), ClosureTarget::Named(y)) => x.raw == y.raw && x.kind == y.kind,
        _ => false,
    }
}

/// A capture fact reduced to a [`BindingKey`]-comparable [`ArgValue`]: a concrete
/// `Singleton` becomes its value (so a snapshot of `1` and of `"abc"` key
/// distinctly); any abstract fact collapses to `Other` (still distinct from a
/// concrete snapshot, sound for memoization).
fn arg_of_fact_key(fact: &Fact) -> ArgValue {
    match fact {
        Fact::Singleton(v) => arg_of_val(v),
        _ => ArgValue::Other,
    }
}

/// An allocation identity — the key the heap is stored under (ADR-0036). Fresh
/// per `new`/`clone`; a variable holds one via [`Store::refs`] (its ObjRef).
type AllocId = u32;

/// A property's value-domain fact together with its trust stratum (ADR-0052 §5).
/// A prop written from an `Asserted` rvalue is `Asserted`; a prop read back out
/// (`$x = $o->p`) carries the stratum forward, so an assert cannot launder into a
/// proof-layer premise through the heap (the derivation clause — heap writes).
#[derive(Clone)]
struct PropFact {
    fact: Fact,
    stratum: Stratum,
}

/// A heap object (ADR-0036 object state): allocation-keyed, so aliases share it.
/// The `class` is fixed at construction and never swept; `class_exact` says whether
/// it is the *exact* runtime class or only a lower bound (see below); `props` are
/// the per-property value-domain facts.
#[derive(Clone)]
struct HeapObj {
    /// The class FQN (lowercase-normalized, as `classes_env` held). For an
    /// allocation-proven object (`new`, enum case, clone-of-exact) this is the exact
    /// runtime class; for a `$this` seed it is only a **lower bound** — the runtime
    /// object may be any descendant that inherited the method. `class_exact`
    /// distinguishes the two (audit G1, ADR-0036).
    class: String,
    /// Whether `class` is the *exact* runtime class (`true`) or a lower bound
    /// (`false`). A No-side conclusion — `is_a(class, T) = No` implies the object is
    /// not a `T` — is only sound when this is `true`: with a lower bound the actual
    /// instance may be a descendant of `class` that *is* a `T`. Yes-side conclusions
    /// (`is_a(class, T) = Yes`) hold for a lower bound too (every descendant is a T).
    class_exact: bool,
    /// Property facts keyed by property name (ADR-0035 Facts live in props), each
    /// with its trust stratum (ADR-0052 §5).
    props: HashMap<String, PropFact>,
    /// Properties declared `readonly` — sweep-immune once established (ADR-0036).
    readonly: HashSet<String>,
    /// readonly props provably written on THIS path (for `readonly.reassigned`).
    ro_written: HashSet<String>,
    /// Whether this object has escaped (passed to a call, returned, stored into an
    /// array/property, or captured by a closure). Escaped objects have their
    /// non-readonly props swept by unknown calls; a purely-local object's props
    /// survive — the ADR-0036 precision payoff.
    escaped: bool,
}

impl HeapObj {
    /// A fresh heap object. `class_exact` defaults to `false` (a lower bound — the
    /// safe default); allocation-proven construction sites set it to `true`
    /// explicitly (`build_new_object`, exact `$this`/clone seeds).
    fn new(class: String) -> Self {
        HeapObj {
            class,
            class_exact: false,
            props: HashMap::new(),
            readonly: HashSet::new(),
            ro_written: HashSet::new(),
            escaped: false,
        }
    }

    /// Sweep the non-readonly props (an unknown/overridable call on an escaped or
    /// `$this` object may have mutated them). readonly props and the class survive.
    fn sweep_nonreadonly(&mut self) {
        self.props.retain(|name, _| self.readonly.contains(name));
    }
}

/// The object store threaded through the walk (ADR-0036), replacing the old flat
/// `var → class` map. `refs` binds a variable to an allocation id (its ObjRef);
/// `heap` maps ids to objects. Aliasing (`$b = $a`) copies the ref (shared id), so
/// a write through any alias is visible through all. The exact-class fact that
/// `classes_env` used to hold is now `heap[refs[var]].class`.
#[derive(Clone, Default)]
struct Store {
    refs: HashMap<String, AllocId>,
    heap: HashMap<AllocId, HeapObj>,
    /// **Contract facts** (ADR-0052 §1, the NEW carrier): a variable's *declared*
    /// type as a lowered, syntactic **arm list**, seeded at scope entry (§9 — THE
    /// entry-state contribution) and narrowed by guards arm-wise (`instanceof`,
    /// `!== null`). Each arm carries its own trust stratum: the native member list
    /// seeds `Verified`, a `@param` phpdoc refinement seeds `Asserted` (ADR-0037
    /// trust order). Subtraction preserves each surviving arm's stratum — an
    /// `Asserted` arm can never launder to `Verified`. Consumed ONLY by the four
    /// §3 consumers (arm filtering, `eval_instanceof` implication, catch matching,
    /// and — reserved for S6 — the declared-receiver lane); NEVER by `call.on-null`
    /// proofs, arity, `call.undefined-method`, or binding descent.
    contract: HashMap<String, Vec<ContractArm>>,
    /// **Class facts** (ADR-0052 §1, the NEW carrier): guard-derived is-a bounds on
    /// an object-holding variable, beside the heap's *exact* class. A positive
    /// `instanceof T` binds `T` into `yes`; a negative branch binds it into `no`.
    /// `Member` is deliberately weaker than exactness (ADR-0043) — it is NOT fed to
    /// the exactness-gated consumers (§3 NOT-fed list); a final-class `Member` is
    /// deliberately not treated as exactness in v1.
    members: HashMap<String, Member>,
    /// **Existence vouches** (ADR-0049 §4 conservative guard-respect leg): the set of
    /// symbols a positive `method_exists`/`function_exists`/`class_exists` … guard has
    /// vouched for on THIS branch. An absence-family emitter (`call.undefined-method`
    /// today, S4's ids tomorrow) that resolves to a vouched symbol stays silent even
    /// when its own proof reached `Absent` — firing against programmer-supplied
    /// existence evidence would call the programmer a liar. Purely additive and
    /// walk-local (ADR-0048): bound on the guarded branch clone, INTERSECTED at a
    /// join (a vouch survives past the `if` only if every fall-through path carried
    /// it — so `if (method_exists(C,'m')) {} (new C)->m();` never silences the tail),
    /// and deliberately untouched by [`Self::unbind`]/[`Self::clear`] (a symbol's
    /// existence does not change when a variable is rebound or a barrier is crossed).
    vouched: HashSet<Vouch>,
}

/// A symbol a positive existence guard vouches for (ADR-0049 §4 guard-respect leg).
/// All names are lowercased — PHP class/function/method names are case-insensitive,
/// so the vouch matches the resolved emitter symbol case-blind.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Vouch {
    /// `method_exists(C, 'm')` vouched `C::m` — `class` is the receiver's FQN.
    Method { class: String, method: String },
    /// `function_exists('f')` vouched the function `f`.
    Function(String),
    /// `class_exists`/`interface_exists`/`trait_exists`/`enum_exists('N')` vouched `N`.
    Class(String),
}

/// One arm of a [`Store::contract`] lane: a declared-type alternative plus the
/// trust stratum it was seeded at (ADR-0052 §1/§5). The `ty` is the syntactic arm
/// judged arm-wise through steins-contract's single acceptance relation.
#[derive(Clone, Debug, PartialEq)]
struct ContractArm {
    ty: steins_contract::ContractTy,
    stratum: Stratum,
}

/// Guard-derived is-a bounds on an object variable (ADR-0052 §1 `Member`): is-a
/// every class in `yes`, provably-not-is-a every class in `no`. FQNs are stored
/// lowercase-normalized (matching the is-a oracle's key). Bound at the `Verified`
/// stratum — a runtime `instanceof` executed on the live branch.
#[derive(Clone, Debug, Default, PartialEq)]
struct Member {
    yes: Vec<String>,
    no: Vec<String>,
}

impl Store {
    /// The exact class of the object `var` currently refers to, if any.
    fn class_of(&self, var: &str) -> Option<&str> {
        self.heap.get(self.refs.get(var)?).map(|o| o.class.as_str())
    }

    /// The allocation id `var` currently refers to.
    fn id_of(&self, var: &str) -> Option<AllocId> {
        self.refs.get(var).copied()
    }

    /// Whether `var` refers to an object whose `class` is the **exact** runtime
    /// class (audit G1). `false` for an unbound var and for any lower-bound object
    /// (a `$this` seed that is not provably exact) — the No-side gate.
    fn is_exact(&self, var: &str) -> bool {
        self.obj_of(var).is_some_and(|o| o.class_exact)
    }

    /// The object `var` currently refers to.
    fn obj_of(&self, var: &str) -> Option<&HeapObj> {
        self.heap.get(self.refs.get(var)?)
    }

    /// Whether `var` is bound to any object.
    fn is_bound(&self, var: &str) -> bool {
        self.refs.contains_key(var)
    }

    /// A property fact of the object `var` refers to (stratum-agnostic — used by
    /// contract-layer consumers, which accept `Asserted`).
    fn prop_fact(&self, var: &str, prop: &str) -> Option<&Fact> {
        self.obj_of(var)?.props.get(prop).map(|p| &p.fact)
    }

    /// The trust stratum of a property fact of the object `var` refers to, or
    /// `Verified` when there is no such prop (the neutral element of `min`).
    fn prop_stratum(&self, var: &str, prop: &str) -> Stratum {
        self.obj_of(var).and_then(|o| o.props.get(prop)).map_or(Stratum::Verified, |p| p.stratum)
    }

    /// Drop `var`'s ObjRef binding — the heap object survives (other aliases keep
    /// seeing it); `var` just forgets which object it held (ADR-0036: a pass-to-call
    /// may rebind `$var`, so the var→id link must die exactly as `classes_env`
    /// entries did, while the id lives on for its other aliases).
    fn unbind(&mut self, var: &str) {
        self.refs.remove(var);
        // Reassignment / invalidation also voids the guard-derived class facts and
        // the declared-type arm lane: a rebound `$var` no longer satisfies the
        // narrowed possibilities established for the old value (ADR-0052 §9 —
        // narrowing carriers are scope-local and die with the value they described).
        self.members.remove(var);
        self.contract.remove(var);
    }

    /// Clear all bindings and the heap — a Barrier: nothing is reachable.
    fn clear(&mut self) {
        self.refs.clear();
        self.heap.clear();
        self.members.clear();
        self.contract.clear();
    }

    /// The narrowed declared-type arm lane of `var` (ADR-0052 §3, consumer (d) —
    /// the declared-receiver lane **reserved for S6** `phpdoc.undefined-method`).
    /// Built now so S6 consumes a stable accessor; N4 itself emits nothing from it.
    /// The returned arms are the seeded declared type minus every guard subtraction
    /// on the live branch (e.g. `{Guest}` after the else of `instanceof User` over
    /// `User|Guest`); each carries its stratum for the min-premise rule.
    #[allow(dead_code)] // consumed by ADR-0049 S6; N4 builds the lane, emits nothing
    fn contract_arms(&self, var: &str) -> Option<&[ContractArm]> {
        self.contract.get(var).map(Vec::as_slice)
    }

    /// The class-membership fact of `var` (ADR-0052 §1 `Member`), if any guard bound
    /// one on this branch. Consumed only by [`eval_instanceof`] implication (§3b)
    /// and catch-arm matching — never the exactness-gated lanes.
    fn member_of(&self, var: &str) -> Option<&Member> {
        self.members.get(var)
    }

    /// Record an existence vouch on this branch (ADR-0049 §4 guard-respect leg).
    fn vouch(&mut self, v: Vouch) {
        self.vouched.insert(v);
    }

    /// Whether a positive existence guard on this path vouched `class::method`
    /// (case-insensitively — the vouch stores lowercased names).
    fn vouches_method(&self, class: &str, method: &str) -> bool {
        self.vouched.contains(&Vouch::Method {
            class: class.to_ascii_lowercase(),
            method: method.to_ascii_lowercase(),
        })
    }

    /// Mark the object `var` refers to as escaped (if any).
    fn mark_escaped(&mut self, var: &str) {
        if let Some(id) = self.refs.get(var).copied()
            && let Some(o) = self.heap.get_mut(&id)
        {
            o.escaped = true;
        }
    }

    /// Sweep every escaped object's non-readonly props (an unknown call ran that may
    /// mutate any escaped object). Non-escaped objects survive (ADR-0036 payoff).
    fn sweep_escaped(&mut self) {
        for o in self.heap.values_mut() {
            if o.escaped {
                o.sweep_nonreadonly();
            }
        }
    }
}

/// The **trust stratum** of a bound fact (ADR-0052 §5): whether it is fit to
/// premise a proof-layer finding. `Verified` facts come from a runtime-executed
/// test on the live branch (`===`, `is_int`, `instanceof`, ordering, truthiness)
/// or a native declaration seed — the branch runs only if the test passed, so the
/// fact holds on the live path. `Asserted` facts come from docblock claims
/// (`@phpstan-assert` family) and from `assert($expr)` narrowing (never evaluated
/// under `zend.assertions=-1`) — a claim, not a proof. The bit is a *checked*
/// attribute (the `"asserted"` provenance string is prose, only for display); the
/// consumption rule (proof-layer ids require all-Verified premises) reads it.
///
/// A derived fact's stratum is the **minimum** over every fact consumed in its
/// derivation (the amendment's derivation clause): folds, array composition, heap
/// property writes, branch joins, and binding-descent seeding all propagate
/// `min(inputs)`, so `Asserted` can never launder into `Verified` across a
/// derivation step. `min` is `Asserted` whenever either operand is `Asserted`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Stratum {
    /// A runtime-executed test or a native declaration — fit for the proof layer.
    Verified,
    /// A docblock claim or an `assert($expr)` narrowing — never premises proof.
    Asserted,
}

impl Stratum {
    /// The weaker of two strata: `Asserted` dominates (the derivation clause). This
    /// is commutative and associative, so the min-stratum rule is order-independent
    /// (ADR-0048: no global ordering enters a fact).
    fn min(self, other: Stratum) -> Stratum {
        match (self, other) {
            (Stratum::Verified, Stratum::Verified) => Stratum::Verified,
            _ => Stratum::Asserted,
        }
    }
}

/// A proven local fact plus where it was established (for provenance). A closure
/// value has no scalar `fact` — it rides in [`Known::closure`] instead (ADR-0033).
#[derive(Clone)]
struct Known {
    /// The scalar/array value-domain fact, or `None` for a closure-only binding.
    fact: Option<Fact>,
    line: u32,
    bound: Option<String>,
    /// The proven closure value bound to this variable, if any (ADR-0033).
    closure: Option<ClosureVal>,
    /// The trust stratum of `fact` (ADR-0052 §5). `Verified` by default; an
    /// assert-derived or assert-laundered fact carries `Asserted`.
    stratum: Stratum,
}

impl Known {
    /// A plain value binding at the `Verified` stratum (native seeds, literal
    /// assignments, native-condition refinements — the common case).
    fn value(fact: Fact, line: u32, bound: Option<String>) -> Self {
        Known { fact: Some(fact), line, bound, closure: None, stratum: Stratum::Verified }
    }

    /// A plain value binding at an explicit stratum (derivation sites propagating
    /// `min(inputs)`, and the assert family binding `Asserted`).
    fn value_strat(fact: Fact, line: u32, bound: Option<String>, stratum: Stratum) -> Self {
        Known { fact: Some(fact), line, bound, closure: None, stratum }
    }

    /// A closure binding (no scalar fact; a closure is never assert-derived).
    fn closure(cv: ClosureVal, line: u32) -> Self {
        Known { fact: None, line, bound: None, closure: Some(cv), stratum: Stratum::Verified }
    }

    /// The single proven value, when the fact is a `Singleton` (converted back to
    /// the trace IR's [`ArgValue`]); `None` for every abstract or multi-valued
    /// layer (and for a closure-only binding) — those resolve no proven value.
    fn singleton(&self) -> Option<ArgValue> {
        match &self.fact {
            Some(Fact::Singleton(v)) => Some(arg_of_val(v)),
            _ => None,
        }
    }
}

/// A binding-descent key: the callee (by FQN-ish key) plus its bound params.
type BindingKey = (String, Vec<(String, ArgValue)>);

/// The state threaded down an interprocedural binding descent (Feature B).
struct Descent<'a> {
    provenance: &'a str,
    depth: usize,
    stack: &'a mut Vec<BindingKey>,
    memo: &'a mut HashSet<BindingKey>,
}

/// Walk one scope's trace with a given initial environment.
#[allow(clippy::too_many_arguments)]
fn analyze_scope(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    mut env: HashMap<String, Known>,
    mut store: Store,
    this_exact: Option<String>,
    mut descent: Option<Descent<'_>>,
    mut facts: Option<&mut Vec<LineFact>>,
    dead_out: Option<&mut Vec<Span>>,
    out: &mut Vec<Diagnostic>,
) {
    let enclosing_class = scope_class(scope);

    // The owning function/method's native return type, resolved once. Return-type
    // checking runs only in the plain per-scope pass (`descent.is_none()`), never
    // under an interprocedural binding descent: a descent rebinds the *callee's*
    // parameters, not its return, so re-checking there would only duplicate the
    // per-scope finding (and the file's own strict mode already governs it). The
    // returned value is resolved against the *file's own* `strict` via `cx`.
    let ret_info: Option<(&NativeType, String)> =
        if descent.is_none() { cx.scope_return(scope) } else { None };
    // The owning declaration's `@return` phpdoc envelope, resolved the same way
    // (plain per-scope pass only). Checked under contract acceptance, skipped where
    // the native return check already fired (no double-report).
    let ret_phpdoc: Option<(PType, String)> =
        if descent.is_none() { cx.scope_return_phpdoc(scope) } else { None };

    // Native-type parameter seeding (Feature B): seed each parameter with the fact
    // its native type guarantees at runtime. This is sound in BOTH strict and
    // coercive modes: the engine coerces or throws at entry, so inside the body an
    // `int $x` param IS an int (post-coercion) — the seed is a *runtime-enforced*
    // fact, not a guess. A descent already binds actual values for the params the
    // caller supplied; we only seed params still absent from the env (unbound
    // params may still carry their native-type fact — sound).
    if let Some(params) = cx.scope_params(scope) {
        for p in params {
            if env.contains_key(&p.name) || store.is_bound(&p.name) {
                continue;
            }
            if let Some(fact) = seed_fact(p) {
                env.insert(p.name.clone(), Known::value(fact, 0, Some("native parameter type".to_owned())));
            }
        }
    }

    // Contract-fact seeding (ADR-0052 §9) — THE entry-state contribution of this
    // ADR (ADR-0048 §3 canonical entry state, defined at landing, not retrofitted):
    // per declared parameter, the native member list (`Verified`) refined by the
    // declared `@param` phpdoc envelope (`Asserted`), the ADR-0037 trust order. The
    // arm lane lives in the walk-local `Store` (cloned as `bclasses` at every
    // branch); a descent that already bound a param's value gets no lane (its value
    // is known, the declared possibilities are moot). Every other narrowing carrier
    // (guard facts, members, static-prop channels) contributes nothing to entry
    // state — the deliberately boring §9 answer.
    if descent.is_none()
        && let Some(params) = cx.scope_params(scope)
    {
        let envelopes = cx.scope_envelopes(scope);
        for p in params {
            if store.contract.contains_key(&p.name) {
                continue;
            }
            let phpdoc = envelopes.as_ref().and_then(|e| {
                // An assertion-target `@param` states a post-condition, not the
                // parameter's declared type — never seed a lane from it.
                if e.is_assert_target(&p.name) { None } else { e.param(&p.name) }
            });
            // Resolve phpdoc class arms in the param's namespace context (its offset
            // falls in the same region as the `@param` docblock), matching the FQNs
            // the `instanceof` subtrahend and S6's `find_class` use.
            let resolve = |n: &str| {
                cx.resolve_pclass(cx.cur, p.span.start, n).trim_start_matches('\\').to_ascii_lowercase()
            };
            if let Some(arms) = seed_contract_arms(p, phpdoc, &resolve)
                && !arms.is_empty()
            {
                store.contract.insert(p.name.clone(), arms);
            }
        }
    }

    // Seed the `$this` object in a method scope (ADR-0036): props/readonly from the
    // class surface. Only when the class declares tracked properties (otherwise
    // `$this` stays unbound — identical to pre-heap behavior). A descent that
    // already bound `this` (impossible today — descents pass an empty store) is left
    // untouched.
    if let Some(class) = enclosing_class
        && !store.is_bound("this")
    {
        // G1 (audit): `$this`'s heap class is a LOWER BOUND — any subclass instance
        // may be running this method — UNLESS exactness is locally provable. A
        // binding descent that proved the exact receiver (`this_exact`) makes `$this`
        // exactly that class; otherwise the enclosing class is the class, exact only
        // when it is `final` or an enum (no subclass can exist).
        let (this_class, exact): (&str, bool) = match this_exact.as_deref() {
            Some(exact) => (exact, true),
            None => (class, cx.this_class_exact(class)),
        };
        if let Some(obj) = seed_this_object(cx, this_class, exact) {
            let id = store.heap.keys().copied().max().map_or(0, |m| m + 1);
            store.heap.insert(id, obj);
            store.refs.insert("this".to_owned(), id);
        }
    }

    // The allocation counter starts past any id already in the store (the seeded
    // `$this`), so a fresh `new`/`clone` never collides with it.
    let alloc_start = store.heap.keys().copied().max().map_or(0, |m| m + 1);
    let w = WalkCx {
        cx,
        scope,
        enclosing_class,
        this_exact: this_exact.as_deref(),
        ret_info: &ret_info,
        ret_phpdoc: &ret_phpdoc,
        dead: std::cell::RefCell::new(Vec::new()),
        alloc: std::cell::Cell::new(alloc_start),
    };
    walk_trace(&w, folder, &scope.stmts, &mut env, &mut store, &mut descent, &mut facts, false, out);
    if let Some(sink) = dead_out {
        sink.extend(w.dead.into_inner());
    }
}

/// Whether a walked (sub-)trace runs off its end (its successor is reachable) or
/// terminates (`return`/`throw`/`exit`, or an `if` where no branch falls through).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    FellThrough,
    Terminated,
}

/// The immutable context shared across a scope's recursive branch walk (ADR-0031).
struct WalkCx<'a, 'w> {
    cx: &'w Cx<'a>,
    scope: &'w Scope,
    enclosing_class: Option<&'w str>,
    this_exact: Option<&'w str>,
    ret_info: &'w Option<(&'a NativeType, String)>,
    ret_phpdoc: &'w Option<(PType, String)>,
    /// Proven-dead statement spans discovered during this walk (skipped decided
    /// branches, unreachable tails). Only the PLAIN per-scope walk's regions are
    /// universal truths — a binding descent's dead branches are dead *for that
    /// binding only*, so descents discard theirs (`dead_out: None`).
    dead: std::cell::RefCell<Vec<Span>>,
    /// A monotone allocation-id counter for this scope walk (ADR-0036). Shared
    /// across branch clones (they clone the `Store`, not this cell), so a `new` in
    /// one branch never collides with a `new` in another that later joins.
    alloc: std::cell::Cell<AllocId>,
}

impl WalkCx<'_, '_> {
    /// Mint a fresh allocation id for a `new`/`clone`.
    fn fresh_id(&self) -> AllocId {
        let id = self.alloc.get();
        self.alloc.set(id + 1);
        id
    }
}

/// Record every top-level statement span of the given traces as proven dead.
/// Nested constructs' calls lie within their statement's span, so containment
/// filtering over these covers them. (Skipped `elseif` *conditions* are not
/// yet marked — a literal-arg call inside one is vanishingly rare; TODO.)
fn mark_dead(w: &WalkCx, traces: &[&[Stmt]]) {
    let mut dead = w.dead.borrow_mut();
    for trace in traces {
        for stmt in *trace {
            dead.push(stmt.span);
        }
    }
}

/// Whether a byte position falls inside any proven-dead region.
fn in_dead(dead: &[Span], pos: u32) -> bool {
    dead.iter().any(|s| s.start <= pos && pos < s.end)
}

/// Walk an ordered statement (sub-)trace against a mutable env, threading the same
/// findings sink, descent, and facts. Returns whether the trace falls through.
/// Statements after a terminator are unreachable and are **not** walked (ADR-0031
/// closes ADR-0027's dead-fallthrough gap).
#[allow(clippy::too_many_arguments)]
fn walk_trace(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmts: &[Stmt],
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    guarded: bool,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let cx = w.cx;
    let scope = w.scope;
    for (stmt_idx, stmt) in stmts.iter().enumerate() {
        // 1. Check + descend every statically-named call this statement carries.
        for call in checkable_calls(&stmt.kind) {
            match &call.receiver {
                Callee::Function(_) => {
                    check_propagated_call(
                        cx, folder, scope.poisoned, descent.is_some(), call, env, store, out,
                    );
                    // Userland function arity (ADR-0049 §6 / S5): judged once in the
                    // plain per-scope pass, like the absence flagship.
                    if descent.is_none() {
                        check_arity(cx, folder, call, store, scope.poisoned, out);
                        // The dump surface (ADR-0053 D3/D4): a recognized
                        // `PHPStan\dumpType`-family or `var_dump` call emits its fact
                        // rendering at this position. Plain per-scope pass only, so a
                        // site is dumped once (never re-emitted under a binding descent).
                        emit_dumps(w, folder, call, env, store, out);
                    }
                    try_descend_function(cx, folder, call, env, scope.poisoned, descent.as_mut(), out);
                }
                Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
                    // Branch-sensitive null-dereference proof (ADR-0031): a `$v->m()`
                    // whose receiver is proven `Singleton(null)` on this path.
                    check_call_on_null(w, call, env, out);
                    // The absence flagship (ADR-0049 §4 / S2): fire only in the plain
                    // per-scope pass — every scope (method bodies included) is walked
                    // once there, so a descent must not re-judge the same site.
                    if descent.is_none() {
                        check_undefined_method(cx, folder, call, store, scope.poisoned, out);
                        // The declared-receiver lane (ADR-0049 §8 / S6): a method
                        // absent on a phpdoc-declared receiver narrowed by branch
                        // analysis, under per-arm descendant closure. Disjoint from
                        // S2 by construction — S2 fires on class_exact receivers, S6
                        // only on non-exact receivers carrying a narrowed arm lane.
                        check_phpdoc_undefined_method(cx, folder, call, store, scope.poisoned, out);
                        // Method / constructor / static arity (ADR-0049 §6 / S5),
                        // under a proven-exact receiver only (the declared-receiver
                        // variant is unsound — see `resolve_arity_method`).
                        check_arity(cx, folder, call, store, scope.poisoned, out);
                    }
                    handle_method_call(
                        cx,
                        folder,
                        scope,
                        call,
                        env,
                        store,
                        w.this_exact,
                        w.enclosing_class,
                        descent.as_mut(),
                        out,
                    );
                }
                // `$fn(...)` — resolve the callee variable against the env: a proven
                // closure value descends into its scope (ADR-0033), a proven string
                // resolves as a function name.
                Callee::DynamicVar(name) => {
                    handle_var_call(cx, folder, scope, name, call, env, descent.as_mut(), out);
                }
                Callee::Dynamic => {}
            }
        }

        // 1z. Offset family (ADR-0049 §7 / S3): fire `offset.missing` /
        // `offset.on-unsupported` at the whitelisted read positions only (A7) — a
        // plain assignment-RHS and a return operand whose value is directly an
        // `OffsetRead`. Judged once per site in the plain per-scope pass
        // (`descent.is_none()`), reading the pre-statement env (which already carries
        // this sub-trace's branch refinements — e.g. an `=== []` guard narrowing the
        // container to `Singleton([])`).
        if descent.is_none()
            && let StmtKind::Assign { value: ArgValue::OffsetRead { base, key }, span, .. }
            | StmtKind::Return { value: ArgValue::OffsetRead { base, key }, span, .. } = &stmt.kind
        {
            check_offset_read(cx, folder, base, key, env, scope.poisoned, *span, out);
        }

        // 1a. Escape + sweep (ADR-0036): passing an object into a call escapes it;
        // an unknown/overridable call — or any call an object was passed into —
        // sweeps every escaped object's non-readonly props. `$this` is pre-escaped,
        // so an overridable call on it (unresolved via the guard) sweeps it, while a
        // private/final call (resolved, no object args) leaves it intact.
        apply_call_escape_and_sweep(w, &stmt.kind, store);

        // 1b. Return-type check (native + phpdoc contract); see the original notes.
        if let StmtKind::Return { value, span, .. } = &stmt.kind {
            let mut native_fired = false;
            // A proven scalar (env/fold), or a proven object / class constant
            // (ADR-0043 stage 3 return path); the object arm rides the is-a oracle.
            let ret_val = cx
                .resolve_literal(value, env, scope.poisoned, folder)
                .or_else(|| cx.resolve_static_value(value, w.enclosing_class));
            // The native return check is proof-layer (`type.return-mismatch`): a
            // returned value proven only through an `Asserted` fact stays silent
            // (ADR-0052 §5). The phpdoc contract check below accepts `Asserted`.
            if let Some((ret, display)) = w.ret_info
                && value_stratum(value, env, Some(&*store)) == Stratum::Verified
                && let Some(lit) = ret_val.as_ref()
                && is_type_error(cx, ret, lit)
                && !object_world_guard_blind(descent.is_some(), ret, lit)
            {
                out.push(cx.return_diagnostic(span.start, lit, ret, display));
                native_fired = true;
            }
            if !native_fired
                && let Some((pret, display)) = w.ret_phpdoc
            {
                // Proven-value path, then the abstract-fact path (Feature E) —
                // same discipline as the `@param` check: only a definite `No`.
                let rendered = match cx.resolve_cval(value, env, store, scope.poisoned, folder) {
                    // ADR-0043 stage 4: the class arm of `accepts` now yields definite
                    // verdicts; a class-touching return verdict is guard-blind inside a
                    // descent (mirror of the native `object_world_guard_blind`).
                    Some(cv) => (accepts(cx, cx.cur, span.start, pret, &cv) == Certainty::No
                        && !phpdoc_object_guard_blind(descent.is_some(), pret, Some(&cv)))
                    .then(|| rendered_cval(&cv)),
                    None => arg_abstract_fact(value, env, scope.poisoned).and_then(|fact| {
                        let cty = steins_contract::lower(pret);
                        // The class valve opens for a pure known-class contract against
                        // a definite scalar fact (see `check_phpdoc_param`).
                        let open_class_valve = is_pure_class_contract(cx, cx.cur, span.start, pret)
                            && !phpdoc_object_guard_blind(descent.is_some(), pret, None);
                        ((!contract_touches_class(&cty) || open_class_valve)
                            && steins_contract::admits_fact(&cty, fact) == Certainty::No)
                            .then(|| describe_fact(fact))
                    }),
                };
                if let Some(rendered) = rendered {
                    let pos = cx.tree().position(span.start);
                    out.push(Diagnostic {
                        id: RETURN_MISMATCH_ID,
                        facet: None,
                        path: cx.path().to_owned(),
                        line: pos.line,
                        column: pos.column,
                        message: format!(
                            "return value {rendered} violates declared @return {pret} of {display}() — declared contract violation",
                        ),
                    });
                }
            }
        }

        // 2. Apply the statement's own effect on the environment + compute its flow.
        let flow = match &stmt.kind {
            StmtKind::Barrier => {
                env.clear();
                store.clear();
                Flow::FellThrough
            }
            // `echo` assigns nothing on its own; anything it *can* mutate (embedded
            // assignment / by-ref call) is in `invalidated` (step 3). Reading a
            // variable in an echo no longer forgets it (ADR-0031 precision payoff).
            StmtKind::Echo(_) => Flow::FellThrough,
            // A still-`Opaque` construct (loop / switch / try) forgets what it may
            // write AND what it branches on (reads) — unchanged from ADR-0027,
            // since the trace does not model its control flow.
            StmtKind::Opaque { writes, reads, poisons } => {
                if *poisons {
                    env.clear();
                    store.clear();
                } else {
                    for v in writes.iter().chain(reads) {
                        env.remove(v);
                        store.unbind(v);
                    }
                }
                Flow::FellThrough
            }
            StmtKind::Call(_) => Flow::FellThrough,
            // `assert($expr)` narrows the fall-through env with the guard's
            // true-branch refinements (ADR-0052 §5). A failed *enabled* assertion
            // throws, so continuing means the condition held. The stratum is
            // `Asserted` by default (under `zend.assertions=-1` the expression is
            // never evaluated — no runtime guarantee), `Verified` only when the boot
            // truth declares assertions enabled.
            StmtKind::Assert { cond } => {
                let stratum =
                    if w.cx.zend_assertions { Stratum::Verified } else { Stratum::Asserted };
                apply_refinements(&then_refinements(cond), env, store, stratum);
                Flow::FellThrough
            }
            // Terminators: the trace stops; the remainder is unreachable.
            StmtKind::Return { value, .. } => {
                // `return $o;` escapes the returned object (ADR-0036).
                if let ArgValue::Var(v) = value {
                    store.mark_escaped(v);
                }
                for v in &stmt.invalidated {
                    env.remove(v);
                    store.unbind(v);
                }
                return Flow::Terminated;
            }
            StmtKind::Throw { .. } | StmtKind::Exit { .. } => {
                for v in &stmt.invalidated {
                    env.remove(v);
                    store.unbind(v);
                }
                return Flow::Terminated;
            }
            StmtKind::Assign { var, value, span, .. } => {
                apply_assign(w, folder, var, value, span.start, env, store, facts);
                Flow::FellThrough
            }
            StmtKind::PropAssign { target_var, prop, value, span, .. } => {
                // Property checks run only in the plain per-scope pass (like the
                // return check): a binding descent rebinds the callee's params to
                // hypothetical caller values that in-body guards (unmodeled here)
                // would narrow — checking a descent-bound property write is
                // guard-blind and unsound. The heap update always runs so reads
                // within the descent still resolve.
                let checks_enabled = descent.is_none();
                apply_prop_assign(
                    w, folder, target_var, prop, value, span.start, guarded, checks_enabled, env,
                    store, out,
                );
                Flow::FellThrough
            }
            StmtKind::If { cond, then_trace, elseifs, else_trace } => walk_if(
                w, folder, cond, then_trace, elseifs, else_trace.as_deref(), env, store,
                descent, facts, out,
            ),
            StmtKind::Match { subject, arms, default, loose } => walk_match(
                w, folder, subject, arms, default.as_deref(), *loose, env, store, descent,
                facts, out,
            ),
        };

        // 3. Apply `@phpstan-assert` (Always) narrowings from every call in this
        // statement (Feature D), collecting the vars they establish. This runs
        // BEFORE the by-ref invalidation below so the replace-if-weaker decision
        // sees a proven `Singleton`/`OneOf` (kept over a weaker asserted fact); the
        // asserted vars are then protected from the conservative forget, since the
        // assertion helper's contract is a *stronger* statement than "the call may
        // have mutated this by reference".
        let mut asserted: HashSet<String> = HashSet::new();
        for call in checkable_calls(&stmt.kind) {
            apply_stmt_asserts(
                cx, scope, call, env, store, w.this_exact, w.enclosing_class, &mut asserted,
            );
        }

        // 4. After the statement, invalidate any variable handed to a call — except
        // one an assertion just narrowed (its post-call fact is known).
        for v in &stmt.invalidated {
            if asserted.contains(v) {
                continue;
            }
            env.remove(v);
            store.unbind(v);
        }

        if flow == Flow::Terminated {
            // The rest of this trace is proven unreachable (ADR-0031).
            mark_dead(w, &[&stmts[stmt_idx + 1..]]);
            return Flow::Terminated;
        }
    }
    Flow::FellThrough
}

/// The trust stratum a resolved value carries (ADR-0052 §5 derivation clause): the
/// minimum over every env/heap fact consumed while resolving `value`. A literal, a
/// `new`/enum/const, or a fully-literal subtree is `Verified`; a bare `$var` takes
/// its env stratum; a property fetch takes the prop's stratum; an array literal or
/// a foldable call takes the min over its elements/arguments; a ternary the min of
/// its arms. Consulted only where resolution succeeded (so consumed vars were
/// proven), it stamps the derived binding with `min(inputs)`, closing the
/// laundering hazard the audit's `$pair = [$x, 99]` snippet names.
fn value_stratum(value: &ArgValue, env: &HashMap<String, Known>, store: Option<&Store>) -> Stratum {
    match value {
        ArgValue::Var(name) => env.get(name).map_or(Stratum::Verified, |k| k.stratum),
        // A property fetch takes its prop's stratum; with no store in scope (the
        // variable-call check) a prop fetch never resolves to a proof premise, so
        // `Verified` is the correct neutral answer.
        ArgValue::PropFetch { var, prop } => {
            store.map_or(Stratum::Verified, |s| s.prop_stratum(var, prop))
        }
        ArgValue::Array(items) => items
            .iter()
            .fold(Stratum::Verified, |acc, (_, v)| acc.min(value_stratum(v, env, store))),
        ArgValue::Call(_, args) => {
            args.iter().fold(Stratum::Verified, |acc, v| acc.min(value_stratum(v, env, store)))
        }
        ArgValue::Ternary { then_val, else_val, .. } => {
            value_stratum(then_val, env, store).min(value_stratum(else_val, env, store))
        }
        // `$a ?? $b` consumes both operands' facts (a widening join): `min` (§5).
        ArgValue::Coalesce(a, b) => {
            value_stratum(a, env, store).min(value_stratum(b, env, store))
        }
        _ => Stratum::Verified,
    }
}

// ---------------------------------------------------------------------------
// The dump surface (ADR-0053): requested introspection — an "answered question".
// Emitted mid-walk at the call position, reading (never binding) the walk's facts
// (§7 / §10); the plain per-scope pass only (`descent.is_none()`), so a site is
// dumped once. The explicit pair (D3) is recognized by resolved FQN; `var_dump`
// (D4) by the PHP fallback rule. Rendering shares the ONE speller (`spell_arms`).
// ---------------------------------------------------------------------------

/// The honest-incompleteness rendering (ADR-0053 §7): the dump knows nothing faithful
/// to spell about the expression. Never a guess, never a `mixed` pretense.
const DUMP_UNKNOWN: &str = "unknown";

/// The `debug.phpdoc-type` rendering when the contract carrier is empty (ADR-0053
/// §2): no declared `@param`/native envelope narrows the expression — never a
/// synthesized type.
const DUMP_NO_CONTRACT: &str = "no declared contract";

/// Which explicit dump the reserved FQN names (ADR-0053 §2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DumpFamily {
    /// `PHPStan\dumpType($e)` → `debug.type`: the trust-ordered best value fact.
    Type,
    /// `PHPStan\dumpPhpDocType($e)` → `debug.phpdoc-type`: the declared arm list.
    PhpDocType,
}

/// A rendered dump fact plus whether it rode an `Asserted`-stratum premise (ADR-0053
/// §2 / ADR-0052 §5): an asserted fact carries an explicit `(asserted)` marker so the
/// introspection surface never launders a docblock claim into a proven value.
struct DumpRendering {
    text: String,
    asserted: bool,
}

/// The resolved function FQN a call names (ADR-0001 name resolution), lowercase-
/// normalized and leading-`\`-stripped — **definition-insensitive** (no index lookup:
/// the reserved dump pair is recognized regardless of whether a userland definition
/// exists, ADR-0053 §5). Mirrors [`Cx::resolve_function`]'s name computation but
/// yields the FQN string rather than a resolution verdict.
fn resolved_fn_fqn(cx: &Cx, r: &NameRef) -> String {
    match r.kind {
        RefKind::FullyQualified => r.raw.to_ascii_lowercase(),
        RefKind::Qualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            fqn.to_ascii_lowercase()
        }
        RefKind::Unqualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            // A `use function` import resolves the name outright.
            if let Some(t) = ctx.fn_imports.get(&name) {
                return t.to_ascii_lowercase();
            }
            // Otherwise PHP tries the current-namespace candidate first; the global
            // fallback (bare `name`, no separator) never matches a reserved `PHPStan\`
            // FQN, so the namespace candidate is the only one recognition needs.
            if ctx.namespace.is_empty() {
                name
            } else {
                format!("{}\\{}", ctx.namespace.to_ascii_lowercase(), name)
            }
        }
    }
}

/// Which explicit dump a `Callee::Function` call is, recognized by resolved FQN
/// (ADR-0053 §5): the reserved `PHPStan\` pair, definition-insensitive and
/// case-insensitive. `None` for every other call.
fn dump_family(cx: &Cx, call: &CallExpr) -> Option<DumpFamily> {
    let Callee::Function(_) = &call.receiver else { return None };
    let r = call.callee_ref.as_ref()?;
    match resolved_fn_fqn(cx, r).as_str() {
        DUMP_TYPE_FQN => Some(DumpFamily::Type),
        DUMP_PHPDOC_TYPE_FQN => Some(DumpFamily::PhpDocType),
        _ => None,
    }
}

/// Whether a call resolves to the **global** `var_dump()` under PHP's own name
/// resolution and fallback rule (ADR-0053 §5 / D4) — the `debug.var-dump` trigger.
/// The six enumerated legs:
///
/// - (a) `\var_dump($e)` — always (the fully-qualified global builtin);
/// - (b) unqualified `var_dump($e)` in the root namespace — always;
/// - (c) unqualified in `namespace Foo;` — only if `Foo\var_dump` is **provably
///   undefined** (the runtime falls back to global); a same-namespace homonym, an
///   ambiguous resolution, or a dam that leaves existence Unknown ⇒ **no dump** (a
///   missed dump is never an FP — silence is the free safe side);
/// - (d) `Foo\var_dump($e)` qualified, or `use function Foo\var_dump;` — never
///   (resolves elsewhere); a `use function var_dump;` importing the global is still
///   the global, so it dumps;
/// - (e) a *method* `$o->var_dump()` / `static::var_dump()` — never (a different
///   symbol space; `Callee::Method`/`Static`, not `Function`);
/// - (f) first-class callables and string callables — never (no argument expression
///   at the site to dump — handled by the arg-less guard in [`emit_dumps`]).
fn recognizes_var_dump(cx: &Cx, call: &CallExpr) -> bool {
    let Callee::Function(_) = &call.receiver else { return false };
    let Some(r) = call.callee_ref.as_ref() else { return false };
    match r.kind {
        // (a) `\var_dump` — the global builtin (a single segment, no namespace).
        RefKind::FullyQualified => r.raw.eq_ignore_ascii_case("var_dump"),
        // (d) `Foo\var_dump` — a qualified name resolves elsewhere.
        RefKind::Qualified => false,
        RefKind::Unqualified => {
            if !r.raw.eq_ignore_ascii_case("var_dump") {
                return false;
            }
            let ctx = cx.tree().ctx_at(r.offset);
            // (d) `use function ...\var_dump;` — resolves to the import target; only a
            // `use function var_dump;` naming the global is still the trigger.
            if let Some(t) = ctx.fn_imports.get("var_dump") {
                return t.eq_ignore_ascii_case("var_dump");
            }
            // (b) the root namespace: always the global.
            if ctx.namespace.is_empty() {
                return true;
            }
            // (c) in a namespace: only if `Ns\var_dump` is provably undefined (index
            // Absent) AND the dam is clear (dynamic code could otherwise mint it,
            // leaving existence Unknown — silence, the free safe side).
            let ns_fqn = format!("{}\\var_dump", ctx.namespace).to_ascii_lowercase();
            matches!(cx.index.resolve_function(&ns_fqn), Res::Absent) && cx.dam.is_clear()
        }
    }
}

/// A first-class callable `f(...)` (ADR-0049 §6 shape): a non-positional call with
/// all of `args`/`named_args` empty and no spread. It creates a `Closure`, not a
/// call — there is no argument expression at the site to dump (ADR-0053 §5 leg f),
/// and a reserved-name first-class callable is not a dumping call either.
fn is_first_class_callable(call: &CallExpr) -> bool {
    !call.positional_only && call.args.is_empty() && call.named_args.is_empty() && !call.has_spread
}

/// Render a value-domain [`Fact`] for the dump surface through the ONE shared
/// spelling (ADR-0053 §7). Finite layers (`Singleton`/`OneOf`) go through the N1
/// normalizer ([`normalize::summarize_vals`]) and the shared plain-text speller
/// ([`steins_contract::spell::spell_arms`]) — the same path the value-domain docblock
/// renderer shares (the D2 extraction), so a dump's finite-fact rendering byte-equals
/// the speller's output for that fact (the parity pin). The abstract layers
/// (`Refined`/`General`) carry no enumerable value set; they render as the honest
/// phpdoc keyword ladder, reusing the speller's own `preds_keyword` for refined
/// strings so the two agree. A set with no faithful scalar spelling (an array member)
/// renders as honest [`DUMP_UNKNOWN`].
fn render_dump_fact(fact: &Fact) -> String {
    if let Some(members) = fact.finite_members() {
        return normalize::summarize_vals(members)
            .and_then(|arms| steins_contract::spell::spell_arms(&arms))
            .unwrap_or_else(|| DUMP_UNKNOWN.to_owned());
    }
    match fact {
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            with_null(int_range_keyword(*r), *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            with_null(steins_contract::spell::preds_keyword(*p), *nullable)
        }
        Fact::Refined { base, nullable, .. } => with_null(base_keyword(*base).to_owned(), *nullable),
        Fact::General { base, nullable } => with_null(base_keyword(*base).to_owned(), *nullable),
        // Finite layers are handled above.
        Fact::Singleton(_) | Fact::OneOf(_) => DUMP_UNKNOWN.to_owned(),
    }
}

/// Append `|null` when the fact admits null (the honest nullable spelling).
fn with_null(s: String, nullable: bool) -> String {
    if nullable { format!("{s}|null") } else { s }
}

/// The bare phpdoc keyword for a scalar base.
fn base_keyword(b: Base) -> &'static str {
    match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    }
}

/// The tightest int-range keyword (mirrors [`describe_fact`]'s ladder): the named
/// predicate classes, else the explicit `int<lo, hi>` interval.
fn int_range_keyword(r: IntRange) -> String {
    if r == IntRange::POSITIVE {
        "positive-int".to_owned()
    } else if r == IntRange::NEGATIVE {
        "negative-int".to_owned()
    } else if r == IntRange::NON_NEGATIVE {
        "non-negative-int".to_owned()
    } else {
        format!("int<{}, {}>", r.lo(), r.hi())
    }
}

/// Render a narrowed contract-fact arm list (ADR-0052 §1 carrier) for the dump
/// surface. Scalar arms spell through the shared [`steins_contract::spell::spell_arms`];
/// a pure class/`null` arm list renders each class's simple name; anything else has
/// no faithful spelling (`None` → the caller falls to honest unknown).
fn render_contract_arms(arms: &[ContractArm]) -> Option<String> {
    let tys: Vec<ContractTy> = arms.iter().map(|a| a.ty.clone()).collect();
    if let Some(scalar) = steins_contract::spell::spell_arms(&tys) {
        return Some(scalar);
    }
    let mut parts = Vec::new();
    for ty in &tys {
        match ty {
            ContractTy::Class(n) => parts.push(n.rsplit('\\').next().unwrap_or(n).to_owned()),
            ContractTy::Null => parts.push("null".to_owned()),
            // An array/generic/shape/callable/intersection arm has no faithful plain
            // spelling here — honest unknown rather than a guess (§7).
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("|"))
}

/// The best value fact of a dump argument, in the trust order (ADR-0052 §1 /
/// ADR-0037): a proven value fact, else the object holder's exact class / membership,
/// else the narrowed declared-arm list, else honest unknown. Drives `debug.type` and
/// `debug.var-dump` (identical rendering, identical fact source, ADR-0053 §2).
fn best_dump_type(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> DumpRendering {
    let cx = w.cx;
    let poisoned = w.scope.poisoned;
    if let ArgValue::Var(name) = value {
        // 1. A proven value fact (the four-layer value domain), carrying its stratum.
        if let Some(known) = env.get(name)
            && let Some(fact) = &known.fact
        {
            return DumpRendering {
                text: render_dump_fact(fact),
                asserted: known.stratum == Stratum::Asserted,
            };
        }
        // 2. An object holder: the heap's exact class (else the lower-bound class).
        if let Some(obj) = store.obj_of(name) {
            return DumpRendering { text: obj.class.clone(), asserted: false };
        }
        // 3. The narrowed declared-arm list (contract carrier).
        if let Some(arms) = store.contract_arms(name)
            && let Some(text) = render_contract_arms(arms)
        {
            return DumpRendering {
                text,
                asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
            };
        }
        // 4. Honest unknown.
        return DumpRendering { text: DUMP_UNKNOWN.to_owned(), asserted: false };
    }
    // A non-variable argument: a resolved literal / foldable value fact, else unknown.
    if let Some(lit) = cx.resolve_literal(value, env, poisoned, folder)
        && let Some(fact) = singleton_fact(&lit)
    {
        return DumpRendering {
            text: render_dump_fact(&fact),
            asserted: value_stratum(value, env, Some(store)) == Stratum::Asserted,
        };
    }
    DumpRendering { text: DUMP_UNKNOWN.to_owned(), asserted: false }
}

/// The declared-side view of a dump argument (ADR-0053 §2, `debug.phpdoc-type`): the
/// contract-fact arm list (the declared envelope as narrowed by guards), or
/// `no declared contract` when the carrier is empty — never a synthesized type.
fn best_dump_phpdoc_type(value: &ArgValue, store: &Store) -> DumpRendering {
    if let ArgValue::Var(name) = value
        && let Some(arms) = store.contract_arms(name)
        && let Some(text) = render_contract_arms(arms)
    {
        return DumpRendering {
            text,
            asserted: arms.iter().any(|a| a.stratum == Stratum::Asserted),
        };
    }
    DumpRendering { text: DUMP_NO_CONTRACT.to_owned(), asserted: false }
}

/// The message frame around a rendered dump fact (ADR-0053 §7: wording is not a
/// contract, the rendered fact is). Carries the `(asserted)` marker when the fact
/// rode a docblock/assert premise.
fn dump_message(label: &str, r: &DumpRendering) -> String {
    let marker = if r.asserted { " (asserted)" } else { "" };
    format!("{label}: {}{marker}", r.text)
}

/// Emit the dump reports a recognized call site produces (ADR-0053 §7): the explicit
/// pair (D3) by resolved FQN, `var_dump` (D4) by the PHP fallback rule. One report
/// per positional argument, in argument order; a zero-argument `dumpType()` still
/// reports (fail-level, "nothing to dump" — the committed call is a runtime fatal
/// either way). Reads the walk's facts at the call position; binds nothing (§10 §3).
fn emit_dumps(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if let Some(family) = dump_family(cx, call) {
        // A first-class callable `dumpType(...)` is a Closure, not a dumping call.
        if is_first_class_callable(call) {
            return;
        }
        let (id, label, call_name) = match family {
            DumpFamily::Type => (DEBUG_TYPE_ID, "dumped type", "PHPStan\\dumpType()"),
            DumpFamily::PhpDocType => {
                (DEBUG_PHPDOC_TYPE_ID, "dumped phpdoc type", "PHPStan\\dumpPhpDocType()")
            }
        };
        if call.args.is_empty() {
            // Zero-argument explicit dump: still fail-level (§7) — the runtime fatal
            // stands regardless of what (nothing) it would dump.
            let pos = cx.tree().position(call.span.start);
            out.push(Diagnostic {
                id,
                facet: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: format!("{call_name} called with no argument — nothing to dump"),
            });
            return;
        }
        for arg in &call.args {
            let rendering = match family {
                DumpFamily::Type => best_dump_type(w, folder, &arg.value, env, store),
                DumpFamily::PhpDocType => best_dump_phpdoc_type(&arg.value, store),
            };
            let pos = cx.tree().position(arg.span.start);
            out.push(Diagnostic {
                id,
                facet: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: dump_message(label, &rendering),
            });
        }
        return;
    }

    // var_dump (ADR-0053 D4): default-on, one `debug.type`-shaped report per argument,
    // same rendering and same fact source as the explicit `debug.type`. A first-class
    // callable and a zero-argument `var_dump()` dump nothing (§2/§5 leg f) — no
    // argument expression exists at the site.
    if recognizes_var_dump(cx, call) {
        if is_first_class_callable(call) || call.args.is_empty() {
            return;
        }
        for arg in &call.args {
            let rendering = best_dump_type(w, folder, &arg.value, env, store);
            let pos = cx.tree().position(arg.span.start);
            out.push(Diagnostic {
                id: DEBUG_VAR_DUMP_ID,
                facet: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: dump_message("dumped type", &rendering),
            });
        }
    }
}

/// Apply a plain `$var = <value>;` assignment to the env (extracted from the walk).
#[allow(clippy::too_many_arguments)]
fn apply_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    value: &ArgValue,
    span_start: u32,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    facts: &mut Option<&mut Vec<LineFact>>,
) {
    let cx = w.cx;
    let line = cx.tree().position(span_start).line;

    // A ternary rvalue `$x = $c ? A : B` is a conditional value (ADR-0031): the
    // walk evaluates the guard and resolves to the chosen arm, or (undecided) a
    // `OneOf` of the two arms when both are literal, else unknown.
    if let ArgValue::Ternary { cond, then_val, else_val } = value {
        match eval_ternary_fact(w, folder, cond, then_val, else_val, env, store) {
            Some(fact) => {
                if let (Fact::Singleton(lit), Some(facts)) = (&fact, facts.as_deref_mut()) {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: render_val(lit) },
                    });
                }
                // Derivation clause: the arm chosen is one of the two operands, so
                // the result stratum is `min` over the arms (either could be the
                // taken one under a `Maybe` verdict).
                let strat = value_stratum(then_val, env, Some(&*store)).min(value_stratum(else_val, env, Some(&*store)));
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                store.unbind(var);
            }
            None => {
                env.remove(var);
                store.unbind(var);
            }
        }
        return;
    }

    // A closure value `$f = fn(...) => …;` / `$f = strtolower(...);` (ADR-0033):
    // record a `ClosureVal` with its by-value capture snapshot taken from the
    // CURRENT (definition-site) env — the semantically correct PHP by-value
    // capture. A poisoned scope drops it (no reliable capture snapshot).
    if let ArgValue::Closure(cref) = value {
        env.remove(var);
        store.unbind(var);
        if w.scope.poisoned {
            return;
        }
        // A closure that captures an object escapes it (ADR-0036): the closure holds
        // the object handle, so an unknown call may reach and mutate it.
        if let steins_syntax::ClosureRef::Anonymous { captures, .. } = cref {
            for name in captures {
                store.mark_escaped(name);
            }
        }
        if let Some(cv) = build_closure_val(cx, cref, line, env) {
            env.insert(var.to_owned(), Known::closure(cv, line));
        }
        return;
    }

    match value {
        // `$x = new Foo(args)` (ADR-0036): a fresh allocation, class from resolution,
        // props populated from promoted ctor params + literal defaults.
        ArgValue::New(class_ref, args) => {
            env.remove(var);
            store.unbind(var);
            if !w.scope.poisoned {
                let class = cx.class_fqn(class_ref);
                let id = build_new_object(w, folder, &class, args, env, store);
                store.refs.insert(var.to_owned(), id);
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::ExactClass { var: var.to_owned(), class },
                    });
                }
            }
        }
        // `$b = $a` where `$a` holds an object (ADR-0036 aliasing): copy the ObjRef
        // (shared id), so a later write through either alias is visible via both.
        ArgValue::Var(src) if !w.scope.poisoned && store.is_bound(src) => {
            env.remove(var);
            let id = store.id_of(src).expect("bound var has an id");
            store.refs.insert(var.to_owned(), id);
        }
        // `clone $a` (ADR-0036 adversarial #1): a NEW id with a COPY of the source
        // object's props (PHP shallow clone) — post-clone writes stay isolated.
        ArgValue::Clone(src) if !w.scope.poisoned && store.is_bound(src) => {
            // Read the source id BEFORE unbinding `var`. For a self-clone
            // `$a = clone $a` we have `var == src`, so `store.unbind(var)` would
            // also drop `src`'s binding and make `id_of(src)` return `None`. PHP
            // evaluates the rvalue (`clone $a`) against the current value first
            // and only then assigns, so the source id is the pre-assignment one;
            // capturing it here keeps the guard's `is_bound(src)` invariant true
            // at the `.expect` for every `var`/`src` pairing.
            let src_id = store.id_of(src).expect("bound var has an id");
            env.remove(var);
            store.unbind(var);
            if let Some(src_obj) = store.heap.get(&src_id) {
                let mut copy = src_obj.clone();
                copy.escaped = false; // a fresh, local clone has not escaped
                let id = w.fresh_id();
                store.heap.insert(id, copy);
                store.refs.insert(var.to_owned(), id);
            }
        }
        // `$x = $o->p` (ADR-0036): a property read flows the prop's fact into `$x`,
        // carrying the prop's stratum (derivation clause — heap reads).
        ArgValue::PropFetch { var: recv, prop } if !w.scope.poisoned => {
            env.remove(var);
            store.unbind(var);
            if let Some(fact) = store.prop_fact(recv, prop).cloned() {
                let strat = store.prop_stratum(recv, prop);
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
            }
        }
        // `$x = $a ?? $b` (ADR-0052 §6): the value is the non-null part of `$a`
        // unioned with `$b` — `clear_null(fact($a)) join fact($b)`. A fact only when
        // BOTH operands are visible facts; an unseen operand (an array offset, an
        // unknown call) yields no fact, so `??` never manufactures certainty for a
        // value it cannot spell. The join widens, so it can only *lose* precision
        // (never fire a proof the concrete arms would not) — the FP-safe side.
        ArgValue::Coalesce(a, b) => {
            match eval_coalesce_fact(w, folder, a, b, env) {
                Some(fact) => {
                    // Derivation clause: the value is one of the two operands, so the
                    // stratum is `min` over both (either could be the chosen one).
                    let strat = value_stratum(a, env, Some(&*store))
                        .min(value_stratum(b, env, Some(&*store)));
                    env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                    store.unbind(var);
                }
                None => {
                    env.remove(var);
                    store.unbind(var);
                }
            }
        }
        _ => match cx.resolve_literal(value, env, w.scope.poisoned, folder).and_then(|lit| {
            singleton_fact(&lit).map(|f| (lit, f))
        }) {
            Some((lit, fact)) => {
                if let Some(facts) = facts.as_deref_mut() {
                    facts.push(LineFact {
                        line,
                        kind: FactKind::Value { var: var.to_owned(), rendered: lit.render() },
                    });
                }
                // Derivation clause: folds and array composition resolve through
                // `resolve_literal`, consuming env facts — stamp `min(inputs)`.
                let strat = value_stratum(value, env, Some(&*store));
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
                store.unbind(var);
            }
            None => {
                env.remove(var);
                store.unbind(var);
            }
        },
    }
}

/// The fact of `$a ?? $b` (ADR-0052 §6): `clear_null(fact($a)) join fact($b)`. Both
/// operand facts must be visible — an operand the domain cannot spell yields no
/// fact and the whole expression yields `None` (silent). A provably-null `$a`
/// (`clear_null` empties it) collapses to exactly `fact($b)`.
fn eval_coalesce_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    a: &ArgValue,
    b: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    let fa = arg_value_fact(w, folder, a, env)?;
    let fb = arg_value_fact(w, folder, b, env)?;
    match clear_null(&fa) {
        // `$a` may be non-null: its non-null part unioned with `$b`.
        Some(a_nonnull) => a_nonnull.join(&fb),
        // `$a` is provably null (nothing survives `clear_null`): the value is `$b`.
        None => Some(fb),
    }
}

/// The value-domain fact of an rvalue operand for the `??` join: a bare variable's
/// env fact, or a literal/foldable value's `Singleton`. Non-representable operands
/// (calls, offsets → `Other`, objects) yield `None`.
fn arg_value_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    match arg {
        ArgValue::Var(name) if !w.scope.poisoned => env.get(name)?.fact.clone(),
        _ => {
            let lit = w.cx.resolve_literal(arg, env, w.scope.poisoned, folder)?;
            singleton_fact(&lit)
        }
    }
}

/// Allocate a fresh heap object for `new Class(args)` (ADR-0036), populating its
/// props from literal property defaults and promoted constructor parameters, and
/// its readonly set from `readonly`-declared properties. Returns the allocation id.
fn build_new_object(
    w: &WalkCx,
    folder: &mut dyn Folder,
    class: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: &mut Store,
) -> AllocId {
    let cx = w.cx;
    let id = w.fresh_id();
    let mut obj = HeapObj::new(class.to_owned());
    obj.class_exact = true; // `new Class(...)` allocates exactly `Class` (audit G1)
    let props = cx.class_props(class);

    // readonly set + literal defaults.
    for p in &props {
        if p.readonly {
            obj.readonly.insert(p.name.clone());
        }
        if let Some(default) = &p.default
            && let Some(fact) = singleton_fact(default)
        {
            // Skip null-admitting facts (unsound to flow past unmodeled guards). A
            // literal default is `Verified` (no env fact consumed).
            if !fact_is_nullish(&fact) {
                obj.props.insert(p.name.clone(), PropFact { fact, stratum: Stratum::Verified });
            }
            if p.readonly {
                obj.ro_written.insert(p.name.clone());
            }
        }
    }

    // Promoted constructor params: bind each from its positional `new` argument.
    if let Some(ctor) = cx.find_ctor(class) {
        let promoted: HashMap<&str, &&PropertyDecl> =
            props.iter().filter(|p| p.promoted).map(|p| (p.name.as_str(), p)).collect();
        for (i, param) in ctor.params.iter().enumerate() {
            if param.variadic {
                break;
            }
            let Some(pd) = promoted.get(param.name.as_str()) else { continue };
            // The value: the resolved arg literal (carrying the arg's stratum —
            // derivation clause, heap write), else the param's native-type seed
            // (`Verified`).
            let (fact, stratum) = match args.get(i) {
                Some(a) => match cx
                    .resolve_literal(a, env, w.scope.poisoned, folder)
                    .and_then(|lit| singleton_fact(&lit))
                {
                    Some(f) => (Some(f), value_stratum(a, env, Some(&*store))),
                    None => (seed_fact(param), Stratum::Verified),
                },
                None => (seed_fact(param), Stratum::Verified),
            };
            // Skip null-admitting facts (unsound to flow past unmodeled guards).
            if let Some(fact) = fact
                && !fact_is_nullish(&fact)
            {
                obj.props.insert(pd.name.clone(), PropFact { fact, stratum });
            }
            // A promoted param is *always* written at construction — even when its
            // value is unknown, record the write (readonly.reassigned first write).
            if pd.readonly {
                obj.ro_written.insert(pd.name.clone());
            }
        }
    }

    store.heap.insert(id, obj);
    id
}

/// Seed the `$this` object shell for a method scope (ADR-0036): `class_fqn` (the
/// exact receiver when a descent proved one, else the enclosing class as a lower
/// bound), plus the readonly set and provably-written readonly props from the class
/// surface. `class_exact` records whether that class is exact (audit G1). `$this` is
/// **pre-escaped** (an overridable call on it sweeps its non-readonly props).
/// Returns `None` when the class has no tracked properties (leaving `$this` unbound
/// — identical to pre-heap behavior).
///
/// Crucially this seeds **no property value facts**. A property's value in an
/// arbitrary method is whatever some *other* method last stored (or a `!== null`
/// guard narrowed) — neither of which this per-scope walk models — so assuming the
/// declared default here would be unsound (it produced null-property false
/// positives past `if ($this->x !== null)` guards). Only facts written *in this
/// method* (explicit `$this->p = …`) flow; readonly bookkeeping stays because a
/// readonly value cannot change after construction.
fn seed_this_object(cx: &Cx, class_fqn: &str, class_exact: bool) -> Option<HeapObj> {
    let props = cx.class_props(class_fqn);
    if props.is_empty() {
        return None;
    }
    let mut obj = HeapObj::new(class_fqn.to_owned());
    obj.escaped = true; // pre-escaped
    // Membership is not exactness (audit G1): `$this` is a lower bound unless the
    // caller proved the exact receiver (a binding descent) or the enclosing class
    // has no subclass (`final`/enum). The No-side consumers gate on this bit.
    obj.class_exact = class_exact;
    for p in &props {
        if p.readonly {
            obj.readonly.insert(p.name.clone());
            // A promoted readonly param or a readonly prop with a literal default is
            // provably written by construction — the first write for reassign checks.
            if p.promoted || p.default.is_some() {
                obj.ro_written.insert(p.name.clone());
            }
        }
    }
    Some(obj)
}

/// Whether a fact admits `null` — such a fact must never be *seeded* into a
/// property (ADR-0036): property reads bypass the guard-narrowing that would clear
/// a `!== null` check, so a seeded nullable/null property fact flowing into a
/// non-null sink is a false positive. Explicitly-written facts still flow (they are
/// sound within the linear trace); only construction-time seeding is filtered.
fn fact_is_nullish(f: &Fact) -> bool {
    match f {
        Fact::Singleton(v) => matches!(v, Val::Null),
        Fact::OneOf(vs) => vs.iter().any(|v| matches!(v, Val::Null)),
        Fact::Refined { nullable, .. } | Fact::General { nullable, .. } => *nullable,
    }
}

/// Apply a `$var->prop = <rvalue>` / `$this->prop = <rvalue>` property assignment
/// (ADR-0036): run the property checks (native `type.property-mismatch`, `@var`
/// `phpdoc.property-mismatch`, `readonly.reassigned`), then record the prop's new
/// fact in the heap. An unknown receiver (no tracked object) records nothing (but
/// an object rvalue still escapes — it is now reachable via the property).
#[allow(clippy::too_many_arguments)]
fn apply_prop_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    target_var: &str,
    prop: &str,
    value: &ArgValue,
    span_start: u32,
    guarded: bool,
    checks_enabled: bool,
    env: &HashMap<String, Known>,
    store: &mut Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if w.scope.poisoned {
        return;
    }
    // An object rvalue stored into a property escapes (now reachable via the prop).
    if let ArgValue::Var(src) = value
        && store.is_bound(src)
    {
        store.mark_escaped(src);
    }
    let Some(id) = store.id_of(target_var) else {
        return;
    };
    let class = store.heap.get(&id).expect("bound id present").class.clone();

    // Resolve the rvalue to a proven literal (for the native check) and a fact
    // (for storage + the abstract phpdoc check). The rvalue's trust stratum
    // (ADR-0052 §5) gates the proof-layer native check and is recorded on the prop
    // (derivation clause — heap write).
    let proven_lit = cx.resolve_literal(value, env, false, folder);
    let rvalue_strat = value_stratum(value, env, Some(&*store));
    let prop_fact_val: Option<Fact> = proven_lit.as_ref().and_then(singleton_fact).or_else(|| {
        match value {
            ArgValue::PropFetch { var: rv, prop: rp } => store.prop_fact(rv, rp).cloned(),
            _ => arg_abstract_fact(value, env, false).cloned(),
        }
    });

    // Locate the property declaration on the object's class surface (for its native
    // type and `@var` contract).
    let pdecl = cx.class_props(&class).into_iter().find(|p| p.name == prop && !p.is_static);

    // 1. Native `type.property-mismatch` — a proven literal against a native prop
    // type. Skip promoted props (checked as constructor args; no double-report).
    let mut native_fired = false;
    if checks_enabled
        && rvalue_strat == Stratum::Verified
        && let Some(pd) = pdecl
        && !pd.promoted
        && let Some(ty) = pd.ty.as_ref()
        && let Some(lit) = proven_lit.as_ref()
        && lit.is_literal()
        && is_type_error(cx, ty, lit)
    {
        let pos = cx.tree().position(span_start);
        let mode = if cx.strict() { "strict" } else { "coercive" };
        out.push(Diagnostic {
            id: PROP_MISMATCH_ID,
            facet: None,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "Cannot assign {} to property {}::${} of type {} — proven TypeError ({} mode)",
                lit.render(), simple_class(&class), prop, ty.render(), mode,
            ),
        });
        native_fired = true;
    }

    // 2. phpdoc `@var` `phpdoc.property-mismatch` — a proven or abstract value that
    // provably does not inhabit the property's `@var` contract (definite No only).
    if checks_enabled
        && !native_fired
        && let Some(pd) = pdecl
        && let Some(mut var_ty) = pd.docblock.as_deref().and_then(parse_var_type)
        && let Some((cfile, cdecl)) = cx.find_class(&class)
    {
        // Class-level `@template` names shadow same-named classes in this property's
        // `@var` type (issue #5) — a property is a member docblock too.
        neutralize_templates(&mut var_ty, &template_names_of(cdecl.docblock.as_deref()));
        let coff = pd.span.start;
        let violates = match proven_lit.as_ref().map(|l| CVal::Scalar(l.clone())) {
            Some(cv) if matches!(cv, CVal::Scalar(ref v) if v.is_literal()) => {
                accepts(cx, cfile, coff, &var_ty, &cv) == Certainty::No
            }
            _ => arg_abstract_fact(value, env, false).is_some_and(|fact| {
                let cty = steins_contract::lower(&var_ty);
                !contract_touches_class(&cty)
                    && steins_contract::admits_fact(&cty, fact) == Certainty::No
            }),
        };
        if violates {
            let rendered = proven_lit
                .as_ref()
                .map(ArgValue::render)
                .or_else(|| arg_abstract_fact(value, env, false).map(describe_fact))
                .unwrap_or_else(|| value.render());
            let pos = cx.tree().position(span_start);
            out.push(Diagnostic {
                id: PHPDOC_PROP_MISMATCH_ID,
                facet: None,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: format!(
                    "value {rendered} assigned to property {}::${prop} violates declared @var {var_ty} — declared contract violation",
                    simple_class(&class),
                ),
            });
        }
    }

    // Whether the rvalue is an object handle (computed before the mutable borrow).
    let rval_is_object = matches!(value, ArgValue::Var(src) if store.refs.contains_key(src));

    // 3. `readonly.reassigned` — a second proven write to a readonly property on
    // this (unguarded) path. `guarded` (inside a branch) suppresses it: the second
    // write is not proven on every path (ADR-0036 conservative side).
    let obj = store.heap.get_mut(&id).expect("bound id present");
    let is_readonly = obj.readonly.contains(prop);
    if checks_enabled && is_readonly && obj.ro_written.contains(prop) && !guarded {
        let pos = cx.tree().position(span_start);
        out.push(Diagnostic {
            id: READONLY_REASSIGNED_ID,
            facet: None,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "Cannot modify readonly property {}::${prop} — proven Error",
                simple_class(&class),
            ),
        });
    }

    // 4. Record the prop's new fact (or drop it when the rvalue is not representable
    // / is an object handle). Mark the readonly write for the reassign check.
    match prop_fact_val {
        Some(fact) if !rval_is_object => {
            obj.props.insert(prop.to_owned(), PropFact { fact, stratum: rvalue_strat });
        }
        _ => {
            obj.props.remove(prop);
        }
    }
    if is_readonly {
        obj.ro_written.insert(prop.to_owned());
    }
}

/// The simple (last-segment) class name of an FQN, for a diagnostic message.
fn simple_class(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// Parse the first `@var` tag's type out of a property docblock (ADR-0036), or
/// `None` when absent/unparseable — the property carries no phpdoc contract.
fn parse_var_type(docblock: &str) -> Option<PType> {
    for tag in scan_docblock(docblock) {
        if matches!(tag.kind, TagKind::Var) {
            return parse_tag_type(&tag.type_text);
        }
    }
    None
}

/// Build a [`ClosureVal`] from a lowered [`ClosureRef`] at its creation site,
/// snapshotting the by-value captures from the definition-site `env` (ADR-0033).
/// A capture whose variable has no proven scalar fact is simply omitted (the
/// closure body sees it as unknown — sound); a captured closure is not re-snapshot
/// (nested closure capture is not modeled — the body treats it as unknown).
fn build_closure_val(
    cx: &Cx,
    cref: &steins_syntax::ClosureRef,
    line: u32,
    env: &HashMap<String, Known>,
) -> Option<ClosureVal> {
    use steins_syntax::ClosureRef;
    match cref {
        ClosureRef::Anonymous { def_offset, captures } => {
            let mut snapshot: Vec<(String, Fact)> = Vec::new();
            for name in captures {
                if let Some(k) = env.get(name)
                    && let Some(f) = &k.fact
                {
                    snapshot.push((name.clone(), f.clone()));
                }
            }
            Some(ClosureVal { target: ClosureTarget::Scope(*def_offset), captures: snapshot, def_line: line })
        }
        ClosureRef::FunctionName(nameref) => {
            let _ = cx;
            Some(ClosureVal { target: ClosureTarget::Named(nameref.clone()), captures: Vec::new(), def_line: line })
        }
    }
}

/// Walk a structured `if`/`elseif`/`else` (ADR-0031 stage 1). Evaluates the guard
/// to a [`Certainty`], walks each **live** branch on a cloned env (applying
/// positive refinement), then joins the envs of the branches that fall through.
/// When no live branch falls through, the code after the `if` is unreachable and
/// the whole construct terminates.
#[allow(clippy::too_many_arguments)]
fn walk_if(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_trace: &[Stmt],
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let poisoned = w.scope.poisoned;
    // 1. Evaluate the guard in the pre-branch env (short-circuit env refinement is
    // stage 2 — each condition sees the same entry env).
    let verdict = eval_cond(w, folder, cond, env, store, poisoned);

    // A decided guard proves the skipped side dead — record it so the env-free
    // direct pass never reports inside it (live-path discipline, ADR-0002/0031).
    match verdict {
        Certainty::Yes => {
            for (_, trace) in elseifs {
                mark_dead(w, &[trace.as_slice()]);
            }
            if let Some(trace) = else_trace {
                mark_dead(w, &[trace]);
            }
        }
        Certainty::No => mark_dead(w, &[then_trace]),
        Certainty::Maybe => {}
    }

    // 2. The guard's own effects on *every* resulting path, sequenced at the calls'
    // positions (ADR-0052 §6, replacing the old blanket `cond_invalidations`): a
    // retained guard call escapes its object arguments/receiver and sweeps the
    // escaped objects' mutable props (the method receiver's binding survives — a
    // method call does not rebind its receiver variable, the payoff (i) that lets
    // `$x !== null && $x->m()` keep a proven-non-null receiver), then by-ref argument
    // invalidation and opaque reads are forgotten. Both apply before the branch
    // clones (a call in either operand may have executed on the excluded path too).
    let guard_calls: Vec<&CallExpr> = collect_guard_calls_any(cond);
    escape_and_sweep_calls(w, &guard_calls, store);
    for v in cond_invalidations(cond) {
        env.remove(&v);
        store.unbind(&v);
    }

    // 3. Walk the live branches on cloned envs, collecting those that fall through.
    let mut fell: Vec<(HashMap<String, Known>, Store)> = Vec::new();

    // Guard calls carrying `-if-true`/`-if-false` envelopes, collected per branch
    // polarity through the `&&`/`||` structure (ADR-0052 §6, extending N2's
    // top-level-only consumption into nested positions: `if ($a && isNonEmpty($s))`
    // now consumes `isNonEmpty`'s `-if-true` on the then-branch). Each carries whether
    // the call returned `true` on that branch, selecting the spec polarity. The specs
    // apply at the `Asserted` stratum (§5) — silence only, never a proof premise.
    if verdict != Certainty::No {
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        apply_refinements(&then_refinements(cond), &mut benv, &mut bclasses, Stratum::Verified);
        apply_class_narrowing(w, cond, true, &mut bclasses);
        let mut then_calls = Vec::new();
        collect_guard_calls(cond, true, &mut then_calls);
        for (call, returns_true) in then_calls {
            let kind = if returns_true { AssertKind::IfTrue } else { AssertKind::IfFalse };
            apply_guard_asserts(w, call, kind, &mut benv, &mut bclasses);
            // Guard-respect leg (ADR-0049 §4): a positive existence guard vouches its
            // symbol on the branch where it holds true, silencing the absence family.
            if returns_true && let Some(v) = existence_vouch(w.cx, &bclasses, call) {
                bclasses.vouch(v);
            }
        }
        if walk_trace(w, folder, then_trace, &mut benv, &mut bclasses, descent, facts, true, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    if verdict != Certainty::Yes {
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        apply_refinements(&else_refinements(cond), &mut benv, &mut bclasses, Stratum::Verified);
        apply_class_narrowing(w, cond, false, &mut bclasses);
        let mut else_calls = Vec::new();
        collect_guard_calls(cond, false, &mut else_calls);
        for (call, returns_true) in else_calls {
            let kind = if returns_true { AssertKind::IfTrue } else { AssertKind::IfFalse };
            apply_guard_asserts(w, call, kind, &mut benv, &mut bclasses);
            // Guard-respect leg (ADR-0049 §4): the negated-guard branch where the
            // predicate holds true (`if (!method_exists(...)) {} else <here>`) vouches too.
            if returns_true && let Some(v) = existence_vouch(w.cx, &bclasses, call) {
                bclasses.vouch(v);
            }
        }
        if walk_else(w, folder, elseifs, else_trace, &mut benv, &mut bclasses, descent, facts, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    // 4. Merge. No live fall-through → the successor is unreachable.
    if fell.is_empty() {
        return Flow::Terminated;
    }
    let (jenv, jclasses) = join_envs(fell);
    *env = jenv;
    *store = jclasses;
    Flow::FellThrough
}

/// Walk the `else` side of an `if`: the `elseif` chain desugars to a nested
/// `if`/`else`; the terminal `else` (if any) is a plain sub-trace; an absent
/// `else` falls through unchanged (the negated-guard path).
#[allow(clippy::too_many_arguments)]
fn walk_else(
    w: &WalkCx,
    folder: &mut dyn Folder,
    elseifs: &[(CondExpr, Vec<Stmt>)],
    else_trace: Option<&[Stmt]>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    match elseifs.split_first() {
        Some(((cond, trace), rest)) => {
            walk_if(w, folder, cond, trace, rest, else_trace, env, store, descent, facts, out)
        }
        None => match else_trace {
            Some(stmts) => walk_trace(w, folder, stmts, env, store, descent, facts, true, out),
            None => Flow::FellThrough,
        },
    }
}

/// Walk a structured statement-position `match`/`switch` (ADR-0031 Part B).
///
/// Per arm, the "taken" certainty is computed left to right with first-match
/// semantics: `taken(k) = Yes` iff arm `k` matches (`Yes`) **and** every earlier
/// arm provably does not (`No`); `No` iff arm `k` provably does not match; `Maybe`
/// otherwise. This ordering rule is what stops a later `Yes` arm from being walked
/// as the sole-live branch while an earlier arm is only `Maybe`. Arms with
/// `taken == No` are recorded dead (the env-free direct pass then stays silent
/// inside them); every other arm is walked on a cloned env, with the subject
/// var refined to the arm's literal set (a `match` binds `Singleton`/`OneOf`; a
/// `switch` binds nothing — its loose `==` truth set is multi-valued).
///
/// The "no arm matched" outcome depends on the construct: with a `default` arm it
/// runs that body (unless a decided `Yes` arm makes it dead); without one, a
/// `switch` falls through to after the construct (entry env preserved) while a
/// `match` raises `\UnhandledMatchError` — a terminator contributing no
/// fall-through. The successor env is the join of every branch that falls
/// through; if none does, the whole construct terminates (tail unreachable).
#[allow(clippy::too_many_arguments)]
fn walk_match(
    w: &WalkCx,
    folder: &mut dyn Folder,
    subject: &CondOperand,
    arms: &[MatchArmT],
    default: Option<&[Stmt]>,
    loose: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let poisoned = w.scope.poisoned;
    let op = if loose { CmpOp::Loose } else { CmpOp::Identical };
    let subj_vals = operand_values(subject, env, poisoned);

    // 1. Per-arm first-match "taken" certainty (left to right). `earlier_all_no`
    // tracks whether every arm before the current one provably does NOT match;
    // `decided_done` records that a *decided* match (`Yes` with all earlier `No`)
    // has been found — every later arm and the default are then unreachable,
    // because `match`/`switch` take the FIRST matching arm.
    let mut takens: Vec<Certainty> = Vec::with_capacity(arms.len());
    let mut earlier_all_no = true;
    let mut decided_done = false;
    for arm in arms {
        if decided_done {
            takens.push(Certainty::No); // a prior sure match makes this unreachable
            continue;
        }
        let cond_k = eval_arm_cond(op, subj_vals.as_deref(), &arm.conditions, env, poisoned);
        let taken = match cond_k {
            Certainty::No => Certainty::No,
            Certainty::Yes if earlier_all_no => {
                decided_done = true;
                Certainty::Yes
            }
            _ => Certainty::Maybe,
        };
        if cond_k != Certainty::No {
            earlier_all_no = false;
        }
        takens.push(taken);
    }
    // The default / no-match path: `No` once a decided arm consumed the value;
    // `Yes` when every arm provably fails to match; else `Maybe`.
    let no_match_taken = if decided_done {
        Certainty::No
    } else if earlier_all_no {
        Certainty::Yes
    } else {
        Certainty::Maybe
    };

    // 2. Walk each live arm on a cloned env; record `No` arms dead.
    let mut fell: Vec<(HashMap<String, Known>, Store)> = Vec::new();
    for (arm, taken) in arms.iter().zip(&takens) {
        if *taken == Certainty::No {
            mark_dead(w, &[arm.trace.as_slice()]);
            continue;
        }
        let mut benv = env.clone();
        let mut bclasses = store.clone();
        refine_match_arm(subject, &arm.conditions, loose, &mut benv);
        if walk_trace(w, folder, &arm.trace, &mut benv, &mut bclasses, descent, facts, true, out)
            == Flow::FellThrough
        {
            fell.push((benv, bclasses));
        }
    }

    // 3. The "no arm matched" outcome.
    match default {
        Some(dtrace) => {
            if no_match_taken == Certainty::No {
                mark_dead(w, &[dtrace]);
            } else {
                let mut benv = env.clone();
                let mut bclasses = store.clone();
                if walk_trace(w, folder, dtrace, &mut benv, &mut bclasses, descent, facts, true, out)
                    == Flow::FellThrough
                {
                    fell.push((benv, bclasses));
                }
            }
        }
        None => {
            // A default-less `switch` falls through to after itself on no match
            // (entry env unchanged); a default-less `match` throws
            // `\UnhandledMatchError` — a terminator that joins nothing.
            if loose && no_match_taken != Certainty::No {
                fell.push((env.clone(), store.clone()));
            }
        }
    }

    // 4. Merge. No live fall-through → the successor is unreachable.
    if fell.is_empty() {
        return Flow::Terminated;
    }
    let (jenv, jclasses) = join_envs(fell);
    *env = jenv;
    *store = jclasses;
    Flow::FellThrough
}

/// The certainty that a `match`/`switch` arm is the one taken *by value* — i.e.
/// the subject equals ANY of the arm's conditions (`===` for match, loose `==`
/// for switch). An unknown subject or condition contributes `Maybe`; the OR folds
/// the per-condition verdicts (any `Yes` → `Yes`, all `No` → `No`, else `Maybe`).
fn eval_arm_cond(
    op: CmpOp,
    subj_vals: Option<&[ArgValue]>,
    conditions: &[CondOperand],
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Certainty {
    let Some(subj) = subj_vals else { return Certainty::Maybe };
    let mut acc = Certainty::No;
    for c in conditions {
        let cert = match operand_values(c, env, poisoned) {
            Some(cv) => eval_cmp(op, subj, &cv),
            None => Certainty::Maybe,
        };
        acc = acc.or(cert);
        if acc == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    acc
}

/// Refine the subject variable inside a matched arm's cloned env. A `match`
/// (strict `===`) whose subject is a bare variable and whose conditions are all
/// literals binds the subject to that exact finite set (`Singleton` for one,
/// `OneOf` for several) — the value is provably one of them on this path. A
/// `switch` (loose `==`) binds NOTHING: a loose-equal truth set is multi-valued
/// (`case 0` matches `0`, `"0"`, `false`, `0.0`, …), so no single `Fact` is sound.
fn refine_match_arm(
    subject: &CondOperand,
    conditions: &[CondOperand],
    loose: bool,
    env: &mut HashMap<String, Known>,
) {
    if loose {
        return;
    }
    let CondOperand::Var(name) = subject else { return };
    let mut vals = Vec::with_capacity(conditions.len());
    for c in conditions {
        match c {
            CondOperand::Literal(v) => match val_of(v) {
                Some(val) => vals.push(val),
                None => return,
            },
            _ => return,
        }
    }
    if let Some(fact) = Fact::from_vals(vals) {
        let line = env.get(name).map_or(0, |k| k.line);
        env.insert(name.clone(), Known::value(fact, line, Some("matched arm".to_owned())));
    }
}

/// The branch-sensitive null-dereference proof (ADR-0031, `call.on-null`): a
/// non-null-safe `$v->m(...)` whose receiver `$v` is proven `Singleton(null)` on
/// the current path is a guaranteed runtime `Error`. A `OneOf` that merely
/// includes null stays `Maybe` (silent), and `?->` never fires.
fn check_call_on_null(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    if w.scope.poisoned {
        return;
    }
    let Callee::Method { receiver: Receiver::Var(v), method, nullsafe: false } = &call.receiver
    else {
        return;
    };
    let Some(k) = env.get(v) else { return };
    if !matches!(&k.fact, Some(Fact::Singleton(Val::Null))) {
        return;
    }
    // Proof-layer consumption rule (ADR-0052 §5): a receiver proven null only by an
    // `Asserted` fact (e.g. `@phpstan-assert null $x`) cannot premise this proof —
    // stay silent.
    if k.stratum != Stratum::Verified {
        return;
    }
    let pos = w.cx.tree().position(call.span.start);
    out.push(Diagnostic {
        id: CALL_ON_NULL_ID,
        facet: None,
        path: w.cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "method call ${v}->{method}() — ${v} is proven null on this path — proven Error (Call to a member function on null)"
        ),
    });
}

// ---------------------------------------------------------------------------
// Condition evaluation → `Certainty` (ADR-0031 stage 1).
// ---------------------------------------------------------------------------

/// Evaluate a lowered [`CondExpr`] against the env to a unified [`Certainty`].
///
/// `folder` is threaded because a foldable existence-guard call
/// (`method_exists`/`function_exists`/`class_exists` …, ADR-0049 §4 / N3) folds to
/// a real verdict by asking the runtime boot surface (the A2ii homonym oracle);
/// every other arm is env-only and ignores it.
fn eval_cond(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Certainty {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            match (operand_values(lhs, env, poisoned), operand_values(rhs, env, poisoned)) {
                (Some(lv), Some(rv)) => eval_cmp(*op, &lv, &rv),
                _ => Certainty::Maybe,
            }
        }
        CondExpr::Truthy(op) => match operand_values(op, env, poisoned) {
            Some(vs) => all_agree(vs.iter().map(php_truthy)),
            None => Certainty::Maybe,
        },
        CondExpr::Instanceof { operand, class_ref } => {
            eval_instanceof(w, operand, class_ref, env, store, poisoned)
        }
        CondExpr::Not(c) => eval_cond(w, folder, c, env, store, poisoned).not(),
        // Short-circuit env threading (ADR-0052 §6 / N3): the RIGHT operand
        // evaluates under the env the LEFT operand's outcome establishes, exactly as
        // PHP's `&&`/`||` sequence it — `b` in `a && b` runs only when `a` was truthy
        // (so it sees `then_refinements(a)`); `b` in `a || b` runs only when `a` was
        // falsy (so it sees `else_refinements(a)`, De Morgan). The composed verdict
        // stays the trinary `and`/`or`; only the operand env threads. This is
        // walk-local left-to-right evaluation (ADR-0048 §2): the refinement clone is
        // discarded, no entry state contributes, no ordering beyond the source's own.
        CondExpr::And(a, b) => {
            let va = eval_cond(w, folder, a, env, store, poisoned);
            // `a` false ⇒ `b` never runs; the verdict is already `No`, and the
            // threaded env would be a contradiction — skip it.
            if va == Certainty::No {
                return Certainty::No;
            }
            let (benv, bstore) = threaded_operand_env(a, true, env, store);
            va.and(eval_cond(w, folder, b, &benv, &bstore, poisoned))
        }
        CondExpr::Or(a, b) => {
            let va = eval_cond(w, folder, a, env, store, poisoned);
            // `a` true ⇒ `b` never runs; the verdict is already `Yes`.
            if va == Certainty::Yes {
                return Certainty::Yes;
            }
            let (benv, bstore) = threaded_operand_env(a, false, env, store);
            va.or(eval_cond(w, folder, b, &benv, &bstore, poisoned))
        }
        // A foldable existence predicate in guard position folds to a Yes/No/Maybe
        // verdict against the closed world (ADR-0049 §4 / N3); an opaque condition or
        // any other guard call stays undecided.
        CondExpr::Call { call, .. } => eval_existence_call(w, folder, call),
        CondExpr::Opaque { .. } => Certainty::Maybe,
    }
}

// ---------------------------------------------------------------------------
// Foldable existence-guard verdicts (ADR-0049 §4 / N3).
//
// `method_exists(C, 'm')` / `function_exists('f')` / `class_exists('N')` (and the
// `interface_`/`trait_`/`enum_exists` siblings) in guard position fold to a
// three-valued `Certainty` against the closed world, so the ADR-0031 dead-region
// discipline prunes the branch the runtime provably never takes. The verdict rests
// on the SAME closure the absence family fires under (S1 existence + S2 chain
// enumeration + the A2ii boot-surface homonym oracle + A2i conditional/dam leg):
//   * `Yes`  — the symbol is provably PRESENT under complete closure;
//   * `No`   — provably ABSENT under complete closure;
//   * `Maybe`— anything short of closure (a trait-bearing chain, a conditional decl
//              with the dam standing, an unresolvable ancestor, an unanswerable
//              homonym query, a non-literal argument, or no live boot surface).
// A `Maybe` verdict is always the FP-safe fallback: it walks both branches live and
// leans on the conservative guard-respect leg (the per-symbol vouch) for silence.
// ---------------------------------------------------------------------------

/// The recognized existence predicate a guard call names, or `None` when the call
/// is not one of them / is a namespaced or userland-shadowed twin (a `Foo\class_exists`
/// or a same-named user function is a DIFFERENT function, never the global builtin).
fn existence_predicate(cx: &Cx, call: &CallExpr) -> Option<&'static str> {
    let callee = call.callee.as_deref()?;
    let r = call.callee_ref.as_ref()?;
    // A qualified name (`Foo\method_exists`) is a different function; the raw has its
    // leading `\` stripped, so any remaining backslash means a namespace prefix.
    if r.raw.contains('\\') {
        return None;
    }
    // A userland function of the same (unqualified) name shadows the builtin.
    if matches!(cx.resolve_function(r), FnResolution::User(_)) {
        return None;
    }
    const PREDS: &[&str] = &[
        "method_exists",
        "function_exists",
        "class_exists",
        "interface_exists",
        "trait_exists",
        "enum_exists",
    ];
    PREDS.iter().copied().find(|p| callee.eq_ignore_ascii_case(p))
}

/// Fold a recognized existence-guard call to a verdict (the N3 machinery). Anything
/// unrecognized or short of closure is `Maybe`.
fn eval_existence_call(w: &WalkCx, folder: &mut dyn Folder, call: &CallExpr) -> Certainty {
    let Some(pred) = existence_predicate(w.cx, call) else {
        return Certainty::Maybe;
    };
    // A2ii/A9: without a live boot surface (or with a runtime-redefinition extension
    // loaded), neither presence nor absence is decidable — the sound subset is Maybe.
    if !folder.absence_family_available() {
        return Certainty::Maybe;
    }
    if pred == "method_exists" {
        // `method_exists(class, 'name')` — two positional literal arguments.
        if !call.positional_only || call.args.len() != 2 {
            return Certainty::Maybe;
        }
        let Some(class_fqn) = existence_class_literal(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        let ArgValue::Str(method) = &call.args[1].value else {
            return Certainty::Maybe;
        };
        method_exists_verdict(w.cx, folder, &class_fqn, method)
    } else if pred == "function_exists" {
        if !call.positional_only || call.args.len() != 1 {
            return Certainty::Maybe;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return Certainty::Maybe;
        };
        function_exists_verdict(w.cx, folder, name)
    } else {
        // `class_exists`/`interface_exists`/`trait_exists`/`enum_exists('Name')`.
        if !call.positional_only || call.args.is_empty() {
            return Certainty::Maybe;
        }
        let Some(name) = existence_class_literal(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        classlike_exists_verdict(w.cx, folder, pred, &name)
    }
}

/// Resolve a *literal* class reference in an existence-predicate argument to an FQN:
/// the `C::class` magic constant (resolved in the call site's namespace context) or
/// a string class name (which PHP treats as fully qualified). A `$var` receiver or
/// any other form is `None` — the verdict then stays `Maybe`, and the conservative
/// guard-respect leg (which CAN read the store) carries the silence for a proven-class
/// variable.
fn existence_class_literal(cx: &Cx, v: &ArgValue) -> Option<String> {
    match v {
        ArgValue::ClassConst(StaticClass::Named(r), name) if name.eq_ignore_ascii_case("class") => {
            Some(cx.class_fqn(r))
        }
        ArgValue::Str(s) => Some(s.trim_start_matches('\\').to_owned()),
        _ => None,
    }
}

/// The three-valued `method_exists(start_fqn, method)` verdict: walk `start_fqn`'s
/// class chain under the S2 closure discipline (ADR-0049 §4). Unlike the absence
/// flagship this ignores `__call`/`__callStatic` — `method_exists` reports only
/// DECLARED methods, magic fallbacks do not make it true. An abstract or any-visibility
/// declaration counts as present (`method_exists` is visibility-blind). Any obstacle
/// to closure (a trait-bearing/enum node, an unresolvable ancestor, a cycle, a
/// conditional node with the dam standing, or an unanswerable/positive boot-surface
/// homonym on any traversed FQN) collapses to `Maybe`.
fn method_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    start_fqn: &str,
    method: &str,
) -> Certainty {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fqns: Vec<String> = Vec::new();
    let mut any_conditional = false;
    let present;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Certainty::Maybe; // cycle — closure cannot terminate soundly.
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Certainty::Maybe; // ancestor leaves the project / ambiguous.
        };
        // Enum methods are not lowered; a trait/`uses_traits` node could carry the
        // method invisibly to this walk — either way, closure is unproven.
        if cd.is_enum || cd.is_trait || cd.uses_traits {
            return Certainty::Maybe;
        }
        fqns.push(cur.clone());
        if cd.conditional {
            any_conditional = true;
        }
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method)) {
            present = true;
            break;
        }
        match &cd.parent {
            None => {
                present = false;
                break;
            }
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
    // A2i: a conditional declaration on the chain re-dams the claim — only the clear
    // whole-universe dam lets either verdict stand.
    if any_conditional && !cx.dam.is_clear() {
        return Certainty::Maybe;
    }
    // A2ii: every traversed FQN must be boot-surface homonym-clear, else the runtime
    // class differs from the textual one and neither presence nor absence is decidable.
    for fqn in &fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return Certainty::Maybe,
        }
    }
    if present { Certainty::Yes } else { Certainty::No }
}

/// The three-valued `function_exists('name')` verdict (ADR-0049 §6 / S1 existence).
/// A catalog builtin is always present; a uniquely-indexed unconditional userland
/// function is present; an absent name that the boot surface answers NOT-a-function
/// is provably absent. A conditional declaration (dam standing), an ambiguous name,
/// or an unanswerable homonym is `Maybe`.
fn function_exists_verdict(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Certainty {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    // A catalogued builtin is a resident function (`strlen`, `array_map`, …).
    if steins_catalog::effect_labels(&lname).is_some() {
        return Certainty::Yes;
    }
    match cx.index.resolve_function(&lname) {
        Res::Unique(site) => {
            if cx.fn_decl(site).conditional && !cx.dam.is_clear() {
                Certainty::Maybe // a conditional polyfill with the dam standing.
            } else {
                Certainty::Yes
            }
        }
        Res::Ambiguous => Certainty::Maybe,
        Res::Absent => match folder.boot_surface_function(&lname) {
            Some(true) => Certainty::Yes,  // a resident extension function.
            Some(false) => Certainty::No,  // provably absent everywhere.
            None => Certainty::Maybe,
        },
    }
}

/// The three-valued `class_exists`/`interface_exists`/`trait_exists`/`enum_exists`
/// verdict (ADR-0049 §4 / S1 existence). A uniquely-indexed unconditional project
/// class-like of the MATCHING kind is present; an absent name the boot surface reports
/// as resident is present; an absent name the boot surface reports NOT-resident is
/// provably absent. A conditional decl (dam standing), an ambiguous name, a kind
/// mismatch (`class_exists` on an interface), or an unanswerable homonym is `Maybe`.
fn classlike_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    pred: &str,
    name: &str,
) -> Certainty {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    match cx.index.resolve_class(&lname) {
        Res::Unique(site) => {
            let (_, cd) = cx.class_decl(site);
            if cd.conditional && !cx.dam.is_clear() {
                return Certainty::Maybe;
            }
            // The predicate queries one specific kind; `enum` satisfies both
            // `enum_exists` and `class_exists` (a PHP enum is a class), while a plain
            // interface/trait never satisfies `class_exists`. A mismatch cannot be
            // proven true here (the name may still resolve to a boot-surface homonym
            // of the right kind), so it stays `Maybe`.
            if classlike_kind_matches(pred, cd) {
                Certainty::Yes
            } else {
                Certainty::Maybe
            }
        }
        Res::Ambiguous => Certainty::Maybe,
        Res::Absent => match folder.boot_surface_class_like(&lname) {
            Some(true) => Certainty::Yes,
            Some(false) => Certainty::No,
            None => Certainty::Maybe,
        },
    }
}

/// Whether a resolved class-like declaration satisfies the given existence predicate:
/// `class_exists` accepts a class or enum (never a bare interface/trait);
/// `interface_exists`/`trait_exists`/`enum_exists` each accept only their own kind.
fn classlike_kind_matches(pred: &str, cd: &ClassDecl) -> bool {
    match pred {
        "class_exists" => !cd.is_interface && !cd.is_trait,
        "interface_exists" => cd.is_interface,
        "trait_exists" => cd.is_trait,
        "enum_exists" => cd.is_enum,
        _ => false,
    }
}

/// The symbol a positive existence guard call vouches for (ADR-0049 §4 guard-respect
/// leg), resolved against the branch store. `None` when the call is not a recognized
/// existence predicate or its subject cannot be pinned to a concrete symbol.
/// `method_exists` additionally resolves a `$var` receiver to its store-known class
/// (the literal `C::class`/string forms go through [`existence_class_literal`]), so
/// the instance idiom `if (method_exists($o,'m')) { $o->m(); }` vouches `C::m` — the
/// exact-textual-match discipline: the vouch key is the RESOLVED class + name.
fn existence_vouch(cx: &Cx, store: &Store, call: &CallExpr) -> Option<Vouch> {
    let pred = existence_predicate(cx, call)?;
    if pred == "method_exists" {
        if !call.positional_only || call.args.len() != 2 {
            return None;
        }
        let ArgValue::Str(method) = &call.args[1].value else {
            return None;
        };
        let class = match &call.args[0].value {
            ArgValue::Var(v) => store.class_of(v)?.to_owned(),
            other => existence_class_literal(cx, other)?,
        };
        Some(Vouch::Method {
            class: class.trim_start_matches('\\').to_ascii_lowercase(),
            method: method.to_ascii_lowercase(),
        })
    } else if pred == "function_exists" {
        if !call.positional_only || call.args.len() != 1 {
            return None;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return None;
        };
        Some(Vouch::Function(name.trim_start_matches('\\').to_ascii_lowercase()))
    } else {
        if !call.positional_only || call.args.is_empty() {
            return None;
        }
        let name = existence_class_literal(cx, &call.args[0].value)?;
        Some(Vouch::Class(name.trim_start_matches('\\').to_ascii_lowercase()))
    }
}

/// A clone of `(env, store)` with the refinements `operand` establishes on the
/// given branch polarity applied (ADR-0052 §6 short-circuit threading). Used only
/// to evaluate the *right* operand of an `&&`/`||` at the precision the left
/// operand's runtime outcome guarantees. Native-condition refinements are
/// `Verified` (the runtime executed the test); the clone is discarded after the
/// verdict, so nothing leaks into the caller's env (ADR-0048 §2 walk-locality).
fn threaded_operand_env(
    operand: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    store: &Store,
) -> (HashMap<String, Known>, Store) {
    let mut benv = env.clone();
    let mut bstore = store.clone();
    let mut refs = Vec::new();
    collect_refine(operand, then, &mut refs);
    apply_refinements(&refs, &mut benv, &mut bstore, Stratum::Verified);
    // The operand's own side effects land *after* its test narrowed (a by-ref call
    // in the operand may rebind a variable the test just constrained): forget them
    // so the right operand's verdict reads the post-`operand` env, not a stale one.
    for v in cond_invalidations(operand) {
        benv.remove(&v);
        bstore.unbind(&v);
    }
    (benv, bstore)
}

/// Evaluate a ternary rvalue to an env [`Fact`] (ADR-0031): a decided guard picks
/// the chosen arm's proven value; an undecided guard yields a `OneOf` of the two
/// arms when both resolve to literals, else `None` (unknown → the var is dropped).
fn eval_ternary_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_val: &ArgValue,
    else_val: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<Fact> {
    let poisoned = w.scope.poisoned;
    let verdict = eval_cond(w, folder, cond, env, store, poisoned);
    // The arms evaluate under the guard's respective refinements (ADR-0052 §6):
    // `$c ? A : B` — `A` runs only when `$c` was truthy (so it sees
    // `then_refinements($c)`), `B` only when `$c` was falsy (`else_refinements`).
    // The arm-selection verdict logic is unchanged; only the arm *envs* thread.
    let (tenv, _) = threaded_operand_env(cond, true, env, store);
    let (eenv, _) = threaded_operand_env(cond, false, env, store);
    match verdict {
        Certainty::Yes => {
            w.cx.resolve_literal(then_val, &tenv, poisoned, folder).and_then(|a| singleton_fact(&a))
        }
        Certainty::No => {
            w.cx.resolve_literal(else_val, &eenv, poisoned, folder).and_then(|a| singleton_fact(&a))
        }
        Certainty::Maybe => {
            // Undecided guard: the value is one of the two arms. `Fact::from_vals`
            // gives the canonical finite form (a `Singleton` when the arms are
            // equal, else a `OneOf`), or `None` (dropped) when an arm is not
            // representable.
            let t = val_of(&w.cx.resolve_literal(then_val, &tenv, poisoned, folder)?)?;
            let e = val_of(&w.cx.resolve_literal(else_val, &eenv, poisoned, folder)?)?;
            Fact::from_vals(vec![t, e])
        }
    }
}

/// The candidate values of a condition operand: the fact's value set for a known
/// variable, the literal itself, else `None` (unknown → the caller yields `Maybe`).
fn operand_values(
    op: &CondOperand,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<ArgValue>> {
    match op {
        CondOperand::Literal(v) => Some(vec![v.clone()]),
        // Only the finite layers (`Singleton`/`OneOf`) offer concrete candidate
        // values for a comparison; an abstract fact has none → `None` → `Maybe`
        // (the sound side). Condition evaluation over `finite_members()`.
        CondOperand::Var(name) if !poisoned => {
            env.get(name).and_then(|k| k.fact.as_ref()?.finite_members().map(|vs| vs.iter().map(arg_of_val).collect()))
        }
        _ => None,
    }
}

/// Evaluate a comparison over two candidate value sets (ADR-0031 OneOf rule: all
/// member pairs agree → that verdict; any disagreement or undecidable pair → Maybe).
fn eval_cmp(op: CmpOp, lhs: &[ArgValue], rhs: &[ArgValue]) -> Certainty {
    let mut acc: Option<bool> = None;
    for l in lhs {
        for r in rhs {
            let b = match op {
                CmpOp::Identical => php_identical(l, r),
                CmpOp::NotIdentical => php_identical(l, r).map(|x| !x),
                CmpOp::Loose => php_loose_eq(l, r),
                CmpOp::NotLoose => php_loose_eq(l, r).map(|x| !x),
                // Ordering: decide only for concrete numeric operands (PHP numeric
                // ordering); any other pairing is undecidable here → `Maybe`. The
                // refinement machinery consumes these guards regardless of verdict.
                CmpOp::Lt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Less),
                CmpOp::Le => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Greater),
                CmpOp::Gt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Greater),
                CmpOp::Ge => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Less),
            };
            match b {
                None => return Certainty::Maybe,
                Some(v) => match acc {
                    None => acc = Some(v),
                    Some(prev) if prev != v => return Certainty::Maybe,
                    _ => {}
                },
            }
        }
    }
    Certainty::from_opt(acc)
}

/// PHP numeric ordering of two concrete operands, decided only when **both** are
/// `int`/`float` (comparing as f64); any other pairing (strings, bools, null,
/// arrays) is `None` — undecidable here, so the guard verdict is `Maybe` (sound).
fn php_num_order(a: &ArgValue, b: &ArgValue) -> Option<std::cmp::Ordering> {
    let num = |v: &ArgValue| match v {
        #[allow(clippy::cast_precision_loss)]
        ArgValue::Int(i) => Some(*i as f64),
        ArgValue::Float(f) => Some(*f),
        _ => None,
    };
    let (x, y) = (num(a)?, num(b)?);
    x.partial_cmp(&y)
}

/// Fold a sequence of per-member truth verdicts (`None` = undecidable) into one
/// [`Certainty`]: all-agree → that pole, else `Maybe`.
fn all_agree(iter: impl Iterator<Item = Option<bool>>) -> Certainty {
    let mut acc: Option<bool> = None;
    for b in iter {
        match b {
            None => return Certainty::Maybe,
            Some(v) => match acc {
                None => acc = Some(v),
                Some(prev) if prev != v => return Certainty::Maybe,
                _ => {}
            },
        }
    }
    Certainty::from_opt(acc)
}

/// `operand instanceof Class`: `Yes` only when the operand's proven exact class
/// is-a the target through the project chain; a non-object literal is `No`;
/// everything else (unknown class, chain leaving the project) is `Maybe`.
fn eval_instanceof(
    w: &WalkCx,
    operand: &CondOperand,
    class_ref: &NameRef,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Certainty {
    match operand {
        CondOperand::Var(name) if !poisoned => {
            let target = w.cx.class_fqn(class_ref);
            match store.class_of(name) {
                Some(obj_fqn) => {
                    // The trinary is-a oracle (ADR-0043): a proven supertype path is a
                    // definite `Yes`; a completely-enumerated hierarchy that excludes
                    // the target is a definite `No` (the branch is dead); an incomplete
                    // hierarchy stays `Maybe` (the FP-safe side). Instanceof still binds
                    // no exactness fact — membership is not exactness (see below).
                    match w.cx.is_a(obj_fqn, &target) {
                        // Yes-side holds for a lower bound too (every descendant of the
                        // proven class is still a `T`) — keep the branch-death precision.
                        IsA::Yes => Certainty::Yes,
                        // No-side needs exactness (audit G1): with a lower-bound `$this`,
                        // the runtime object may be a descendant that IS a `T`, so a
                        // `No` here is not decisive — the then-branch is not dead.
                        IsA::No if store.is_exact(name) => Certainty::No,
                        IsA::No | IsA::Unknown => Certainty::Maybe,
                    }
                }
                // No heap object. First the VALUE side (survey FP class 14): if the
                // variable's value-domain fact proves it holds a *non-object* value on
                // this path (`null`/int/float/string/bool/array — e.g. a call-site
                // `null` imported as `Singleton(null)` by a binding descent), then
                // `instanceof T` is definitionally `false` for every `T` — `null` and
                // every scalar are instances of nothing. This is a decisive `No` that
                // needs NO class reasoning and NO exactness: the G1 exactness discipline
                // (`store.is_exact`) is scoped to *object-class* No verdicts on the heap
                // path above and is untouched here. Sound unconditionally (silence: the
                // then-branch is dead — no PHP value that is not an object is an instance).
                None if env.get(name).and_then(|k| k.fact.as_ref()).is_some_and(fact_is_non_object) => {
                    Certainty::No
                }
                // Otherwise a prior `instanceof` guard may have bound a `Member` fact
                // whose is-a implication decides this test (ADR-0052 §3b, consumer (b)).
                // A11 does NOT thread here: it is scoped to the arm-deletion consumers,
                // and this implication is a separate one.
                None => member_instanceof(w.cx, store.member_of(name), &target),
            }
        }
        // A concrete non-object literal (`null`, `5`, `"x"`, …) is never an
        // instance of a class.
        CondOperand::Literal(v) if v.is_literal() => Certainty::No,
        _ => Certainty::Maybe,
    }
}

/// The `instanceof T2` verdict implied by a variable's guard-derived [`Member`]
/// fact (ADR-0052 §3b), when no exact heap class is known:
///
/// - **`Yes`** — some proven `T1 ∈ yes` has `is_a(T1, T2) = Yes`: the value is
///   already a `T1`, and every `T1` is a `T2`.
/// - **`No`** — some excluded `T1' ∈ no` has `is_a(T2, T1') = Yes`: a `T2` instance
///   would be a `T1'`, which the guard proved the value is not.
/// - **`Maybe`** — otherwise (no fact, or the hierarchy does not decide).
///
/// Sound in both directions and monotone: it only turns `Maybe` into a decided
/// verdict (branch-death → *silence*), never emits.
fn member_instanceof(cx: &Cx, member: Option<&Member>, target: &str) -> Certainty {
    let Some(m) = member else { return Certainty::Maybe };
    if m.yes.iter().any(|t1| cx.is_a(t1, target) == IsA::Yes) {
        return Certainty::Yes;
    }
    if m.no.iter().any(|excluded| cx.is_a(target, excluded) == IsA::Yes) {
        return Certainty::No;
    }
    Certainty::Maybe
}

/// Whether a value-domain [`Fact`] proves the variable holds a **non-object**
/// value on this path (survey FP class 14 — the value-side `instanceof` rule).
/// Every inhabitant of the fact must be a non-object PHP value; then `instanceof
/// T` is `false` for every `T` (`null`, ints, floats, strings, bools and arrays
/// are instances of nothing). All four fact layers denote non-object values —
/// objects live in the heap, never in the value domain — so this holds whenever
/// a value fact is present. A `Singleton`/`OneOf` is checked inhabitant-wise so
/// the rule stays correct (a *mixed* `OneOf` would be `Maybe`) if the value
/// domain ever gains an object inhabitant; the scalar-base layers admit only a
/// scalar base plus optionally `null`, both non-objects.
fn fact_is_non_object(f: &Fact) -> bool {
    match f {
        Fact::Singleton(v) => val_is_non_object(v),
        Fact::OneOf(vs) => vs.iter().all(val_is_non_object),
        Fact::Refined { .. } | Fact::General { .. } => true,
    }
}

/// Whether a concrete [`Val`] is a non-object PHP value. Exhaustive by design:
/// no current `Val` variant denotes an object, and if one is ever added this
/// match forces a deliberate decision rather than silently answering `No` to an
/// `instanceof` on a value that could be an object.
fn val_is_non_object(v: &Val) -> bool {
    match v {
        Val::Int(_) | Val::Float(_) | Val::Str(_) | Val::Bool(_) | Val::Null | Val::Array(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Guard refinement (ADR-0031 stage 1 → stage 2 negative facts). A guard narrows
// a variable's fact on the branch where it holds. Stage 1's positive `$x === v`
// binds a Singleton; stage 2 adds the *negative* facts (ADR-0031): `!== null`
// clears nullability, `!== v` removes a member (or, for `!== ''`, adds
// NON_EMPTY), ordering guards intersect an int interval, and truthiness adds
// NON_FALSY / clears null. Instanceof binds nothing — membership is not
// exactness (a subclass instance would make an exact-class fact WRONG).
//
// A refinement that would empty a fact (e.g. an int-range intersection with no
// overlap, reachable only across an `&&` of contradictory guards) drops the
// var's fact rather than signalling branch-death: the decided-guard verdict
// already prunes truly-dead branches up front, so dropping-to-no-fact is the
// sound, simpler fallback here (documented choice).
// ---------------------------------------------------------------------------

/// The fact a parameter's **native** type guarantees at runtime (Feature B), or
/// `None` when nothing representable is seeded this slice. Only a **single scalar**
/// type (optionally nullable) seeds — a `General{base, nullable}` fact; unions and
/// bool-literal members (`string|false`) have no clean single-`Fact` form and are
/// skipped (documented). By-ref params are never seeded (the caller may hold
/// anything and the var can be rebound); variadic params are skipped.
fn seed_fact(p: &Param) -> Option<Fact> {
    if p.by_ref || p.variadic {
        return None;
    }
    let ty = p.ty.as_ref()?;
    let [TypeMember::Scalar(scalar)] = ty.members.as_slice() else { return None };
    let base = match scalar {
        ScalarType::Int => Base::Int,
        ScalarType::Float => Base::Float,
        ScalarType::String => Base::String,
        ScalarType::Bool => Base::Bool,
    };
    // A `= null` default makes even a non-`?T` param implicitly nullable.
    let nullable = ty.nullable || p.has_null_default;
    Some(Fact::General { base, nullable })
}

/// Seed a parameter's contract-fact arm lane (ADR-0052 §9): the native member list
/// at [`Stratum::Verified`], refined by the `@param` phpdoc envelope at
/// [`Stratum::Asserted`] (ADR-0037 trust order). Returns the declaration-ordered
/// arm list, or `None` when neither source yields a representable arm.
///
/// - **native only** (`int|string $x`, no `@param`): the scalar/instance/null arms,
///   each `Verified` — a runtime-enforced entry fact.
/// - **phpdoc present** (`object $value` + `@param User|Guest`): the phpdoc's arms
///   are the declared contract; an arm the native type *also* proves (`arm_eq` to a
///   native arm) stays `Verified`, every other (refined/added) arm is `Asserted` —
///   a claim, never a proof (so an `Asserted` arm can never premise the proof layer,
///   and an S6 finding it premises inherits the min stratum).
///
/// By-ref / variadic params are skipped (the caller may rebind a by-ref; a variadic
/// is an array), matching [`seed_fact`].
///
/// `resolve_class` namespace-resolves a phpdoc **class** arm's name to its normalized
/// (lowercase, no leading `\`) project FQN, using the callee file's namespace/use
/// context — the same resolution [`Cx::resolve_pclass`] performs for every other
/// phpdoc class contract. Without it, a `@param User|Guest` under a `namespace`
/// would seed the arm lane with the *unqualified* names while the `instanceof`
/// subtrahend (and S6's `find_class`) carry the fully-qualified ones, so subtraction
/// would silently keep both arms and the declared-receiver lane could never close
/// (the latent N4 gap this consumer surfaces). Native `Instance` arms are already
/// FQN-resolved at syntax lowering, so only the phpdoc arms are re-resolved.
fn seed_contract_arms(
    p: &Param,
    phpdoc: Option<&PType>,
    resolve_class: &dyn Fn(&str) -> String,
) -> Option<Vec<ContractArm>> {
    if p.by_ref || p.variadic {
        return None;
    }
    let native: Vec<ContractTy> = p.ty.as_ref().map(native_arms).unwrap_or_default();
    match phpdoc {
        Some(pt) => {
            let arms = flatten_arms(steins_contract::lower(pt));
            let out: Vec<ContractArm> = arms
                .into_iter()
                .map(|ty| {
                    // Resolve a top-level class arm against the callee namespace; the
                    // native member list already holds FQNs, so this aligns the two.
                    let ty = match ty {
                        ContractTy::Class(n) => ContractTy::Class(resolve_class(&n)),
                        other => other,
                    };
                    let stratum = if native.iter().any(|n| normalize::arm_eq(n, &ty)) {
                        Stratum::Verified
                    } else {
                        Stratum::Asserted
                    };
                    ContractArm { ty, stratum }
                })
                .collect();
            (!out.is_empty()).then_some(out)
        }
        None => {
            let out: Vec<ContractArm> =
                native.into_iter().map(|ty| ContractArm { ty, stratum: Stratum::Verified }).collect();
            (!out.is_empty()).then_some(out)
        }
    }
}

/// Lower a native scalar/union type to contract arms (declaration order, then a
/// `null` arm when nullable). Every native member is representable: the four
/// scalars, `false`/`true` literals, and object `Instance` members (the lowercase
/// FQN, matching [`ContractTy::Class`]'s normalization).
fn native_arms(ty: &NativeType) -> Vec<ContractTy> {
    let mut arms: Vec<ContractTy> = ty
        .members
        .iter()
        .map(|m| match m {
            TypeMember::Scalar(ScalarType::Int) => ContractTy::Base(Base::Int),
            TypeMember::Scalar(ScalarType::Float) => ContractTy::Base(Base::Float),
            TypeMember::Scalar(ScalarType::String) => ContractTy::Base(Base::String),
            TypeMember::Scalar(ScalarType::Bool) => ContractTy::Base(Base::Bool),
            TypeMember::BoolLiteral(b) => ContractTy::LitBool(*b),
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
            TypeMember::InstanceInter(cs) => {
                ContractTy::Inter(cs.iter().map(|c| ContractTy::Class(c.fqn.clone())).collect())
            }
        })
        .collect();
    if ty.nullable {
        arms.push(ContractTy::Null);
    }
    arms
}

/// Flatten a lowered contract into a top-level arm list, dissolving nested unions
/// (a declared `User|Guest|null` lowers to a `Union`; each member is one arm). A
/// non-union lowers to a single arm.
fn flatten_arms(cty: ContractTy) -> Vec<ContractTy> {
    match cty {
        ContractTy::Union(members) => members.into_iter().flat_map(flatten_arms).collect(),
        other => vec![other],
    }
}

/// One narrowing a guard establishes for a variable on a given branch.
enum Refine {
    /// `$x === v` (then) — narrow to exactly this value.
    Exact(String, Val),
    /// `$x !== null` (then) / `$x === null` (else) — drop nullability: clear the
    /// abstract `nullable` flag, or remove the `null` member of a finite fact.
    NotNull(String),
    /// `$x !== v` (non-null, then) — remove `v` from a finite fact; for a
    /// String-based abstract fact and `v == ""`, add `NON_EMPTY` instead.
    Exclude(String, Val),
    /// `$x > k` &c. — intersect an Int-based abstract fact with this interval.
    IntRange(String, IntRange),
    /// `if ($x)` (then) — truthiness: clear nullability and, for a String-based
    /// fact, add `NON_FALSY` (a truthy string is neither `""` nor `"0"`).
    Truthy(String),
}

/// The refinements that hold when `cond` is TRUE (the then-branch).
fn then_refinements(cond: &CondExpr) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, true, &mut out);
    out
}

/// The refinements that hold when `cond` is FALSE (the else-branch).
fn else_refinements(cond: &CondExpr) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, false, &mut out);
    out
}

/// Guard calls whose `@phpstan-assert-if-true`/`-if-false` envelope applies on the
/// given branch polarity, in source order — the same And-then / Or-else
/// distribution as [`collect_refine`], so a call nested in a threaded `&&`/`||`
/// (`if ($a && isNonEmpty($s))`) reaches its consumption point (ADR-0052 §6). The
/// paired `bool` is whether the call returned `true` on this branch (it flips under
/// `Not`), selecting the [`AssertKind`] polarity.
fn collect_guard_calls<'a>(cond: &'a CondExpr, then: bool, out: &mut Vec<(&'a CallExpr, bool)>) {
    match cond {
        CondExpr::Call { call, .. } => out.push((call, then)),
        CondExpr::Not(c) => collect_guard_calls(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_guard_calls(a, true, out);
            collect_guard_calls(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_guard_calls(a, false, out);
            collect_guard_calls(b, false, out);
        }
        _ => {}
    }
}

/// Every retained guard call anywhere in the condition (both polarities), for the
/// position-sequenced escape/sweep and by-ref invalidation that apply on *every*
/// resulting path (a call in either operand may have executed on the excluded path).
fn collect_guard_calls_any(cond: &CondExpr) -> Vec<&CallExpr> {
    let mut out = Vec::new();
    collect_all_calls(cond, &mut out);
    out
}

fn collect_all_calls<'a>(cond: &'a CondExpr, out: &mut Vec<&'a CallExpr>) {
    match cond {
        CondExpr::Call { call, .. } => out.push(call),
        CondExpr::Not(c) => collect_all_calls(c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_all_calls(a, out);
            collect_all_calls(b, out);
        }
        _ => {}
    }
}

/// Collect the refinements a condition implies on the given polarity (`then` =
/// true-path, `!then` = false-path). Negation flips polarity; `&&` distributes on
/// the true-path, `||` on the false-path (De Morgan).
fn collect_refine(cond: &CondExpr, then: bool, out: &mut Vec<Refine>) {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => collect_cmp_refine(*op, lhs, rhs, then, out),
        CondExpr::Truthy(op) => {
            // Only the true-path of a bare truthiness test refines (the false-path
            // — "falsy" — is not cleanly representable: `""`, `"0"`, `0`, null …).
            if then && let CondOperand::Var(v) = op {
                out.push(Refine::Truthy(v.clone()));
            }
        }
        CondExpr::Not(c) => collect_refine(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_refine(a, true, out);
            collect_refine(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_refine(a, false, out);
            collect_refine(b, false, out);
        }
        _ => {}
    }
}

/// Refinements from a comparison guard on a given polarity.
fn collect_cmp_refine(op: CmpOp, lhs: &CondOperand, rhs: &CondOperand, then: bool, out: &mut Vec<Refine>) {
    // Identity/equality guards over a (var, literal) pair.
    if let Some((v, val)) = var_literal(lhs, rhs) {
        // The *effective* operator on this branch: `===`/`!==` flip under `!then`.
        let identical = match (op, then) {
            (CmpOp::Identical, true) | (CmpOp::NotIdentical, false) => Some(true),
            (CmpOp::NotIdentical, true) | (CmpOp::Identical, false) => Some(false),
            _ => None,
        };
        if let Some(identical) = identical
            && let Some(vv) = val_of(&val)
        {
            match (identical, &vv) {
                (true, _) => out.push(Refine::Exact(v, vv)),
                (false, Val::Null) => out.push(Refine::NotNull(v)),
                (false, _) => out.push(Refine::Exclude(v, vv)),
            }
            return;
        }
    }
    // Ordering guards over a (var, int-literal) pair → an interval intersection.
    if let Some((v, k, var_on_left)) = var_int_literal(lhs, rhs) {
        // Normalize so the operator reads `var <op> k`.
        let eff_op = if var_on_left { op } else { flip_ordering(op) };
        // On the false-path the guard is negated.
        let branch_op = if then { eff_op } else { negate_ordering(eff_op) };
        if let Some(range) = ordering_range(branch_op, k) {
            out.push(Refine::IntRange(v, range));
        }
    }
}

/// Collect the `instanceof` guards a condition establishes on the given polarity
/// (`then` = true-path, `!then` = false-path), with the **branch** polarity per
/// guard (`positive` = the guard held). Negation flips polarity; `&&` distributes
/// on the true-path, `||` on the false-path — the same De Morgan distribution as
/// [`collect_refine`], so an `instanceof` nested in `$a && $v instanceof T` reaches
/// its narrowing point. Only bare-variable operands are collected (a `$x->p
/// instanceof T` is depth-1 property narrowing, N5's concern, not here).
fn collect_instanceof<'a>(
    cond: &'a CondExpr,
    then: bool,
    out: &mut Vec<(&'a str, &'a NameRef, bool)>,
) {
    match cond {
        CondExpr::Instanceof { operand: CondOperand::Var(v), class_ref } => {
            out.push((v.as_str(), class_ref, then));
        }
        CondExpr::Not(c) => collect_instanceof(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_instanceof(a, true, out);
            collect_instanceof(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_instanceof(a, false, out);
            collect_instanceof(b, false, out);
        }
        _ => {}
    }
}

/// Apply a branch's **class-fact** narrowing (ADR-0052 N4) to its cloned `Store`:
/// the two NEW carriers, mutated arm-wise through the real is-a oracle. This runs
/// beside [`apply_refinements`] (which owns the value-domain `Fact` carrier).
///
/// 1. Each `instanceof T` guard on this branch subtracts from the variable's
///    **contract lane** — the negative branch deletes arm `M` iff `is_a(M, T) = Yes`
///    (is-a inherited), the positive branch deletes `M` only when `M` is final/enum
///    and `is_a(M, T) = No` — through steins-contract's single deletion judgment
///    ([`normalize::subtrahend_covers`]), preserving each surviving arm's stratum.
///    An emptied lane drops to no-fact (never a death signal, §2). The `oracle`
///    threads the A11 demotion into exactly these arm-deletion queries.
/// 2. The same guard binds the **`Member`** fact at `Verified` (the runtime test
///    executed): the positive branch adds `T` to `yes`, the negative to `no`.
/// 3. A `!== null` on this branch subtracts the `null` arm from the contract lane
///    (the nullable-bit analogue for the arm carrier).
fn apply_class_narrowing(w: &WalkCx, cond: &CondExpr, then: bool, store: &mut Store) {
    let oracle = ProjectIsa { cx: w.cx, demote_catalog: w.cx.a11_demote_catalog() };

    let mut ins = Vec::new();
    collect_instanceof(cond, then, &mut ins);
    for (var, class_ref, positive) in ins {
        let norm = w.cx.class_fqn(class_ref).trim_start_matches('\\').to_ascii_lowercase();
        // (1) Contract-arm subtraction (both polarities), strata preserved.
        subtract_contract_lane(
            store,
            var,
            &normalize::Subtrahend::Class { fqn: norm.clone(), polarity: positive },
            &oracle,
        );
        // (2) Member binding: positive → `yes`, negative → `no`.
        let m = store.members.entry(var.to_owned()).or_default();
        let bucket = if positive { &mut m.yes } else { &mut m.no };
        if !bucket.iter().any(|c| c.eq_ignore_ascii_case(&norm)) {
            bucket.push(norm);
        }
    }

    // (3) `!== null` on this branch → drop the `null` arm of the contract lane.
    let mut refs = Vec::new();
    collect_refine(cond, then, &mut refs);
    for r in &refs {
        if let Refine::NotNull(var) = r {
            subtract_contract_lane(store, var, &normalize::Subtrahend::Null, &oracle);
        }
    }
}

/// Subtract `sub` from `var`'s contract lane in `store`, arm-wise, preserving each
/// surviving arm's stratum (the single deletion judgment [`normalize::subtrahend_covers`]
/// applied to the stratified lane); an emptied lane drops to no-fact.
fn subtract_contract_lane(
    store: &mut Store,
    var: &str,
    sub: &normalize::Subtrahend,
    oracle: &dyn normalize::IsaOracle,
) {
    if let Some(arms) = store.contract.get_mut(var) {
        arms.retain(|a| !normalize::subtrahend_covers(sub, &a.ty, oracle).is_yes());
        if arms.is_empty() {
            store.contract.remove(var);
        }
    }
}

/// The `($var, literal)` of a comparison whose two operands are exactly one bare
/// variable and one literal (in either order).
fn var_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, ArgValue)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(val))
        | (CondOperand::Literal(val), CondOperand::Var(v)) => Some((v.clone(), val.clone())),
        _ => None,
    }
}

/// The `($var, int_literal, var_on_left)` of a comparison with one bare variable
/// and one **int** literal (ordering refinement only applies to int bounds).
fn var_int_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, i64, bool)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(ArgValue::Int(i))) => Some((v.clone(), *i, true)),
        (CondOperand::Literal(ArgValue::Int(i)), CondOperand::Var(v)) => Some((v.clone(), *i, false)),
        _ => None,
    }
}

/// Mirror an ordering operator (used when the variable is the right operand).
fn flip_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

/// The logical negation of an ordering operator (for the false-path).
fn negate_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Ge,
        CmpOp::Le => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Ge => CmpOp::Lt,
        other => other,
    }
}

/// The int interval `var <op> k` denotes (`> 0` → positive, `>= 0` → non-negative,
/// `<`/`<=` symmetric). `None` when the interval is empty (a saturating bound
/// overflow) — the caller adds no refinement.
fn ordering_range(op: CmpOp, k: i64) -> Option<IntRange> {
    match op {
        CmpOp::Gt => IntRange::new(k.checked_add(1)?, i64::MAX),
        CmpOp::Ge => IntRange::new(k, i64::MAX),
        CmpOp::Lt => IntRange::new(i64::MIN, k.checked_sub(1)?),
        CmpOp::Le => IntRange::new(i64::MIN, k),
        _ => None,
    }
}

/// Apply a branch's refinements to its cloned env (clearing any stale exact-class
/// fact for a positively-narrowed variable), at trust stratum `stratum` (ADR-0052
/// §5): `Verified` for native-condition branches (the runtime test executed),
/// `Asserted` for an `assert($expr)`-derived narrowing (never evaluated under
/// `zend.assertions=-1`).
fn apply_refinements(
    refs: &[Refine],
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    stratum: Stratum,
) {
    for r in refs {
        match r {
            // `=== v` replaces the value with proven equality: the fact is exactly
            // as trustworthy as the test that established it (`stratum`), regardless
            // of any prior (weaker) knowledge about the variable.
            Refine::Exact(var, val) => {
                env.insert(
                    var.clone(),
                    Known::value_strat(
                        Fact::Singleton(val.clone()),
                        0,
                        Some("proven on this branch".to_owned()),
                        stratum,
                    ),
                );
                store.unbind(var);
            }
            Refine::NotNull(var) => refine_fact(env, var, stratum, clear_null),
            Refine::Exclude(var, val) => {
                refine_fact(env, var, stratum, |f| exclude_member(f, val));
            }
            Refine::IntRange(var, range) => {
                refine_fact(env, var, stratum, |f| intersect_int(f, *range));
            }
            Refine::Truthy(var) => refine_fact(env, var, stratum, truthy_narrow),
        }
    }
}

/// Transform the fact of `var` in place with `f` (a `None` result drops the fact —
/// the conservative empty-fact fallback); a no-op when `var` has no fact. The
/// result stratum is `min(existing, refine_stratum)`: a narrowing (`!== null`,
/// interval, truthy, member exclusion) constrains the *existing* fact, so it is
/// only as trustworthy as its weakest component (ADR-0052 §5 derivation clause).
fn refine_fact(
    env: &mut HashMap<String, Known>,
    var: &str,
    refine_stratum: Stratum,
    f: impl FnOnce(&Fact) -> Option<Fact>,
) {
    let Some(k) = env.get(var) else { return };
    // A closure-only binding carries no scalar fact — value refinements do not
    // apply to it; leave it intact.
    let Some(kf) = &k.fact else { return };
    match f(kf) {
        Some(nf) => {
            let (line, bound, stratum) = (k.line, k.bound.clone(), k.stratum.min(refine_stratum));
            env.insert(var.to_owned(), Known::value_strat(nf, line, bound, stratum));
        }
        None => {
            env.remove(var);
        }
    }
}

/// Clear nullability: an abstract fact loses its `nullable` flag; a finite fact
/// loses its `null` member. `None` only if that empties a finite fact.
fn clear_null(f: &Fact) -> Option<Fact> {
    match f {
        Fact::Refined { base, refinement, nullable: true } => {
            Some(Fact::refined(*base, *refinement, false))
        }
        Fact::General { base, nullable: true } => Some(Fact::General { base: *base, nullable: false }),
        Fact::Singleton(_) | Fact::OneOf(_) => exclude_member(f, &Val::Null),
        // Already non-nullable abstract fact — unchanged.
        other => Some(other.clone()),
    }
}

/// Remove `val` from a finite fact; for a String-based abstract fact excluding
/// `""`, add `NON_EMPTY` (the `!== ''` refinement). Otherwise unchanged.
fn exclude_member(f: &Fact, val: &Val) -> Option<Fact> {
    match f.finite_members() {
        Some(members) => {
            let kept: Vec<Val> = members.iter().filter(|m| *m != val).cloned().collect();
            // Empty → drop the fact (conservative fallback; a truly-dead branch is
            // already pruned by the decided-guard verdict).
            Fact::from_vals(kept)
        }
        None => match (f, val) {
            (Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. }, Val::Str(s))
                if s.is_empty() =>
            {
                Some(add_str_preds(f, StrPreds::NON_EMPTY))
            }
            _ => Some(f.clone()),
        },
    }
}

/// Intersect an Int-based abstract fact with `range`; a finite/other fact is left
/// unchanged. `None` when the intersection is empty.
fn intersect_int(f: &Fact, range: IntRange) -> Option<Fact> {
    match f {
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(have), nullable } => {
            let r = have.intersect(range)?;
            Some(Fact::refined(Base::Int, Refinement::Int(r), *nullable))
        }
        Fact::General { base: Base::Int, nullable } => {
            Some(Fact::refined(Base::Int, Refinement::Int(range), *nullable))
        }
        other => Some(other.clone()),
    }
}

/// Truthiness narrowing on the true-path: clear nullability (null is falsy) and,
/// for a String-based fact, add `NON_FALSY`. Int-based facts gain nothing usable
/// (nonzero is not an interval — skipped, documented). Never empties.
fn truthy_narrow(f: &Fact) -> Option<Fact> {
    let f = clear_null(f)?;
    Some(match &f {
        Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. } => {
            add_str_preds(&f, StrPreds::NON_FALSY)
        }
        other => other.clone(),
    })
}

/// Add string predicates to a String-based abstract fact (union-closed); a
/// non-string or finite fact is returned unchanged.
fn add_str_preds(f: &Fact, preds: StrPreds) -> Fact {
    match f {
        Fact::Refined { base: Base::String, refinement: Refinement::Str(have), nullable } => {
            Fact::refined(Base::String, Refinement::Str(have.union(preds)), *nullable)
        }
        Fact::General { base: Base::String, nullable } => {
            Fact::refined(Base::String, Refinement::Str(preds), *nullable)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// `@phpstan-assert` application (ADR-0030, Feature D). After a call to an
// assertion helper, the asserted type narrows the CALLER's env for the variable
// passed at the asserted position. `Always` asserts apply on the fall-through
// (statement position); `-if-true`/`-if-false` apply only in guard position.
// ---------------------------------------------------------------------------

/// Convert a lowered contract type to the domain [`Fact`] an assertion of it
/// establishes (conservative): `Base` → General, `IntIn` → Refined, `StrWith` →
/// Refined, `Null` → `Singleton(null)`, a nullable union (`X|null`) → `X`'s fact
/// with `nullable = true`; anything else → `None` (no application).
fn assert_fact_of(cty: &steins_contract::ContractTy) -> Option<Fact> {
    use steins_contract::ContractTy as C;
    match cty {
        C::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        C::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        C::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        C::Null => Some(Fact::Singleton(Val::Null)),
        C::Union(members) => {
            let has_null = members.iter().any(|m| matches!(m, C::Null));
            let non_null: Vec<&C> = members.iter().filter(|m| !matches!(m, C::Null)).collect();
            // `X|null` (exactly one representable non-null member) → X, nullable.
            if has_null && non_null.len() == 1 {
                return Some(with_nullable(assert_fact_of(non_null[0])?));
            }
            None
        }
        _ => None,
    }
}

/// Set the `nullable` flag on an abstract fact (a `Singleton`/`OneOf` is left
/// unchanged — a nullable-union member never lowers to a finite fact here).
fn with_nullable(f: Fact) -> Fact {
    match f {
        Fact::General { base, .. } => Fact::General { base, nullable: true },
        Fact::Refined { base, refinement, .. } => Fact::refined(base, refinement, true),
        other => other,
    }
}

/// Apply the `Always` assertions of every statically-resolved call in a statement
/// to the caller's env (Feature D) — the fall-through position. `-if-true`/
/// `-if-false` asserts are conditional on the boolean result and belong to guard
/// position (see [`apply_guard_asserts`]).
#[allow(clippy::too_many_arguments)]
fn apply_stmt_asserts(
    cx: &Cx,
    scope: &Scope,
    call: &CallExpr,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    asserted: &mut HashSet<String>,
) {
    apply_call_asserts(cx, scope, call, env, store, this_exact, enclosing_class, AssertKind::Always, asserted);
}

/// Apply a guard-position call's `@phpstan-assert-if-true`/`-if-false` specs to a
/// branch env (ADR-0052 §5, at the `Asserted` stratum). `kind` selects the
/// polarity: `IfTrue` on the true branch, `IfFalse` on the false branch. This is
/// the *minimal* guard-call tag consumption — the full retained-guard-call
/// machinery (§6) is N3.
fn apply_guard_asserts(
    w: &WalkCx,
    call: &CallExpr,
    kind: AssertKind,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let mut asserted = HashSet::new();
    apply_call_asserts(
        w.cx, w.scope, call, env, store, w.this_exact, w.enclosing_class, kind, &mut asserted,
    );
}

/// Resolve a call's callee declaration and apply every assertion spec of a given
/// `kind`, mapping each spec's `@param` name to the call's positional argument
/// variable and narrowing it via [`apply_assert_to_var`] (always at the `Asserted`
/// stratum). Shared by the fall-through (`Always`) and guard (`IfTrue`/`IfFalse`)
/// consumption points.
#[allow(clippy::too_many_arguments)]
fn apply_call_asserts(
    cx: &Cx,
    scope: &Scope,
    call: &CallExpr,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    kind: AssertKind,
    asserted: &mut HashSet<String>,
) {
    if scope.poisoned || !call.positional_only {
        return;
    }
    let (params, docblock): (&[Param], Option<&str>) = match &call.receiver {
        Callee::Function(_) => {
            let Some(site) = cx.resolve_user_fn(call) else { return };
            let decl = cx.fn_decl(site);
            (&decl.params, decl.docblock.as_deref())
        }
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            let Some(target) = resolve_call_target(
                cx, &call.receiver, store, this_exact, enclosing_class, scope.poisoned,
            ) else {
                return;
            };
            (&target.method.params, target.method.docblock.as_deref())
        }
        // A `$fn(...)` variable call carries no static declaration to read
        // `@phpstan-assert` envelopes from — nothing to apply.
        Callee::DynamicVar(_) | Callee::Dynamic => return,
    };
    let Some(envelopes) = parse_envelopes(docblock) else { return };
    for spec in &envelopes.asserts {
        if spec.kind != kind {
            continue;
        }
        let Some(pos) = params.iter().position(|p| p.name == spec.param) else { continue };
        let Some(arg) = call.args.get(pos) else { continue };
        let ArgValue::Var(v) = &arg.value else { continue };
        if apply_assert_to_var(env, store, v, spec) {
            asserted.insert(v.clone());
        }
    }
}

/// Apply one assertion spec to a caller variable at the **`Asserted`** stratum
/// (ADR-0052 §5 — a docblock is a claim, never a proof). Replace-if-weaker, in two
/// halves: (1) a stronger finite fact (`Singleton`/`OneOf`) is kept — an assert
/// never coarsens known-exact knowledge; (2) **an `Asserted` fact never overwrites
/// a `Verified` one of any layer** — the missing half this slice adds, so a lying
/// `@phpstan-assert` cannot downgrade a proven fact into a forgeable one (nor
/// launder its own claim past the stratum gate). A negated `!null` clears
/// nullability (also `Asserted`); other negated forms are not representable as a
/// positive fact and are skipped (documented).
///
/// Returns whether the variable now carries an established fact (so the caller
/// protects it from the by-ref invalidation) — `true` when a fact was set or a
/// stronger/Verified fact was deliberately kept, `false` when nothing applied.
fn apply_assert_to_var(
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    var: &str,
    spec: &AssertSpec,
) -> bool {
    let cty = steins_contract::lower(&spec.ty);
    if spec.negated {
        // Only `!null` is representable as a positive narrowing (clear nullable);
        // other negated forms establish nothing. The narrowing is `Asserted`, so
        // `refine_fact` mins the result to `Asserted`.
        if matches!(cty, steins_contract::ContractTy::Null) && env.contains_key(var) {
            refine_fact(env, var, Stratum::Asserted, clear_null);
            return true;
        }
        return false;
    }
    let Some(fact) = assert_fact_of(&cty) else { return false };
    // Keep the existing fact when it is a stronger finite layer OR when it is
    // already `Verified` (replace-if-weaker, both halves): an `Asserted` claim may
    // neither coarsen exact knowledge nor overwrite a proven fact. Either way the
    // variable stays protected from the by-ref invalidation (a by-value assert did
    // not mutate it).
    if env.get(var).is_some_and(|k| {
        k.stratum == Stratum::Verified || k.fact.as_ref().is_some_and(|f| f.finite_members().is_some())
    }) {
        return true;
    }
    env.insert(var.to_owned(), Known::value_strat(fact, 0, Some("asserted".to_owned()), Stratum::Asserted));
    store.unbind(var);
    true
}

/// Every bare variable an opaque sub-condition reads (for the guard-mutation
/// invalidation: an opaque condition may mutate its operands by reference).
fn cond_invalidations(cond: &CondExpr) -> Vec<String> {
    let mut out = Vec::new();
    collect_cond_opaque_reads(cond, &mut out);
    out
}

fn collect_cond_opaque_reads(cond: &CondExpr, out: &mut Vec<String>) {
    match cond {
        // An opaque condition may mutate any variable it reads by reference — the
        // whole read-set is forgotten (the conservative floor, unchanged).
        CondExpr::Opaque { reads } => {
            for r in reads {
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
        }
        // A retained guard call forgets its by-ref arguments and every nested
        // by-ref mention — but NOT a pure method receiver (`$x` in `$x->m()` is not
        // rebound by the call, only its object's props are swept; ADR-0052 §6 payoff
        // (i)). The receiver survives only when it is not also handed in as an
        // argument (`$x->m($x)` passes it by value/ref, so it is still forgotten).
        CondExpr::Call { call, reads } => {
            let recv = call_method_receiver_var(call);
            let recv_is_arg = recv.is_some_and(|r| {
                call.args.iter().any(|a| matches!(&a.value, ArgValue::Var(v) if v == r))
            });
            for r in reads {
                if Some(r.as_str()) == recv && !recv_is_arg {
                    continue;
                }
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
        }
        CondExpr::Not(c) => collect_cond_opaque_reads(c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_cond_opaque_reads(a, out);
            collect_cond_opaque_reads(b, out);
        }
        _ => {}
    }
}

/// The bare method-receiver variable of a call (`$x` in `$x->m(...)`), or `None`
/// for a function/static/constructor/dynamic call or a non-variable receiver.
fn call_method_receiver_var(call: &CallExpr) -> Option<&str> {
    match &call.receiver {
        Callee::Method { receiver: Receiver::Var(v), .. } => Some(v),
        _ => None,
    }
}

/// Join the fall-through envs of several live branches (ADR-0031/0035): a scalar
/// fact survives only when present in *every* branch, folded through [`Fact::join`]
/// (equal → Singleton; differing → OneOf; overflow → dropped). An exact-class fact
/// survives only when every branch agrees on the class.
fn join_envs(
    branches: Vec<(HashMap<String, Known>, Store)>,
) -> (HashMap<String, Known>, Store) {
    let mut it = branches.into_iter();
    let (first_env, first_classes) = it.next().expect("join_envs called with no branches");
    let rest: Vec<(HashMap<String, Known>, Store)> = it.collect();
    if rest.is_empty() {
        return (first_env, first_classes);
    }

    let mut env: HashMap<String, Known> = HashMap::new();
    for (name, k0) in &first_env {
        // A closure-only binding survives a join only when every branch binds the
        // SAME closure target (a differing/absent branch drops it — the safe side).
        if let Some(cv0) = &k0.closure {
            let all_same = rest.iter().all(|(be, _)| {
                be.get(name)
                    .and_then(|k| k.closure.as_ref())
                    .is_some_and(|cv| closure_target_eq(&cv0.target, &cv.target))
            });
            if all_same {
                env.insert(name.clone(), Known::closure(cv0.clone(), k0.line));
            }
            continue;
        }
        let Some(mut fact) = k0.fact.clone() else { continue };
        // Derivation clause: a branch join takes `min` over the joined arms' strata
        // (Verified ⊔ Asserted ⇒ Asserted). The neutral start is `k0`'s own stratum.
        let mut stratum = k0.stratum;
        let mut ok = true;
        for (be, _) in &rest {
            match be.get(name) {
                Some(k) if k.fact.is_some() => match fact.join(k.fact.as_ref().expect("checked")) {
                    Some(joined) => {
                        fact = joined;
                        stratum = stratum.min(k.stratum);
                    }
                    None => {
                        ok = false;
                        break;
                    }
                },
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            env.insert(name.clone(), Known::value_strat(fact, k0.line, k0.bound.clone(), stratum));
        }
    }

    let rest_stores: Vec<&Store> = rest.iter().map(|(_, s)| s).collect();
    let store = join_stores(&first_classes, &rest_stores);
    (env, store)
}

/// Join the heap stores of several fall-through branches (ADR-0036). A variable's
/// ObjRef survives only when every branch binds it to the SAME allocation id (a
/// pre-branch object keeps its id across the clones; a per-branch `new`/`clone`
/// gets a distinct id and so is dropped). A surviving object joins its props
/// member-wise (a prop survives only if present-and-joinable in every branch),
/// unions `escaped` (escaped anywhere → escaped), and intersects `ro_written` (a
/// readonly write counts only when proven on every joined path).
fn join_stores(first: &Store, rest: &[&Store]) -> Store {
    let mut refs: HashMap<String, AllocId> = HashMap::new();
    for (var, id) in &first.refs {
        if rest.iter().all(|s| s.refs.get(var) == Some(id)) {
            refs.insert(var.clone(), *id);
        }
    }
    let mut heap: HashMap<AllocId, HeapObj> = HashMap::new();
    // Join every id that survives via a ref (and any id present in all branches).
    let live_ids: HashSet<AllocId> = refs.values().copied().collect();
    for id in live_ids {
        let Some(o0) = first.heap.get(&id) else { continue };
        let others: Vec<&HeapObj> = rest.iter().filter_map(|s| s.heap.get(&id)).collect();
        if others.len() != rest.len() {
            continue; // not present in every branch — drop it
        }
        let mut joined = HeapObj::new(o0.class.clone());
        // A surviving id is the SAME allocation across every branch, so its class and
        // exactness bit are invariant — carry them from the first branch (audit G1).
        joined.class_exact = o0.class_exact;
        joined.readonly = o0.readonly.clone();
        joined.escaped = o0.escaped || others.iter().any(|o| o.escaped);
        // ro_written: written on EVERY joined path.
        joined.ro_written = o0
            .ro_written
            .iter()
            .filter(|n| others.iter().all(|o| o.ro_written.contains(*n)))
            .cloned()
            .collect();
        // props: present-and-joinable in every branch, at `min` over strata
        // (derivation clause — a joined prop is Asserted if any branch's was).
        for (name, p0) in &o0.props {
            let mut fact = p0.fact.clone();
            let mut stratum = p0.stratum;
            let mut ok = true;
            for o in &others {
                match o.props.get(name) {
                    Some(kp) => match fact.join(&kp.fact) {
                        Some(j) => {
                            fact = j;
                            stratum = stratum.min(kp.stratum);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    },
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                joined.props.insert(name.clone(), PropFact { fact, stratum });
            }
        }
        heap.insert(id, joined);
    }

    // Contract lane: a var survives the join only if present in every branch; its
    // arms are the union of the branches' surviving arms (any value live on ANY
    // path is possible after the merge), deduped, each arm keeping its own stratum
    // (an `Asserted` arm never launders to `Verified` through a join). Absent in any
    // branch → dropped to no-fact (sound: the successor simply carries no lane).
    let mut contract: HashMap<String, Vec<ContractArm>> = HashMap::new();
    for (var, arms0) in &first.contract {
        if !rest.iter().all(|s| s.contract.contains_key(var)) {
            continue;
        }
        let mut merged = arms0.clone();
        for s in rest {
            if let Some(arms) = s.contract.get(var) {
                merged.extend(arms.iter().cloned());
            }
        }
        dedup_contract_arms(&mut merged);
        contract.insert(var.clone(), merged);
    }

    // Member lane: a var survives only if present in every branch; its `yes`/`no`
    // sets are the INTERSECTION across branches (a bound holds after the merge only
    // if it held on every path). An emptied Member is dropped (no-fact).
    let mut members: HashMap<String, Member> = HashMap::new();
    for (var, m0) in &first.members {
        let others: Vec<&Member> = rest.iter().filter_map(|s| s.members.get(var)).collect();
        if others.len() != rest.len() {
            continue;
        }
        let yes: Vec<String> =
            m0.yes.iter().filter(|c| others.iter().all(|o| o.yes.contains(c))).cloned().collect();
        let no: Vec<String> =
            m0.no.iter().filter(|c| others.iter().all(|o| o.no.contains(c))).cloned().collect();
        if !(yes.is_empty() && no.is_empty()) {
            members.insert(var.clone(), Member { yes, no });
        }
    }

    // Existence-vouch lane (ADR-0049 §4): a vouch survives the join only if EVERY
    // branch carried it — the intersection. A vouch bound on a guarded branch that
    // falls through must not leak onto a sibling path that was never guarded (so the
    // tail of `if (method_exists(C,'m')) {} (new C)->m();` still fires).
    let vouched: HashSet<Vouch> =
        first.vouched.iter().filter(|v| rest.iter().all(|s| s.vouched.contains(*v))).cloned().collect();

    Store { refs, heap, contract, members, vouched }
}

/// Remove contract arms another surviving arm subsumes (`Certainty::Yes`) — the
/// stratified analogue of [`normalize::dedup_arms`]: on a subsumption tie the arm
/// with the **weaker** (min) stratum is kept, so a join can never raise an
/// `Asserted` possibility to `Verified` by dropping it in favor of a `Verified`
/// twin that denotes the same set (ADR-0052 §5 derivation clause).
fn dedup_contract_arms(arms: &mut Vec<ContractArm>) {
    let mut kept: Vec<ContractArm> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        // Collapse a structurally-identical arm FIRST (`ty == ty`), keeping the min
        // stratum. This is the reflexive tie `subsumes`/`arm_eq` deliberately cannot
        // prove for the non-extensional arms (`StrOpaque`, `ArrayAny`/`ListOf`/`MapOf`,
        // `CallableTy`, `Opaque` — ADR-0038: membership is unmodeled, so `subsumes(x, x)`
        // is `Maybe`, not `Yes`). Exact structural equality is a strictly stronger
        // witness of same-denotation than mutual subsumption, so keeping one is sound
        // and loses no precision — and it is what stops a branch-union from *doubling*
        // a pile of identical opaque arms at every join. Without it an `array`/`Closure`
        // parameter threaded through a deeply nested `if` tree grew to 2^depth copies
        // of one arm (survey non-termination on nextcloud `core/Migrations`).
        if let Some(k) =
            kept.iter_mut().find(|k| k.ty == arm.ty || normalize::arm_eq(&k.ty, &arm.ty))
        {
            k.stratum = k.stratum.min(arm.stratum);
            continue;
        }
        // Drop an arm strictly subsumed by a kept one; if it subsumes kept arms,
        // it replaces them (widening), inheriting the min stratum of all it covers.
        if kept.iter().any(|k| normalize::subsumes(&k.ty, &arm.ty).is_yes()) {
            continue;
        }
        let mut stratum = arm.stratum;
        kept.retain(|k| {
            if normalize::subsumes(&arm.ty, &k.ty).is_yes() {
                stratum = stratum.min(k.stratum);
                false
            } else {
                true
            }
        });
        kept.push(ContractArm { ty: arm.ty, stratum });
    }
    *arms = kept;
}

// ---------------------------------------------------------------------------
// PHP scalar comparison semantics (ADR-0031). `===`/truthiness are exact; `==`
// is settled EMPIRICALLY against PHP 8.5.8 (see [`php_loose_eq`]). Undecidable
// cells return `None` → the caller yields `Maybe` (silence, the sound side).
// ---------------------------------------------------------------------------

/// PHP truthiness of a proven value. Note `"0"` and `""` are the only falsy
/// non-empty/-empty strings (`"0.0"` and `"00"` are **truthy**), `0`/`0.0`/`[]`
/// are falsy. `None` for a non-concrete value.
fn php_truthy(v: &ArgValue) -> Option<bool> {
    match v {
        ArgValue::Null => Some(false),
        ArgValue::Bool(b) => Some(*b),
        ArgValue::Int(i) => Some(*i != 0),
        ArgValue::Float(f) => Some(*f != 0.0),
        ArgValue::Str(s) => Some(!(s.is_empty() || s == "0")),
        ArgValue::Array(items) => Some(!items.is_empty()),
        _ => None,
    }
}

/// Strict identity `===`: same runtime type AND equal value. Different concrete
/// runtime types are a definite non-identity; a non-concrete operand is `None`.
fn php_identical(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    match (a, b) {
        (Int(x), Int(y)) => Some(x == y),
        (Float(x), Float(y)) => Some(x == y),
        (Str(x), Str(y)) => Some(x == y),
        (Bool(x), Bool(y)) => Some(x == y),
        (Null, Null) => Some(true),
        (Array(_), Array(_)) => php_array_identical(a, b),
        _ if is_concrete(a) && is_concrete(b) => Some(false),
        _ => None,
    }
}

/// Deep `===` of two array literals: same length, same key order, element-wise
/// identical. A non-concrete element makes the result `None`.
fn php_array_identical(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    let (ArgValue::Array(ai), ArgValue::Array(bi)) = (a, b) else { return None };
    let na = normalize_array(ai);
    let nb = normalize_array(bi);
    if na.len() != nb.len() {
        return Some(false);
    }
    for ((ka, va), (kb, vb)) in na.iter().zip(nb.iter()) {
        if ka != kb {
            return Some(false);
        }
        match php_identical(va, vb) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
}

/// Whether a value is a fully-known concrete value (a scalar literal or an array).
fn is_concrete(v: &ArgValue) -> bool {
    v.is_literal() || matches!(v, ArgValue::Array(_))
}

/// Loose equality `==`, settled **empirically against PHP 8.5.8** (`php -r`, the
/// full cross-product of `null`/`false`/`true`/`0`/`0.0`/`""`/`"0"`/`"abc"`/`"5"`/`[]`
/// recorded). The measured table (`T` = equal):
///
/// ```text
///           null false true  0   0.0   ""   "0"  "abc" "5"   []
///   null     T    T    F    T    T     T    F    F     F     T
///   false    T    T    F    T    T     T    T    F     F     T
///   true     F    F    T    F    F     F    F    T     T     F
///   0        T    T    F    T    T     F    T    F     F     F
///   0.0      T    T    F    T    T     F    T    F     F     F
///   ""       T    T    F    F    F     T    F    F     F     F
///   "0"      F    T    F    T    T     F    T    F     F     F
///   "abc"    F    F    T    F    F     F    F    T     F     F
///   "5"      F    F    T    F    F     F    F    F     T     F
///   []       T    T    F    F    F     F    F    F     F     T
/// ```
///
/// The rules reproduced (stable since PHP 8.0): a `bool` operand casts BOTH sides
/// to bool; `null` compares to the other side's zero/empty (except bool, handled
/// by the bool rule); `int`/`float` vs a numeric string compares numerically,
/// vs a non-numeric string compares the number's string form; two strings compare
/// numerically iff both are numeric strings, else byte-wise; an array is unequal
/// to any scalar (non-null, non-bool). Cells not covered (a `float` vs a
/// non-numeric string; non-trivial arrays) return `None` → `Maybe`.
fn php_loose_eq(a: &ArgValue, b: &ArgValue) -> Option<bool> {
    use ArgValue::{Array, Bool, Float, Int, Null, Str};
    // A `bool` on either side casts both operands to bool (subsumes null==bool).
    if matches!(a, Bool(_)) || matches!(b, Bool(_)) {
        return Some(php_truthy(a)? == php_truthy(b)?);
    }
    match (a, b) {
        (Null, Null) => Some(true),
        (Null, Int(i)) | (Int(i), Null) => Some(*i == 0),
        (Null, Float(f)) | (Float(f), Null) => Some(*f == 0.0),
        (Null, Str(s)) | (Str(s), Null) => Some(s.is_empty()),
        (Null, Array(items)) | (Array(items), Null) => Some(items.is_empty()),
        (Null, _) | (_, Null) => None,

        (Int(x), Int(y)) => Some(x == y),
        (Int(x), Float(y)) | (Float(y), Int(x)) => Some((*x as f64) == *y),
        (Float(x), Float(y)) => Some(x == y),

        (Int(i), Str(s)) | (Str(s), Int(i)) => Some(php_int_str_eq(*i, s)),
        (Float(f), Str(s)) | (Str(s), Float(f)) => php_float_str_eq(*f, s),
        (Str(x), Str(y)) => Some(php_str_eq(x, y)),

        (Array(x), Array(y)) => php_array_loose_eq(x, y),
        // An array is never loosely equal to a (non-null, non-bool) scalar.
        (Array(_), Int(_) | Float(_) | Str(_)) | (Int(_) | Float(_) | Str(_), Array(_)) => {
            Some(false)
        }
        _ => None,
    }
}

/// `int == string`: numeric string → numeric compare; else compare the int's
/// decimal form to the string (PHP 8 semantics).
fn php_int_str_eq(i: i64, s: &str) -> bool {
    if php_is_numeric(s) {
        php_str_to_float(s).is_some_and(|f| (i as f64) == f)
    } else {
        i.to_string() == s
    }
}

/// `float == string`: numeric string → numeric compare; a non-numeric string is
/// undecidable here (float→string formatting is precision-sensitive) → `None`.
fn php_float_str_eq(f: f64, s: &str) -> Option<bool> {
    if php_is_numeric(s) {
        Some(php_str_to_float(s).is_some_and(|g| f == g))
    } else {
        None
    }
}

/// `string == string`: both numeric strings → numeric compare; else byte compare.
fn php_str_eq(x: &str, y: &str) -> bool {
    if php_is_numeric(x) && php_is_numeric(y) {
        match (php_str_to_float(x), php_str_to_float(y)) {
            (Some(a), Some(b)) => a == b,
            _ => x == y,
        }
    } else {
        x == y
    }
}

/// `array == array`: same key set with loosely-equal values (order-independent).
/// An undecidable element value makes the whole comparison `None`.
fn php_array_loose_eq(x: &[(ArrayKey, ArgValue)], y: &[(ArrayKey, ArgValue)]) -> Option<bool> {
    let nx = normalize_array(x);
    let ny = normalize_array(y);
    if nx.len() != ny.len() {
        return Some(false);
    }
    for (k, va) in &nx {
        let Some((_, vb)) = ny.iter().find(|(k2, _)| k2 == k) else {
            return Some(false);
        };
        match php_loose_eq(va, vb) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => return None,
        }
    }
    Some(true)
}

/// The class FQN that lexically owns a method scope; `None` for function/top.
fn scope_class(scope: &Scope) -> Option<&str> {
    match &scope.owner {
        ScopeOwner::Method { class, .. } => Some(class),
        // A closure lexically inside a method captures `$this`, but this slice
        // does not thread the enclosing class into the closure scope (documented).
        ScopeOwner::TopLevel | ScopeOwner::Function(_) | ScopeOwner::Closure { .. } => None,
    }
}

/// The statically-named calls a statement carries.
fn checkable_calls(kind: &StmtKind) -> Vec<&CallExpr> {
    match kind {
        StmtKind::Call(c) => vec![c],
        StmtKind::Return { call: Some(c), .. }
        | StmtKind::Assign { call: Some(c), .. }
        | StmtKind::PropAssign { value_call: Some(c), .. } => vec![c],
        StmtKind::Echo(cs) => cs.iter().collect(),
        _ => Vec::new(),
    }
}

/// Escape + sweep the heap for a statement's calls (ADR-0036). Passing an object
/// (as an argument, or as the `$var` receiver of a method call) escapes it. If any
/// object was passed into a call, or any call is unknown/overridable (not resolved
/// to a project target), sweep every escaped object's non-readonly props. A purely
/// local object never passed anywhere survives an unrelated unknown call — the
/// precision payoff.
fn apply_call_escape_and_sweep(w: &WalkCx, kind: &StmtKind, store: &mut Store) {
    let calls = checkable_calls(kind);
    escape_and_sweep_calls(w, &calls, store);
}

/// Escape + sweep for an explicit set of calls (ADR-0036), shared by the
/// statement-position pass ([`apply_call_escape_and_sweep`]) and the guard-position
/// retained-call handling (ADR-0052 §6): a guard call's object arguments and its
/// method receiver escape, and any object passed in — or any unknown/overridable
/// call — sweeps every escaped object's non-readonly props. The receiver's var→id
/// *binding* survives (a method call does not rebind its receiver variable), so the
/// receiver stays usable on the guarded path; only its mutable props are swept.
fn escape_and_sweep_calls(w: &WalkCx, calls: &[&CallExpr], store: &mut Store) {
    if calls.is_empty() {
        return;
    }
    let mut object_passed = false;
    let mut unknown = false;
    for call in calls {
        if let Callee::Method { receiver: Receiver::Var(v), .. } = &call.receiver
            && store.is_bound(v)
        {
            store.mark_escaped(v);
            object_passed = true;
        }
        for arg in &call.args {
            if let ArgValue::Var(name) = &arg.value
                && store.is_bound(name)
            {
                store.mark_escaped(name);
                object_passed = true;
            }
        }
        if !call_is_resolved(w, call, store) {
            unknown = true;
        }
    }
    if object_passed || unknown {
        store.sweep_escaped();
    }
}

/// Whether a call resolves to a known project/user target (ADR-0036). An unresolved
/// function (builtin/unknown/dynamic) or an unresolved-via-guard method (an
/// overridable `$this`/`self` call) counts as unknown — the sweeping side.
fn call_is_resolved(w: &WalkCx, call: &CallExpr, store: &Store) -> bool {
    match &call.receiver {
        Callee::Function(_) => w.cx.resolve_user_fn(call).is_some(),
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            resolve_call_target(
                w.cx, &call.receiver, store, w.this_exact, w.enclosing_class, w.scope.poisoned,
            )
            .is_some()
        }
        Callee::DynamicVar(_) | Callee::Dynamic => false,
    }
}

/// Check a function call whose arguments may be propagated values (`Var`/`Call`/
/// array). Runs the native runtime check and the phpdoc declared-contract check;
/// a site where the native check fired is skipped by the phpdoc check.
#[allow(clippy::too_many_arguments)]
fn check_propagated_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    in_descent: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let Some(site) = cx.resolve_user_fn(call) else { return };
    let decl = cx.fn_decl(site);
    let envelopes = parse_envelopes(decl.docblock.as_deref());

    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = decl.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            let resolved: Option<(ArgValue, String)> = match &arg.value {
                ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
                    let v = k.singleton()?;
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    Some((v, prov))
                }),
                ArgValue::Call(name, args) => {
                    if args.is_empty() {
                        cx.resolve_const_fn(name)
                            .map(|(lit, line)| {
                                (lit, format!("from {name}(), defined at line {line}"))
                            })
                            .or_else(|| cx.try_fold(name, args, folder))
                    } else {
                        cx.try_fold(name, args, folder)
                    }
                }
                // A property read `$o->p` (ADR-0036): a `Singleton` prop fact flows.
                ArgValue::PropFetch { var, prop } if !poisoned => {
                    store.prop_fact(var, prop).and_then(|f| match f {
                        Fact::Singleton(v) => Some((arg_of_val(v), format!("from ${var}->{prop}"))),
                        _ => None,
                    })
                }
                _ => None,
            };
            // Proof-layer consumption rule (ADR-0052 §5): the native
            // `type.argument-mismatch` fires only on an all-`Verified` premise. A
            // value proven through an `Asserted` env/heap fact stays silent (the
            // phpdoc contract check below still accepts it).
            if let Some((value, provenance)) = resolved
                && value_stratum(&arg.value, env, Some(store)) == Stratum::Verified
                && is_type_error(cx, ty, &value)
                && !object_world_guard_blind(in_descent, ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    Some(&provenance),
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // A variable bound to a proven object (ADR-0036 heap): object-vs-type
            // definite-No (ADR-0043 stage 3). `new`/enum/const args are the direct
            // pass's job; this covers the env/heap-dependent `$x = new Foo(); f($x)`.
            // Guard-blind inside a descent (see `object_world_guard_blind`).
            if !native_fired
                && !poisoned
                && !in_descent
                && let ArgValue::Var(name) = &arg.value
                && store.is_exact(name) // No-side needs exactness (audit G1)
                && let Some(class) = store.class_of(name)
                && cx.object_is_type_error(ty, class)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &ArgValue::Var(name.clone()),
                    Some(&format!("holds a {}", simple_class(class))),
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
        }

        // Only the propagation-carrier arg kinds (`$var`/`call()`) are the
        // propagation pass's to phpdoc-check; literal/array/`new` args are owned
        // by the direct pass (no double-report across the two passes).
        if !native_fired
            && matches!(arg.value, ArgValue::Var(_) | ArgValue::Call(..))
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                site.file,
                decl.span.start,
                &decl.name,
                arg.span.start,
                &arg.value,
                env,
                store,
                poisoned,
                in_descent,
                out,
            );
        }
    }
}

/// Attempt an interprocedural binding descent into a same-project function.
fn try_descend_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(site) = cx.resolve_user_fn(call) else { return };
    let decl = cx.fn_decl(site);
    let Some((callee_file, callee_scope)) = cx.fn_scope(site) else { return };
    descend(
        cx,
        folder,
        &decl.params,
        callee_file,
        callee_scope,
        &decl.fqn,
        &decl.name,
        None,
        call,
        &[],
        env,
        poisoned,
        descent,
        out,
    );
}

/// Handle a `$fn(...)` variable call (ADR-0033): resolve the callee variable
/// against the env. A proven closure value → argument check against the closure's
/// params + binding descent into the closure scope (with the capture snapshot
/// seeded); a proven `Singleton(Str)` → resolve as a function name through the
/// normal function path. An unresolved `$fn` does nothing (opaque; the effects
/// pass taints exhaustiveness separately).
#[allow(clippy::too_many_arguments)]
fn handle_var_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if scope.poisoned || !call.positional_only {
        return;
    }
    let Some(known) = env.get(name) else { return };

    // 1. Proven closure value → check args + descend into the closure scope.
    if let Some(cv) = &known.closure {
        match &cv.target {
            ClosureTarget::Scope(def_offset) => {
                let Some(callee_scope) = cx.closure_scope(*def_offset) else { return };
                // Argument type check at the `$fn(...)` site (mirrors the direct /
                // propagated check for named calls, which never see a variable call).
                check_callable_args(
                    cx, folder, scope.poisoned, descent.is_some(), &callee_scope.params, "closure",
                    call, env, out,
                );
                let display = format!("closure (defined on line {})", cv.def_line);
                descend(
                    cx,
                    folder,
                    &callee_scope.params,
                    cx.cur,
                    callee_scope,
                    &format!("closure@{def_offset}"),
                    &display,
                    None,
                    call,
                    &cv.captures,
                    env,
                    scope.poisoned,
                    descent,
                    out,
                );
            }
            ClosureTarget::Named(nameref) => {
                dispatch_named_callable(cx, folder, scope.poisoned, nameref, call, env, descent, out);
            }
        }
        return;
    }

    // 2. Proven string value → resolve as a function name (`$fn = 'strtolower';`).
    if let Some(ArgValue::Str(s)) = known.singleton() {
        let nameref = NameRef { raw: s, kind: RefKind::Unqualified, offset: call.span.start };
        dispatch_named_callable(cx, folder, scope.poisoned, &nameref, call, env, descent, out);
    }
}

/// Dispatch a `$fn(...)` call whose target is a named free function (a first-class
/// callable or a proven string callable, ADR-0033): argument type check against the
/// resolved function's params, then normal binding descent.
#[allow(clippy::too_many_arguments)]
fn dispatch_named_callable(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    nameref: &NameRef,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let synth = synth_function_call(call, nameref);
    if let Some(site) = cx.resolve_user_fn(&synth) {
        let decl = cx.fn_decl(site);
        check_callable_args(
            cx, folder, poisoned, descent.is_some(), &decl.params, &decl.name, call, env, out,
        );
    }
    try_descend_function(cx, folder, &synth, env, poisoned, descent, out);
}

/// A synthetic named-function [`CallExpr`] from a `$fn(...)` variable call and a
/// resolved function reference, so the normal function-resolution/descent path can
/// consume it (ADR-0033 first-class-callable / string-callable dispatch).
fn synth_function_call(call: &CallExpr, nameref: &NameRef) -> CallExpr {
    CallExpr {
        callee: Some(nameref.raw.clone()),
        callee_ref: Some(nameref.clone()),
        receiver: Callee::Function(nameref.raw.clone()),
        args: call.args.clone(),
        named_args: call.named_args.clone(),
        has_spread: call.has_spread,
        positional_only: call.positional_only,
        span: call.span,
    }
}

/// Argument type check for a `$fn(...)` call at the call site (ADR-0033): each
/// proven argument (literal, or resolved `$var`/fold) is checked against the
/// callable's corresponding native param type, firing `type.argument-mismatch` on
/// a proven coercive TypeError — the variable-call analogue of the direct /
/// propagated check (which never see a variable call). `display` names the callee
/// in the message (`"closure"` or the resolved function name).
#[allow(clippy::too_many_arguments)]
fn check_callable_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    in_descent: bool,
    params: &[Param],
    display: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }
        let Some(ty) = param.ty.as_ref() else { continue };
        // Resolve the argument to a proven value (literal directly; `$var`/fold via
        // the env). Provenance names the variable/fold source where applicable.
        let resolved: Option<(ArgValue, Option<String>)> = match &arg.value {
            v if v.is_literal() => Some((v.clone(), None)),
            ArgValue::Var(vn) if !poisoned => env.get(vn).and_then(|k| {
                let v = k.singleton()?;
                let prov = match &k.bound {
                    Some(b) => format!("from ${vn}, {b}"),
                    None => format!("from ${vn}, assigned at line {}", k.line),
                };
                Some((v, Some(prov)))
            }),
            ArgValue::Call(cn, cargs) => cx
                .try_fold(cn, cargs, folder)
                .map(|(lit, prov)| (lit, Some(prov))),
            // A proven object (`new` / enum case) or resolved class constant
            // (ADR-0043 stage 3); env-free, `self`/`parent` unavailable here.
            _ => cx.resolve_static_value(&arg.value, None).map(|v| (v, None)),
        };
        // Proof-layer consumption rule (ADR-0052 §5): silent on an `Asserted`
        // premise (no store here — a prop-fetch arg never resolves to a fire).
        if let Some((value, provenance)) = resolved
            && value_stratum(&arg.value, env, None) == Stratum::Verified
            && is_type_error(cx, ty, &value)
            && !object_world_guard_blind(in_descent, ty, &value)
        {
            out.push(cx.diagnostic(
                arg.span.start,
                &value,
                provenance.as_deref(),
                display,
                &param.name,
                ty,
            ));
        }
    }
}

/// Interprocedural argument-binding descent into a resolved callee body.
#[allow(clippy::too_many_arguments)]
fn descend(
    cx: &Cx,
    folder: &mut dyn Folder,
    params: &[Param],
    callee_file: usize,
    callee_scope: &Scope,
    key_name: &str,
    display_name: &str,
    body_this_exact: Option<String>,
    call: &CallExpr,
    captures: &[(String, Fact)],
    env: &HashMap<String, Known>,
    poisoned: bool,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if callee_scope.poisoned {
        return;
    }

    // Resolve each positional argument to a literal and try to bind it (using
    // the *caller's* env, strict mode, and folding). Each binding carries the arg's
    // trust stratum (ADR-0052 §5): the seeded callee param inherits it, so an
    // `Asserted` argument narrows into the descent without laundering to `Verified`.
    let mut bound: Vec<(String, ArgValue, Stratum)> = Vec::new();
    let mut render_args: Vec<ArgValue> = Vec::new();
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = params.get(i) else { break };
        if param.variadic {
            break;
        }
        let Some(value) = cx.resolve_literal(&arg.value, env, poisoned, folder) else {
            continue;
        };
        let strat = value_stratum(&arg.value, env, None);
        render_args.push(value.clone());
        if param.by_ref {
            return;
        }
        let Some(ty) = param.ty.as_ref() else {
            bound.push((param.name.clone(), value, strat));
            continue;
        };
        match coerce_into_param(cx, ty, &value) {
            Some(coerced) => bound.push((param.name.clone(), coerced, strat)),
            None => return,
        }
    }

    // A closure with captures descends even with no bound args (the capture
    // snapshot drives the body); a plain function needs at least one bound arg.
    if bound.is_empty() && captures.is_empty() {
        return;
    }

    // The binding key incorporates the captured snapshot so two calls of the same
    // closure with different snapshots memoize distinctly (adversarial #1). The
    // stratum is a trust attribute, not an identity — it is excluded from the key.
    let mut key_binding: Vec<(String, ArgValue)> =
        bound.iter().map(|(n, v, _)| (n.clone(), v.clone())).collect();
    for (name, fact) in captures {
        key_binding.push((format!("use:{name}"), arg_of_fact_key(fact)));
    }
    key_binding.sort_by(|a, b| a.0.cmp(&b.0));
    let key: BindingKey = (key_name.to_owned(), key_binding);

    // Provenance names the *first* binding site; a nested descent inherits it.
    // When the call crosses files, the site names the caller's file.
    let cross = cx.cur != callee_file;
    let new_provenance;
    let (provenance, next_depth): (&str, usize) = match &descent {
        Some(d) => (d.provenance, d.depth + 1),
        None => {
            let line = cx.tree().position(call.span.start).line;
            let render = render_call(display_name, &render_args);
            new_provenance = if cross {
                format!("bound at {render} call at {} line {line}", cx.path())
            } else {
                format!("bound at {render} call on line {line}")
            };
            (&new_provenance, 1)
        }
    };

    if next_depth > MAX_BINDING_DEPTH {
        return;
    }

    // Bound params are always resolved literals/arrays, so `singleton_fact`
    // succeeds; a value that somehow fails conversion is simply left unbound
    // (the callee param stays unknown — sound).
    let mut bound_env: HashMap<String, Known> = bound
        .into_iter()
        .filter_map(|(name, value, strat)| {
            singleton_fact(&value)
                .map(|fact| (name, Known::value_strat(fact, 0, Some(provenance.to_owned()), strat)))
        })
        .collect();
    // Closure captures (ADR-0033): the by-value snapshot seeds the initial env,
    // UNDER the param bindings (a param of the same name shadows a capture, PHP
    // semantics — `use ($x)` is ignored if `$x` is also a parameter).
    for (name, fact) in captures {
        bound_env.entry(name.clone()).or_insert_with(|| {
            Known::value(fact.clone(), 0, Some(provenance.to_owned()))
        });
    }

    let child_cx = cx.at(callee_file);
    match descent {
        Some(d) => {
            if d.stack.contains(&key) || d.memo.contains(&key) {
                return;
            }
            d.stack.push(key.clone());
            let child = Descent { provenance, depth: next_depth, stack: d.stack, memo: d.memo };
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                Store::default(),
                body_this_exact,
                Some(child),
                None,
                None,
                out,
            );
            d.stack.pop();
            d.memo.insert(key);
        }
        None => {
            let mut stack: Vec<BindingKey> = vec![key.clone()];
            let mut memo: HashSet<BindingKey> = HashSet::new();
            let child = Descent { provenance, depth: next_depth, stack: &mut stack, memo: &mut memo };
            analyze_scope(
                &child_cx,
                folder,
                callee_scope,
                bound_env,
                Store::default(),
                body_this_exact,
                Some(child),
                None,
                None,
                out,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Class-world method resolution (ADR-0001 sound dispatch), project-wide.
// ---------------------------------------------------------------------------

/// A method resolved through a project inheritance chain.
struct ResolvedMethod<'a> {
    method: &'a MethodDecl,
    declaring_class: &'a ClassDecl,
    class_file: usize,
}

/// The outcome of walking a class's inheritance chain for a method.
enum Resolution<'a> {
    Found(ResolvedMethod<'a>),
    NotFoundChainComplete,
    Unknown,
}

/// Walk `start_fqn`'s project inheritance chain for a concrete `method`.
fn resolve_in_chain<'a>(cx: &Cx<'a>, start_fqn: &str, method: &str) -> Resolution<'a> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Resolution::Unknown;
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Resolution::Unknown; // chain leaves the project
        };
        if cd.uses_traits {
            return Resolution::Unknown;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            return if m.is_abstract {
                Resolution::Unknown
            } else {
                Resolution::Found(ResolvedMethod { method: m, declaring_class: cd, class_file: cfile })
            };
        }
        match &cd.parent {
            None => return Resolution::NotFoundChainComplete,
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// A resolved call target.
struct CallTarget<'a> {
    method: &'a MethodDecl,
    declaring_class: &'a ClassDecl,
    class_file: usize,
    this_exact: Option<String>,
}

/// Resolve a method/static/constructor `receiver` to a project target.
fn resolve_call_target<'a>(
    cx: &Cx<'a>,
    receiver: &Callee,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<CallTarget<'a>> {
    match receiver {
        Callee::Construct { class } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, "__construct", enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::New(class), method, .. } => {
            let fqn = cx.class_fqn(class);
            resolve_exact(cx, &fqn, method, enclosing_class, Some(fqn.clone()))
        }
        Callee::Method { receiver: Receiver::Var(v), method, .. } => {
            if poisoned {
                return None;
            }
            let obj = store.obj_of(v)?;
            let class = obj.class.clone();
            if obj.class_exact {
                // An allocation-proven receiver (`$x = new Foo(); $x->m()`) dispatches
                // exactly — the landed precision.
                resolve_exact(cx, &class, method, enclosing_class, Some(class.clone()))
            } else {
                // A lower-bound receiver — a laundered `$this` alias (`$u = $this`) or
                // `clone $this` — is NOT exact (audit G1): fall back to the same
                // final/private override guard `Receiver::This` uses, so an overridable
                // method on it never resolves to the enclosing declaration.
                resolve_guarded(cx, &class, method, enclosing_class)
            }
        }
        Callee::Method { receiver: Receiver::This, method, .. } => {
            let enclosing = enclosing_class?;
            match this_exact {
                Some(exact) => resolve_exact(cx, exact, method, enclosing_class, Some(exact.to_owned())),
                None => resolve_guarded(cx, enclosing, method, enclosing_class),
            }
        }
        Callee::Static { class: StaticClass::SelfKw, method } => {
            let enclosing = enclosing_class?;
            resolve_guarded(cx, enclosing, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Parent, method } => {
            let parent = cx.parent_fqn(enclosing_class?)?;
            resolve_static_named(cx, &parent, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            let fqn = cx.class_fqn(name);
            resolve_static_named(cx, &fqn, method, enclosing_class)
        }
        Callee::Static { class: StaticClass::Static, .. } => None,
        Callee::Function(_) | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// Resolve an exact-receiver instance/constructor call (no override guard).
fn resolve_exact<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
    this_exact: Option<String>,
) -> Option<CallTarget<'a>> {
    match resolve_in_chain(cx, class, method) {
        Resolution::Found(r) if !private_blocked(&r, enclosing_class) => Some(CallTarget {
            method: r.method,
            declaring_class: r.declaring_class,
            class_file: r.class_file,
            this_exact,
        }),
        _ => None,
    }
}

/// Resolve a `$this->`/`self::` call under the override guard.
fn resolve_guarded<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    let declaring_final = r.declaring_class.is_final;
    let final_or_private =
        r.method.is_final || r.method.visibility == Visibility::Private || declaring_final;
    if !final_or_private {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
    })
}

/// Resolve an explicit `Foo::m()` / `parent::m()` static call (exact).
fn resolve_static_named<'a>(
    cx: &Cx<'a>,
    class: &str,
    method: &str,
    enclosing_class: Option<&str>,
) -> Option<CallTarget<'a>> {
    let Resolution::Found(r) = resolve_in_chain(cx, class, method) else { return None };
    if private_blocked(&r, enclosing_class) {
        return None;
    }
    if !r.method.is_static && enclosing_class.is_none() {
        return None;
    }
    Some(CallTarget {
        method: r.method,
        declaring_class: r.declaring_class,
        class_file: r.class_file,
        this_exact: None,
    })
}

/// Whether a resolved `private` method is invisible at the call site.
fn private_blocked(r: &ResolvedMethod, enclosing_class: Option<&str>) -> bool {
    r.method.visibility == Visibility::Private
        && !enclosing_class.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
}

// ---------------------------------------------------------------------------
// The finding-breadth flagship: `call.undefined-method` (ADR-0049 §4 / S2).
//
// An *absence* proof — fire only under complete closure over every place a method
// could hide. The ladder (ADR-0049 §4 + amendments A1/A2/A3/A9) is applied leg by
// leg; ANY doubt is silence (the zero-FP identity, ADR-0013). The cheap textual
// legs run first so the sidecar homonym IPC (A2ii) is reached only for a chain
// that already survived every local check.
// ---------------------------------------------------------------------------

/// Which magic fallback swallows an otherwise-undefined call, and the PHP phrasing
/// of the call kind — instance (`$recv->m()`, `__call`) vs static (`C::m()`,
/// `__callStatic`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum UndefKind {
    Instance,
    Static,
}

impl UndefKind {
    /// The magic method whose presence anywhere in the chain makes the call defined.
    fn magic(self) -> &'static str {
        match self {
            UndefKind::Instance => "__call",
            UndefKind::Static => "__callStatic",
        }
    }
}

/// The receiver a `call.undefined-method` claim can rest on, after legs (a)/(l).
/// Carries the *exact* receiver class FQN and the call kind. `None` from the
/// resolver means the receiver is out of scope for S2 (silence): `$this` (A1
/// membership), an inexact/lower-bound variable, a nullsafe `?->`, a first-class
/// callable, `self`/`static`/`parent`, or any dynamic form.
fn undefined_method_receiver(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<(String, String, UndefKind)> {
    // Leg (l): the first-class-callable form `$v->m(...)` / `C::m(...)` lowers to an
    // arg-less non-positional call — it builds a Closure, it does not invoke — so it
    // is never an undefined-method site. (A named/spread call keeps `args` non-empty
    // and stays eligible: method existence is argument-shape-independent.)
    if !call.positional_only && call.args.is_empty() {
        return None;
    }
    match &call.receiver {
        Callee::Method { receiver, method, nullsafe } => {
            if *nullsafe {
                return None; // leg (l): `?->` excluded in v1.
            }
            let class = match receiver {
                // Leg (a)/A1: an exact-class receiver only. `new Foo()` is exact by
                // construction; a `$var` is exact only when the heap says so (a
                // `new`/clone-of-exact allocation, never a `$this` lower bound or a
                // laundered alias). `class_exact` is set solely by Verified-origin
                // allocation sites, so the N2 stratum requirement on the receiver
                // identity holds by construction.
                Receiver::New(name) => cx.class_fqn(name),
                Receiver::Var(v) => {
                    if poisoned {
                        return None;
                    }
                    let obj = store.obj_of(v)?;
                    if !obj.class_exact {
                        return None; // lower bound → S6's lane, not ours.
                    }
                    obj.class.clone()
                }
                // A1: `$this` is a membership fact, never exactness — silent in S2.
                Receiver::This => return None,
            };
            Some((class, method.clone(), UndefKind::Instance))
        }
        Callee::Static { class, method } => match class {
            // Textual, exact — no receiver proof needed. `self`/`static`/`parent`
            // stay unlowered and silent (ADR-0043 §1).
            StaticClass::Named(name) => {
                Some((cx.class_fqn(name), method.clone(), UndefKind::Static))
            }
            StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static => None,
        },
        // `new C()` (no method), `$fn()`, dynamic method names → not our sites.
        Callee::Function(_)
        | Callee::Construct { .. }
        | Callee::DynamicVar(_)
        | Callee::Dynamic => None,
    }
}

/// The outcome of walking `start_fqn`'s ancestor chain for `method` under the
/// ADR-0049 §4 closure discipline (the C1 completeness standard).
enum ChainWalk {
    /// The method is absent from a fully-enumerated, obstacle-free chain: fire
    /// eligible. Carries the ordered simple class names (for the message), the
    /// ordered chain FQNs (for the A2ii homonym leg), and whether any node was
    /// declared conditionally (A2i — re-dams the claim).
    Absent { simple_chain: Vec<String>, fqns: Vec<String>, any_conditional: bool },
    /// An obstacle taints closure anywhere on the chain, or the method is present:
    /// silence (the FP-safe verdict).
    Silent,
}

/// Walk `start_fqn`'s parent chain proving the method's *absence* under complete
/// enumeration (ADR-0049 §4 (b)–(f), (j); A2i/A2iii). Interfaces are not walked:
/// a PHP interface never carries a method body, so it can never *define* the
/// method — only the `extends` (class-parent) chain can, exactly as the landed
/// [`resolve_in_chain`] does. Any of these taints closure ⇒ `Silent`:
/// unresolvable/`Ambiguous`/builtin ancestor (leg b/f/i), a trait name or a
/// `uses_traits` node (leg e), an `is_enum` node (leg j / A3), the magic fallback
/// (`__call`/`__callStatic`, leg d), a cycle (leg b), or the method being present
/// (not undefined).
fn enumerate_method_chain(cx: &Cx, start_fqn: &str, method: &str, kind: UndefKind) -> ChainWalk {
    let magic = kind.magic();
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut simple_chain: Vec<String> = Vec::new();
    let mut fqns: Vec<String> = Vec::new();
    let mut any_conditional = false;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return ChainWalk::Silent; // cycle — closure cannot terminate soundly.
        }
        // Leg (b)/(f)/(i): every ancestor edge must resolve to a UNIQUE project
        // declaration. `find_class` returns `None` for `Absent` (a builtin/vendor-
        // unresolved ancestor — leg f, awaiting the reflect method surface, M2) and
        // for `Ambiguous` (a duplicate FQN or an alias/decl collision — leg i).
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return ChainWalk::Silent;
        };
        // Leg (j)/A3: enum methods are not lowered, so an enum chain would look
        // method-empty — Unknown until enum method lowering lands.
        if cd.is_enum {
            return ChainWalk::Silent;
        }
        // A trait name in the class-like index carries no lowered members (S1): it
        // would falsely read as "method absent". Never a method holder here.
        if cd.is_trait {
            return ChainWalk::Silent;
        }
        // Leg (e): a trait use adds methods the is-a oracle rightly ignores for
        // ancestry — Unknown until trait flattening (per-node, like resolve_in_chain).
        if cd.uses_traits {
            return ChainWalk::Silent;
        }
        // Leg (d): a magic fallback anywhere swallows the name — no error at runtime.
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(magic)) {
            return ChainWalk::Silent;
        }
        // The method is present (case-insensitively) — including an abstract
        // declaration: it is defined, so the call is not undefined.
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method)) {
            return ChainWalk::Silent;
        }
        simple_chain.push(cd.name.clone());
        fqns.push(cur.clone());
        if cd.conditional {
            any_conditional = true;
        }
        match &cd.parent {
            None => {
                return ChainWalk::Absent { simple_chain, fqns, any_conditional };
            }
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// Run the full ADR-0049 §4 ladder for one method/static call and emit
/// `call.undefined-method` iff **every** leg holds. Called only from the plain
/// per-scope pass (`descent.is_none()`) so a site is judged once, never re-emitted
/// under an interprocedural descent.
fn check_undefined_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    // Legs (a)/(l): identify an exact receiver and the call kind, or bail.
    let Some((class_fqn, method, kind)) = undefined_method_receiver(cx, call, store, poisoned)
    else {
        return;
    };
    // Guard-respect leg (ADR-0049 §4): a positive `method_exists($this-or-C, 'm')`
    // guard dominating this site vouched `C::m` — the programmer supplied existence
    // evidence, so stay silent even if the chain enumeration below would reach Absent
    // (a `Maybe`-verdict guard whose branch we are walking live). Exact-textual match
    // on the RESOLVED class + method (case-insensitive).
    if store.vouches_method(&class_fqn, &method) {
        return;
    }
    // A9 (global) + A2ii's honest consequence: without a live sidecar, or with a
    // monkey-patch extension loaded, the id is entirely silent (checked once, cached).
    if !folder.absence_family_available() {
        return;
    }
    // Legs (b)–(f), (j), A2i/A2iii: textual closure over the ancestor chain.
    let ChainWalk::Absent { simple_chain, fqns, any_conditional } =
        enumerate_method_chain(cx, &class_fqn, &method, kind)
    else {
        return;
    };
    // Leg A2i: a conditional declaration in the chain re-dams the claim — fire only
    // when the whole-universe dam is clear (no vouch machinery in this slice).
    if any_conditional && !cx.dam.is_clear() {
        return;
    }
    // Leg (h)/A2ii: every chain FQN must be answered NOT-present by the boot-surface
    // existence oracle. A homonym (`Some(true)`) or an unanswerable query (`None` —
    // a mid-run sidecar failure) is silence.
    for fqn in &fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }

    // Every leg holds — a proven `Error: Call to undefined method C::m()`.
    let pos = cx.tree().position(call.span.start);
    let simple_class = simple_chain.first().map_or(class_fqn.as_str(), String::as_str);
    let chain_render = simple_chain.join(" → ");
    let message = format!(
        "call to undefined method {simple_class}::{method}() — hierarchy fully enumerated ({chain_render}), no {}",
        kind.magic(),
    );
    out.push(Diagnostic {
        id: CALL_UNDEFINED_METHOD_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
    });
}

// ---------------------------------------------------------------------------
// Arity: `call.too-few-arguments` / `call.unknown-named-argument`
// (ADR-0049 §6 / S5 — the userland arms).
//
// The verified PHP 8.5 table is ASYMMETRIC (every row `php -r`-checked): too few
// positional/named arguments to a userland target is always a fatal
// `ArgumentCountError`; too MANY to a non-variadic runs clean (extras ignored) and
// is NEVER a finding (the ADR-0002 consequence pattern, whatever PHPStan reports —
// `call.too-many-arguments` stays REGISTERED_NOT_YET_EMITTED for the internal
// slice, M2); an unknown named argument to a non-variadic is a fatal `Error`, while
// a variadic silently collects it (`fv(x: 1)` → `{"x":1}`). A named argument that
// overwrites a positional (`f(1, a: 5)`) is a fatal `Error` too — a DEFERRED id, so
// it is a *silence* leg here. The verified runtime precedence is
// **overwrite ≻ unknown-named ≻ too-few** (`f(z: 9)` on `f($a, $b)` throws the
// unknown-name `Error`, not `ArgumentCountError`), and the checks below honor it so
// the emitted id never misnames the runtime consequence. Internal (builtin) targets
// take their arity from sidecar reflection and ship with the reflect slice (M2).
//
// Provability rests entirely on the RESOLVED TARGET's ground-truth signature:
//   - functions: a uniquely-indexed userland function (ADR-0049 A2 legs — not
//     Ambiguous, not builtin-shadowed; a conditional declaration re-dams the claim;
//     the boot-surface function-homonym is cleared via the sidecar);
//   - methods/constructors/statics: ONLY under a proven-EXACT receiver. The
//     declared-receiver variant is UNSOUND — an override may ADD optional
//     parameters (`P::m(int $a)` vs `Q::m($a = 0, $b = 0)`), so `$p->m()` on a
//     declared `P` holding a `Q` satisfies the runtime contract and runs; a finding
//     there is a false positive, REFUSED outright (never deferred to a contract
//     lane, unlike `phpdoc.undefined-method`). Exactness reuses S2's gate: `new`,
//     a `class_exact` heap object, or a textual `Class::` static; `$this`
//     (membership, A1), `self::`/`static::`/`parent::`, `?->`, and every dynamic
//     form are silent.
// Call-site conditions: no argument unpacking (`...` ⇒ count unproven; counting
// proven Singleton arrays is deferred); the first-class-callable `f(...)` is not a
// call; named binding is resolved against the target's parameter names
// case-SENSITIVELY, exactly as PHP binds them.
// ---------------------------------------------------------------------------

/// A resolved arity target: the callee's parameter list (the ground-truth
/// signature) and its PHP display name for the message (`format`, `Order::pay`,
/// `Order::__construct`).
struct ArityTarget<'a> {
    params: &'a [Param],
    display: String,
}

/// The number of **required** parameters (ADR-0049 §6): the 1-based index of the
/// last parameter that is neither variadic nor default-valued. Matches PHP 8.5's
/// `ReflectionFunctionAbstract::getNumberOfRequiredParameters`, including the
/// deprecated "optional parameter declared before a required one is implicitly
/// required" shape (`f($a = 1, $b)` ⇒ 2, `php -r`-verified). A variadic is never
/// required; by-ref and promoted parameters are required exactly like any other
/// (both `php -r`-verified).
fn required_param_count(params: &[Param]) -> usize {
    let mut required = 0;
    for (i, p) in params.iter().enumerate() {
        if !p.variadic && !p.has_default {
            required = i + 1;
        }
    }
    required
}

/// Resolve the exact-receiver class + method name for an arity method/static/
/// constructor call, or `None` when the receiver is not proven exact (S2's gate,
/// plus constructors). Constructors and textual `Class::` statics are exact by
/// construction; a `$var` receiver is exact only under a `class_exact` heap fact.
/// The `bool` is whether this is a **static** (`Class::m()`) call — a static call
/// to a NON-static method raises `Error: Non-static method … cannot be called
/// statically` *before* any `ArgumentCountError` (`php -r`-verified), so the caller
/// silences that shape rather than misnaming the consequence.
fn arity_method_receiver(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<(String, String, bool)> {
    match &call.receiver {
        Callee::Construct { class } => Some((cx.class_fqn(class), "__construct".to_owned(), false)),
        Callee::Method { receiver, method, nullsafe } => {
            if *nullsafe {
                return None; // `?->` excluded in v1 (S2 leg (l)).
            }
            let class = match receiver {
                Receiver::New(name) => cx.class_fqn(name),
                Receiver::Var(v) => {
                    if poisoned {
                        return None;
                    }
                    let obj = store.obj_of(v)?;
                    if !obj.class_exact {
                        return None; // lower bound → the refused declared-receiver lane.
                    }
                    obj.class.clone()
                }
                // A1: `$this` is a membership fact, never exactness — silent.
                Receiver::This => return None,
            };
            Some((class, method.clone(), false))
        }
        Callee::Static { class, method } => match class {
            StaticClass::Named(name) => Some((cx.class_fqn(name), method.clone(), true)),
            StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static => None,
        },
        Callee::Function(_) | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// Walk `start_fqn`'s exact-receiver chain resolving `method` to its declaring
/// [`MethodDecl`] under S2's closure discipline. Returns the method, its declaring
/// class's simple name, the ordered traversed FQNs (for the A2ii homonym leg), and
/// whether any traversed class was declared conditionally (A2i). `None` on any
/// obstacle: an unresolvable/`Ambiguous`/absent class, an enum (A3 — methods not
/// lowered), a trait name or a `uses_traits` class (a trait could shadow the method
/// with a different signature), a cycle, an **abstract** or **non-public** resolved
/// method (a protected/private method may route to `__call` or raise a distinct
/// visibility `Error` — not an `ArgumentCountError`), or the method being absent
/// from the whole chain (that is S2's job, not arity's).
fn walk_arity_chain<'a>(
    cx: &Cx<'a>,
    start_fqn: &str,
    method: &str,
) -> Option<(&'a MethodDecl, String, Vec<String>, bool)> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut traversed: Vec<String> = Vec::new();
    let mut any_conditional = false;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None; // cycle — closure cannot terminate soundly.
        }
        let (cfile, cd) = cx.find_class(&cur)?; // unique project class, or bust.
        if cd.is_enum || cd.is_trait || cd.uses_traits {
            return None;
        }
        traversed.push(cur.clone());
        if cd.conditional {
            any_conditional = true;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            if m.is_abstract || m.visibility != Visibility::Public {
                return None;
            }
            return Some((m, cd.name.clone(), traversed, any_conditional));
        }
        // A `None` parent ends the chain: the method is absent from the whole chain
        // — that is S2's `call.undefined-method`, never arity's id.
        cur = cx.units[cfile].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// Resolve a userland **function** call to its arity target (ADR-0049 §6 / A2
/// legs). Cheap textual resolution first, then the sidecar-backed legs.
fn resolve_arity_function<'a>(
    cx: &Cx<'a>,
    folder: &mut dyn Folder,
    call: &CallExpr,
) -> Option<ArityTarget<'a>> {
    let r = call.callee_ref.as_ref()?;
    // Unique userland function only — `Ambiguous` and builtin-shadowed both resolve
    // to `Unknown` (silent); a catalogued builtin is the internal slice (M2).
    let FnResolution::User(site) = cx.resolve_function(r) else {
        return None;
    };
    let decl = cx.fn_decl(site);
    // A9 + the A2ii homonym leg both require a live sidecar.
    if !folder.absence_family_available() {
        return None;
    }
    // A2i: a conditionally-declared function re-dams the claim.
    if decl.conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii: the resolved FQN must be answered NOT-present as a boot-surface
    // function (a homonym extension function may be the real runtime binding — the
    // `function_exists`-guarded polyfill shadowed by a loaded extension).
    match folder.boot_surface_function(&decl.fqn) {
        Some(false) => {}
        Some(true) | None => return None,
    }
    Some(ArityTarget { params: &decl.params, display: decl.name.clone() })
}

/// Resolve a method/static/constructor arity target under a proven-exact receiver
/// and S2's chain closure (ADR-0049 §6). Cheap textual legs first.
fn resolve_arity_method<'a>(
    cx: &Cx<'a>,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<ArityTarget<'a>> {
    let (start_fqn, method, is_static_call) = arity_method_receiver(cx, call, store, poisoned)?;
    // `new AbstractClass()` / `new SomeInterface()` raises `Error: Cannot
    // instantiate abstract class / interface` BEFORE any `ArgumentCountError`
    // (`php -r`-verified) — silence it (would misname the consequence).
    if let Callee::Construct { .. } = &call.receiver {
        let (_, start_cd) = cx.find_class(&start_fqn)?;
        if start_cd.is_abstract || start_cd.is_interface {
            return None;
        }
    }
    let (mdecl, declaring_name, traversed, any_conditional) =
        walk_arity_chain(cx, &start_fqn, &method)?;
    // A static call (`Class::m()`) to a NON-static method raises the non-static
    // `Error` before any `ArgumentCountError` — silence it (would misname).
    if is_static_call && !mdecl.is_static {
        return None;
    }
    // A9 + the A2ii homonym leg both require a live sidecar.
    if !folder.absence_family_available() {
        return None;
    }
    // A2i: a conditional class anywhere on the traversed chain re-dams the claim.
    if any_conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii: every traversed class must be boot-surface-absent as a class-like.
    for fqn in &traversed {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return None,
        }
    }
    let display = if method.eq_ignore_ascii_case("__construct") {
        format!("{declaring_name}::__construct")
    } else {
        format!("{declaring_name}::{}", mdecl.name)
    };
    Some(ArityTarget { params: &mdecl.params, display })
}

/// The finding half: given a resolved target, apply the ordered arity checks
/// (overwrite ≻ unknown-named ≻ too-few) to one call site, honoring the verified
/// runtime precedence so the emitted id never misnames the consequence.
fn emit_arity(cx: &Cx, call: &CallExpr, target: &ArityTarget, out: &mut Vec<Diagnostic>) {
    let params = target.params;
    // Shape gates. Unpacking (or a non-canonical order) leaves the count unproven.
    if call.has_spread {
        return;
    }
    let pos = call.args.len();
    let named = &call.named_args;
    // First-class-callable `f(...)` lowers to an arg-less non-positional call — not
    // a call for arity. (Any real call is `positional_only`, or has ≥1 arg.)
    if !call.positional_only && pos == 0 && named.is_empty() {
        return;
    }

    // Overwrite guard (verified precedence #1): a named argument targeting a
    // parameter already filled by a positional argument (`f(1, a: 5)`) raises the
    // DEFERRED overwrite `Error` — silence both of our ids so neither misclaims.
    let overwrite = named
        .iter()
        .any(|n| params.iter().position(|p| p.name == n.name).is_some_and(|i| i < pos));
    if overwrite {
        return;
    }

    let has_variadic = params.iter().any(|p| p.variadic);
    // unknown-named (verified precedence #2): a named argument matching no parameter
    // of a NON-variadic target is a fatal `Error`; a variadic silently collects it.
    // Parameter-name matching is case-SENSITIVE (`f(A: 1)` on `$a` is unknown).
    if !has_variadic
        && let Some(unknown) = named.iter().find(|n| !params.iter().any(|p| p.name == n.name))
    {
        let at = cx.tree().position(call.span.start);
        out.push(Diagnostic {
            id: CALL_UNKNOWN_NAMED_ARGUMENT_ID,
            facet: None,
            path: cx.path().to_owned(),
            line: at.line,
            column: at.column,
            message: format!(
                "unknown named argument ${} to {}() — no parameter ${}, provable Error",
                unknown.name, target.display, unknown.name,
            ),
        });
        return;
    }

    // too-few (verified precedence #3): a required parameter covered by neither a
    // positional argument (index < pos) nor a named argument of that name.
    let required = required_param_count(params);
    let uncovered =
        (0..required).any(|i| i >= pos && !named.iter().any(|n| n.name == params[i].name));
    if uncovered {
        let passed = pos + named.len();
        let at = cx.tree().position(call.span.start);
        out.push(Diagnostic {
            id: CALL_TOO_FEW_ARGUMENTS_ID,
            facet: None,
            path: cx.path().to_owned(),
            line: at.line,
            column: at.column,
            message: format!(
                "too few arguments to {}(): {passed} passed, {required} required — provable ArgumentCountError",
                target.display,
            ),
        });
    }
}

/// Run the full ADR-0049 §6 userland arity ladder for one call and emit
/// `call.too-few-arguments` / `call.unknown-named-argument` iff every leg holds.
/// Called only from the plain per-scope pass (`descent.is_none()`) so a site is
/// judged once, never re-emitted under an interprocedural descent.
fn check_arity(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    let target = match &call.receiver {
        Callee::Function(_) => resolve_arity_function(cx, folder, call),
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            resolve_arity_method(cx, folder, call, store, poisoned)
        }
        Callee::DynamicVar(_) | Callee::Dynamic => None,
    };
    let Some(target) = target else {
        return;
    };
    emit_arity(cx, call, &target, out);
}

// ---------------------------------------------------------------------------
// The declared-receiver lane: `phpdoc.undefined-method` (ADR-0049 §8 / S6).
//
// The **contract-layer** twin of `call.undefined-method`. Where S2 fires on a
// proven-exact receiver (`class_exact`), S6 fires on a receiver whose *declared*
// type — a phpdoc `@param User|Guest`, narrowed by branch analysis (N4) down to a
// surviving contract-arm list — provably lacks the method under a stricter ladder:
// "conditional is not enough" (§8), so each surviving arm must clear both the §4
// chain legs AND **descendant closure** (a subclass, incl. an `eval`-minted one,
// could satisfy the contract and define the method).
//
// **Disjointness from S2 (stated in code).** S2 owns `class_exact` receivers; S6
// requires the receiver be NOT exact — an inexact/lower-bound `$var` carrying a
// narrowed arm lane. A receiver is never judged by both ids: an exact object has no
// contract lane consulted here (the `is_exact` bail), and a lane-carrying var is
// never `class_exact`. The two lists (`ALL_EMITTABLE_IDS`) stay disjoint too.
//
// This id **accepts Asserted premises** (contract layer, ADR-0052 §5): the
// narrowed lane's arms may be `Asserted` (a `@param` refinement) — the finding is
// coherent at the min stratum. It still respects `absence_family_available` (A9
// monkey-patch silence + the A2ii homonym leg needs a live sidecar) and the A11
// version-skew demotion of descendant closure.
// ---------------------------------------------------------------------------

/// The `(receiver-var, method)` an S6 claim can rest on, or `None` when the call is
/// out of scope for the declared-receiver lane (silence). Only a plain
/// `$var->method(...)` qualifies: `?->` (leg l), static/`$this`/`new`/dynamic forms,
/// and the first-class-callable shape are excluded exactly as S2 excludes them.
fn phpdoc_undefined_method_receiver(call: &CallExpr) -> Option<(String, String)> {
    // Leg (l): the first-class-callable form builds a Closure, never a call.
    if !call.positional_only && call.args.is_empty() {
        return None;
    }
    match &call.receiver {
        Callee::Method { receiver: Receiver::Var(v), method, nullsafe: false } => {
            Some((v.clone(), method.clone()))
        }
        _ => None,
    }
}

/// The project-wide descendant enumeration of a union member (ADR-0049 §8 / A4).
enum DescendantClosure<'a> {
    /// The member is `final` (or an enum): no subclass can exist — extending it is
    /// fatal — so the arm is immune and needs no descendant scan and no dam.
    Immune,
    /// The member's descendant declarations are **completely enumerated** (no
    /// obstacle): every declared class either provably is-a the member (collected
    /// here) or provably is not, over declarations (both halves of an Ambiguous FQN)
    /// with alias-edge parent matching and interface edge kinds. A non-empty set
    /// still requires the dam clear (an `eval`-minted subclass) before it closes.
    Enumerated(Vec<(usize, &'a ClassDecl)>),
    /// Closure is tainted — Unknown ⇒ silence. An anonymous class could extend the
    /// member (invisible to the index), a candidate's is-a is Unknown (incomplete
    /// hierarchy), the member itself is Ambiguous/absent, or a catalog-backed verdict
    /// is demoted under a PHP-minor skew (A11).
    Obstacle,
}

/// Whether the specific declaration `cd` (in file `file`) provably **is-a**
/// `target` (lowercase FQN), walking its own inheritance edges directly rather than
/// through the deduped index — so an Ambiguous declaration still contributes as a
/// descendant (A4). A direct edge resolving to the *same index site* as `target`
/// counts as `Yes`, which folds literal `class_alias` edges into parent matching
/// (`class B extends LegacyName` with `class_alias('T', 'LegacyName')` makes B a
/// descendant of T). Deeper hops defer to the trinary [`Cx::is_a`] oracle, whose
/// `Unknown` (an unresolvable/uncatalogued ancestor) taints the enumeration.
fn decl_is_a(cx: &Cx, file: usize, cd: &ClassDecl, target: &str) -> IsA {
    let tree = &cx.units[file].tree;
    let arm_site = match cx.index.resolve_class(target) {
        Res::Unique(s) => Some(s),
        _ => None,
    };
    let mut edges: Vec<String> = Vec::new();
    if let Some(p) = &cd.parent {
        edges.push(tree.resolve_class_fqn(p));
    }
    for i in &cd.implements {
        edges.push(tree.resolve_class_fqn(i));
    }
    if cd.is_enum {
        edges.push("UnitEnum".to_owned());
        if cd.enum_backing.is_some() {
            edges.push("BackedEnum".to_owned());
        }
    }
    let mut any_unknown = false;
    for e in &edges {
        let en = e.trim_start_matches('\\');
        if en.eq_ignore_ascii_case(target) {
            return IsA::Yes;
        }
        // Alias-edge / site-identity parent match (A4): the direct edge resolves to
        // the same index site as the arm.
        if let (Some(a), Res::Unique(es)) = (arm_site, cx.index.resolve_class(en))
            && es == a
        {
            return IsA::Yes;
        }
        match cx.is_a(en, target) {
            IsA::Yes => return IsA::Yes,
            IsA::Unknown => any_unknown = true,
            IsA::No => {}
        }
    }
    if any_unknown { IsA::Unknown } else { IsA::No }
}

/// Enumerate the project-wide descendant set of a union member (ADR-0049 §8 / A4).
/// A query-style whole-universe function (ADR-0048): recomputed per run, no ordering
/// dependence. See [`DescendantClosure`].
fn descendant_closure<'a>(cx: &Cx<'a>, arm_fqn: &str) -> DescendantClosure<'a> {
    // The member must resolve Unique — an Ambiguous/absent member cannot be closed.
    let Some((_, arm_cd)) = cx.find_class(arm_fqn) else {
        return DescendantClosure::Obstacle;
    };
    // A `final` class or an enum has no subclass — extending it is fatal — so the
    // arm is immune (A9 already gated finality via `absence_family_available`).
    if arm_cd.is_final || arm_cd.is_enum {
        return DescendantClosure::Immune;
    }
    // A11: a PHP-minor skew can fake a catalog-backed is-a edge, so descendant
    // closure demotes to Unknown (blanket v1) — silence, never a wrong narrowing.
    if cx.a11_demote_catalog() {
        return DescendantClosure::Obstacle;
    }
    // A4 anonymous-class obstacle: an anon class is invisible to the index, so any
    // one whose extends/implements edge could reach the member taints closure.
    for unit in cx.units {
        for edge in unit.tree.anonymous_class_edges() {
            let refs = edge.parent.iter().chain(edge.implements.iter());
            for r in refs {
                let efqn = unit.tree.resolve_class_fqn(r);
                let en = efqn.trim_start_matches('\\');
                if en.eq_ignore_ascii_case(arm_fqn) {
                    return DescendantClosure::Obstacle;
                }
                // is-a-or-Unknown against the member ⇒ a possible invisible descendant.
                match cx.is_a(en, arm_fqn) {
                    IsA::Yes | IsA::Unknown => return DescendantClosure::Obstacle,
                    IsA::No => {}
                }
            }
        }
    }
    // Enumerate declared descendants over ALL declarations (not the deduped index —
    // both halves of an Ambiguous FQN count, A4). A single Unknown candidate (an
    // incompletely-enumerated hierarchy) taints the whole closure.
    let mut descendants: Vec<(usize, &'a ClassDecl)> = Vec::new();
    for (fi, unit) in cx.units.iter().enumerate() {
        for cd in unit.tree.classes() {
            if cd.fqn.eq_ignore_ascii_case(arm_fqn) {
                continue; // the member itself (Unique — resolved above).
            }
            match decl_is_a(cx, fi, cd, arm_fqn) {
                IsA::Yes => descendants.push((fi, cd)),
                IsA::Unknown => return DescendantClosure::Obstacle,
                IsA::No => {}
            }
        }
    }
    DescendantClosure::Enumerated(descendants)
}

/// Whether a descendant declaration could **introduce** `method` (or an obstacle
/// that hides it) below a member whose own chain already lacks it (ADR-0049 §8). A
/// descendant that declares the method, uses a trait, is an enum (A3, methods
/// unlowered), or carries `__call` is a witness that the runtime object — though
/// contract-typed as the member — may answer the call. Any such descendant makes
/// the absence claim fail (silence).
fn descendant_introduces_method(cd: &ClassDecl, method: &str) -> bool {
    cd.is_enum
        || cd.is_trait
        || cd.uses_traits
        || cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case("__call"))
        || cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method))
}

/// Run the full §8 ladder for one narrowed contract arm and return its display
/// simple-name when the method is **provably absent** across the arm's whole
/// hierarchy *and* its complete descendant set, or `None` when any leg fails
/// (silence). Instance calls only — the declared-receiver lane is `$var->m()`.
fn arm_provably_lacks_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    arm_fqn: &str,
    method: &str,
) -> Option<String> {
    // §4 chain closure over the arm's own ancestor chain (reuses S2's walk).
    let ChainWalk::Absent { simple_chain, fqns, any_conditional } =
        enumerate_method_chain(cx, arm_fqn, method, UndefKind::Instance)
    else {
        return None;
    };
    // A2i: a conditional declaration in the chain re-dams the claim.
    if any_conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii homonym: every chain FQN must be answered NOT-present by the boot surface.
    for fqn in &fqns {
        if folder.boot_surface_class_like(fqn) != Some(false) {
            return None;
        }
    }
    // Descendant closure (A4): the arm is final-immune, or its descendant set is
    // fully enumerated AND the dam is clear (an `eval`-minted subclass), and no
    // descendant introduces the method.
    match descendant_closure(cx, arm_fqn) {
        DescendantClosure::Immune => {}
        DescendantClosure::Obstacle => return None,
        DescendantClosure::Enumerated(descendants) => {
            if !cx.dam.is_clear() {
                return None; // eval could mint a subclass carrying the method.
            }
            for (_, dcd) in &descendants {
                if descendant_introduces_method(dcd, method) {
                    return None;
                }
                // A homonym descendant may be dead code shadowed by a loaded class.
                if folder.boot_surface_class_like(&dcd.fqn) != Some(false) {
                    return None;
                }
            }
        }
    }
    Some(simple_chain.first().cloned().unwrap_or_else(|| arm_fqn.to_owned()))
}

/// Run the ADR-0049 §8 ladder for one `$var->method()` and emit
/// `phpdoc.undefined-method` iff the receiver's narrowed contract-arm lane consists
/// entirely of class arms that **each** provably lack the method under descendant
/// closure. Contract layer — Asserted arms are coherent premises; any leg failure
/// on any arm is silence.
fn check_phpdoc_undefined_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    if poisoned {
        return;
    }
    let Some((var, method)) = phpdoc_undefined_method_receiver(call) else {
        return;
    };
    // Disjointness with S2: an exact receiver is S2's, never S6's.
    if store.is_exact(&var) {
        return;
    }
    // The narrowed declared-type arm lane (N4's accessor). No lane ⇒ nothing declared
    // to close over.
    let Some(arms) = store.contract_arms(&var) else {
        return;
    };
    if arms.is_empty() {
        return;
    }
    // Every surviving arm must be a class/interface arm: a scalar/array/null arm
    // means the runtime receiver may be a non-object, so a method-absence claim does
    // not hold (a different error, out of this lane) — silence.
    let mut class_fqns: Vec<String> = Vec::with_capacity(arms.len());
    for a in arms {
        match &a.ty {
            steins_contract::ContractTy::Class(f) => class_fqns.push(f.clone()),
            _ => return,
        }
    }
    // A9 (monkey-patch) + A2ii's honest consequence: without a live sidecar, or with
    // a runtime-redefinition extension loaded, the id is silent (checked once).
    if !folder.absence_family_available() {
        return;
    }
    // Every arm must provably lack the method under its closed ladder.
    let mut arm_names: Vec<String> = Vec::with_capacity(class_fqns.len());
    for f in &class_fqns {
        match arm_provably_lacks_method(cx, folder, f, &method) {
            Some(name) => arm_names.push(name),
            None => return, // any arm not provably-absent ⇒ silence.
        }
    }

    let pos = cx.tree().position(call.span.start);
    let arms_disp = arm_names.join("|");
    let message = format!(
        "call to undefined method {arms_disp}::{method}() — declared receiver ${var} narrowed to {{{arms_disp}}}, \
         hierarchy and descendants fully enumerated, no __call"
    );
    out.push(Diagnostic {
        id: PHPDOC_UNDEFINED_METHOD_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
    });
}

// ---------------------------------------------------------------------------
// The offset family: `offset.missing` / `offset.on-unsupported` (ADR-0049 §7 / S3).
//
// A value-domain absence proof: a read `$base[$key]` provably emits an `E_WARNING`
// because the whole container value is known (a Verified `Singleton`/all-array
// `OneOf`) and the key is provably absent, or the base is a proven non-offsetable
// scalar/null. Provability is value-domain evidence only (§7): `General`/`Refined`,
// objects, string bases, and any non-`Verified` fact (N2) are silent.
//
// **Read-context whitelist (A7).** The emitter is called ONLY from the whitelisted
// read positions — a plain assignment-RHS and a return operand — in the plain
// per-scope pass. Every silence context (`isset`/`??`/`array_key_exists`/`unset`,
// write lvalues, by-ref/unresolved-callee argument positions, array elements) never
// reaches here: `??` lowers its operand into an [`ArgValue::Coalesce`] (not a
// top-level `OffsetRead`), a write lvalue never lowers to an `Assign`/`Return`
// value, and `isset`/`unset`/`array_key_exists` are constructs/calls the lowering
// keeps out of these value slots entirely.
//
// v1 scope (deferred-with-comment, all safe silence): the Error-grade object case
// (needs the ArrayAccess is-a surface), the TypeError string-key-on-string case,
// string-base offset reads (in-range present / uninitialized-offset warning), the
// call-argument read position (the by-ref / unresolved-callee autovivification risk,
// A7), and the compound-assignment read half — none of which the current lowering
// carries into `Assign`/`Return` value slots.
// ---------------------------------------------------------------------------

/// Severity grade of an offset finding (ADR-0049 §7 verified table). The
/// `warning-handler` posture gates only [`Self::Warning`]; [`Self::Fatal`] (the
/// object `Error` / string-key `TypeError` cases) would emit under both — but those
/// cases are deferred in this slice, so every finding here is currently `Warning`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OffsetGrade {
    Warning,
    #[allow(dead_code)]
    Fatal,
}

/// Canonicalize a proven key [`Val`] to a domain array key (ADR-0049 A10), reusing
/// the SAME [`php_canonical_int_string`] primitive as the write/lowering side — so
/// `$a = [5 => 'x']; $a["5"]` resolves to the present key `5`, while `"05"`/`"+5"`
/// stay strings. `None` for an array key (an illegal offset type — a distinct
/// TypeError, out of scope here) or a non-finite float.
fn offset_key_of(v: &Val) -> Option<VKey> {
    match v {
        Val::Int(i) => Some(VKey::Int(*i)),
        Val::Bool(b) => Some(VKey::Int(i64::from(*b))),
        Val::Null => Some(VKey::Str(String::new())),
        #[allow(clippy::cast_possible_truncation)]
        Val::Float(f) if f.is_finite() => Some(VKey::Int(f.trunc() as i64)),
        Val::Str(s) => Some(match php_canonical_int_string(s) {
            Some(i) => VKey::Int(i),
            None => VKey::Str(s.clone()),
        }),
        Val::Float(_) | Val::Array(_) => None,
    }
}

/// The proven `Verified` value-domain fact for an offset-read operand (base or key),
/// or `None` when unproven, poisoned, or below the proof stratum (N2). A bare `Var`
/// reads the env (requiring `Verified`); a literal / fully-literal array resolves
/// directly (literals are `Verified` whole values). Every other form is unproven.
fn offset_operand_fact(arg: &ArgValue, env: &HashMap<String, Known>, poisoned: bool) -> Option<Fact> {
    match arg {
        ArgValue::Var(name) => {
            if poisoned {
                return None;
            }
            let k = env.get(name)?;
            (k.stratum == Stratum::Verified).then(|| k.fact.clone()).flatten()
        }
        _ => singleton_fact(arg),
    }
}

/// The PHP type word for an offset read on a proven non-offsetable scalar/null base
/// (verified PHP 8.5.8: `Trying to access array offset on null|int|float|true|false`).
/// `None` for a string base (offsetable — deferred) or any array.
fn unsupported_base_word(v: &Val) -> Option<&'static str> {
    match v {
        Val::Null => Some("null"),
        Val::Int(_) => Some("int"),
        Val::Float(_) => Some("float"),
        Val::Bool(true) => Some("true"),
        Val::Bool(false) => Some("false"),
        Val::Str(_) | Val::Array(_) => None,
    }
}

/// Render a canonical key in Steins' own phrasing (`0`, `'foo'`) for the evidence
/// clause, and in PHP's verbatim phrasing (`0`, `"foo"`) for the quoted consequence.
fn render_offset_key(k: &VKey) -> (String, String) {
    match k {
        VKey::Int(i) => (i.to_string(), i.to_string()),
        VKey::Str(s) => (format!("'{s}'"), format!("\"{s}\"")),
    }
}

/// Judge a single whitelisted offset read `base[key]` and emit at most one finding
/// (ADR-0049 §7 / S3). `span` locates the diagnostic (the enclosing statement).
#[allow(clippy::too_many_arguments)]
fn check_offset_read(
    cx: &Cx,
    folder: &mut dyn Folder,
    base: &ArgValue,
    key: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    // A9 (global): the whole family is silent without a live, monkey-patch-free
    // sidecar (checked once, cached) — the uniform absence-family availability gate.
    if !folder.absence_family_available() {
        return;
    }
    // Legs (b)/(e): the base must be a proven `Verified` whole value (N2). An object
    // base (fact `None` — object state lives in the heap, not the value domain) is
    // silent: the ArrayAccess-arbitrary-code / non-ArrayAccess-`Error` split is the
    // deferred object case.
    let Some(base_fact) = offset_operand_fact(base, env, poisoned) else {
        return;
    };

    // Case 1 — a proven non-offsetable scalar/null base (`offset.on-unsupported`,
    // warning-grade): the read warns regardless of the key, so no proven key is
    // required. Only a `Singleton` fires (a mixed/abstract base is silent).
    if let Fact::Singleton(v) = &base_fact
        && let Some(word) = unsupported_base_word(v)
    {
        emit_offset(
            cx,
            span,
            OFFSET_ON_UNSUPPORTED_ID,
            OffsetGrade::Warning,
            format!(
                "offset read on {} — provably {word}; reads null with \"Trying to access array offset on {word}\"",
                base.render(),
            ),
            out,
        );
        return;
    }

    // Case 2 — a container base (`offset.missing`, warning-grade): the key must be a
    // proven single value (leg (c)), canonicalized through the shared helper (A10).
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned) else {
        return;
    };
    let Some(canon) = offset_key_of(&key_val) else {
        return;
    };

    let (our_key, php_key) = render_offset_key(&canon);
    match &base_fact {
        // A single proven array (including `Singleton([])` from an `=== []` guard):
        // key absence is definite (leg (b)).
        Fact::Singleton(Val::Array(entries)) => {
            if !array_has_key(entries, &canon) {
                emit_offset(
                    cx,
                    span,
                    OFFSET_MISSING_ID,
                    OffsetGrade::Warning,
                    format!(
                        "offset {our_key} provably missing — {} is {} on this path; reads null with \"Undefined array key {php_key}\"",
                        base.render(),
                        render_val(&base_fact_val(&base_fact)),
                    ),
                    out,
                );
            }
        }
        // A `OneOf` fires only when EVERY member is an array and none carries the key
        // (leg (b), the join rule): any member with the key — or any non-array member
        // — is silence.
        Fact::OneOf(members) => {
            let all_arrays_missing = members.iter().all(|m| {
                matches!(m, Val::Array(entries) if !array_has_key(entries, &canon))
            });
            if all_arrays_missing {
                emit_offset(
                    cx,
                    span,
                    OFFSET_MISSING_ID,
                    OffsetGrade::Warning,
                    format!(
                        "offset {our_key} provably missing — {} is one of {} proven arrays, none carrying the key; reads null with \"Undefined array key {php_key}\"",
                        base.render(),
                        members.len(),
                    ),
                    out,
                );
            }
        }
        // `Refined`/`General` (no proven whole value), a string base, or anything
        // else: silent (§7 value-domain-only provability).
        _ => {}
    }
}

/// Whether a normalized array-entry list contains `key` (the read-side membership
/// check, over the already-canonical [`VKey`]s the domain stores).
fn array_has_key(entries: &[(VKey, Val)], key: &VKey) -> bool {
    entries.iter().any(|(k, _)| k == key)
}

/// The `Val` inside a `Singleton` fact (for rendering the container); a no-op clone
/// guarded by the caller having matched `Singleton`.
fn base_fact_val(f: &Fact) -> Val {
    match f {
        Fact::Singleton(v) => v.clone(),
        _ => Val::Null,
    }
}

/// Emit one offset finding, honoring the `warning-handler` posture (ADR-0049 §7):
/// under `"null"` (`!warning_handler_abort`) a warning-grade finding leaves the
/// proof surface and is not emitted; a `Fatal`-grade finding would emit under both
/// (none are produced in this slice).
fn emit_offset(
    cx: &Cx,
    span: Span,
    id: &'static str,
    grade: OffsetGrade,
    message: String,
    out: &mut Vec<Diagnostic>,
) {
    if grade == OffsetGrade::Warning && !cx.warning_handler_abort {
        return;
    }
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic { id, path: cx.path().to_owned(), line: pos.line, column: pos.column, message, facet: None });
}

/// Check + descend one method / static / constructor call.
#[allow(clippy::too_many_arguments)]
fn handle_method_call(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    descent: Option<&mut Descent<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if !call.positional_only {
        return;
    }
    let Some(target) =
        resolve_call_target(cx, &call.receiver, store, this_exact, enclosing_class, scope.poisoned)
    else {
        return;
    };

    let callee_name = format!("{}::{}", target.declaring_class.name, target.method.name);
    let class_templates = template_names_of(target.declaring_class.docblock.as_deref());
    check_method_args(
        cx,
        folder,
        target.method,
        target.class_file,
        &class_templates,
        &callee_name,
        call,
        env,
        store,
        scope.poisoned,
        descent.is_some(),
        out,
    );

    let Some(callee_scope) =
        cx.method_scope(target.class_file, &target.declaring_class.fqn, &target.method.name)
    else {
        return;
    };
    let display = display_of_call(&call.receiver, &target.declaring_class.name, &target.method.name);
    descend(
        cx,
        folder,
        &target.method.params,
        target.class_file,
        callee_scope,
        &format!("{}::{}", target.declaring_class.fqn, target.method.name),
        &display,
        target.this_exact,
        call,
        &[],
        env,
        scope.poisoned,
        descent,
        out,
    );
}

/// The provenance render base for a bound method/constructor call.
fn display_of_call(receiver: &Callee, declaring_class: &str, method: &str) -> String {
    match receiver {
        Callee::Construct { class } => format!("new {}", class.simple()),
        _ => format!("{declaring_class}::{method}"),
    }
}

/// Check the arguments of a resolved method/constructor call at its call site
/// (native runtime check plus the phpdoc declared-contract check; no double-report).
/// `class_file` locates the callee method's docblock context for class-name
/// resolution.
#[allow(clippy::too_many_arguments)]
fn check_method_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    method: &MethodDecl,
    class_file: usize,
    class_templates: &HashSet<String>,
    callee_name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) {
    let mut envelopes = parse_envelopes(method.docblock.as_deref());
    // Class-level `@template` names shadow same-named classes in every member
    // docblock of the class-like (issue #5) — the second, idempotent shadow stage.
    if let Some(e) = &mut envelopes {
        e.shadow_templates(class_templates);
    }
    for (i, arg) in call.args.iter().enumerate() {
        let Some(param) = method.params.get(i) else { break };
        if param.variadic {
            break;
        }
        if param.by_ref {
            continue;
        }

        let mut native_fired = false;
        if let Some(ty) = param.ty.as_ref() {
            let resolved: Option<(ArgValue, Option<String>)> = match &arg.value {
                v if v.is_literal() => Some((v.clone(), None)),
                ArgValue::Var(name) if !poisoned => env.get(name).and_then(|k| {
                    let v = k.singleton()?;
                    let prov = match &k.bound {
                        Some(b) => format!("from ${name}, {b}"),
                        None => format!("from ${name}, assigned at line {}", k.line),
                    };
                    Some((v, Some(prov)))
                }),
                ArgValue::Call(name, args) => {
                    if args.is_empty() {
                        cx.resolve_const_fn(name)
                            .map(|(lit, line)| {
                                (lit, Some(format!("from {name}(), defined at line {line}")))
                            })
                            .or_else(|| cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p))))
                    } else {
                        cx.try_fold(name, args, folder).map(|(l, p)| (l, Some(p)))
                    }
                }
                // A property read `$o->p` (ADR-0036): a `Singleton` prop fact flows.
                ArgValue::PropFetch { var, prop } if !poisoned => {
                    store.prop_fact(var, prop).and_then(|f| match f {
                        Fact::Singleton(v) => Some((arg_of_val(v), Some(format!("from ${var}->{prop}")))),
                        _ => None,
                    })
                }
                // A proven object (`new` / enum case) or resolved class constant
                // (ADR-0043 stage 3). Env-free; `self`/`parent` at the call site are
                // not available here, so only a written class name resolves.
                _ => cx.resolve_static_value(&arg.value, None).map(|v| (v, None)),
            };
            // Proof-layer consumption rule (ADR-0052 §5): silent on an `Asserted`
            // premise; the phpdoc contract check below still accepts it.
            if let Some((value, prov)) = resolved
                && value_stratum(&arg.value, env, Some(store)) == Stratum::Verified
                && is_type_error(cx, ty, &value)
                && !object_world_guard_blind(in_descent, ty, &value)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &value,
                    prov.as_deref(),
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // A variable bound to a proven object (ADR-0036 heap): the object-vs-type
            // definite-No, rendered against the variable (ADR-0043 stage 3).
            // Guard-blind inside a descent (see `object_world_guard_blind`).
            if !native_fired
                && !poisoned
                && !in_descent
                && let ArgValue::Var(name) = &arg.value
                && store.is_exact(name) // No-side needs exactness (audit G1)
                && let Some(class) = store.class_of(name)
                && cx.object_is_type_error(ty, class)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &ArgValue::Var(name.clone()),
                    Some(&format!("holds a {}", simple_class(class))),
                    callee_name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
        }

        if !native_fired
            && let Some(env_e) = &envelopes
        {
            check_phpdoc_param(
                cx,
                folder,
                env_e,
                param,
                class_file,
                method.span.start,
                callee_name,
                arg.span.start,
                &arg.value,
                env,
                store,
                poisoned,
                in_descent,
                out,
            );
        }
        // Callable-signature variance (issue #11) for a closure / first-class
        // callable argument to a method against a signature-bearing `callable(...)`
        // @param. The closure's declared signature is a static CST fact, so this is
        // safe to run at the resolved call site.
        if let ArgValue::Closure(closure) = &arg.value
            && let Some(env_e) = &envelopes
        {
            check_callable_arg(cx, env_e, param, callee_name, arg.span.start, closure, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Value / type helpers (unchanged from the per-file slice).
// ---------------------------------------------------------------------------

/// Render a call with its literal arguments for a folding provenance string.
fn render_call(name: &str, args: &[ArgValue]) -> String {
    let inner: Vec<String> = args.iter().map(ArgValue::render).collect();
    format!("{name}({})", inner.join(", "))
}

/// The generalized truth table: does passing (or returning) a **literal** `arg`
/// where a native scalar/union type `ty` is required provably raise a
/// `TypeError` under PHP 8.1+ (honoring `strict`)?
///
/// The table was settled **empirically against PHP 8.5.8** (the analyzer's floor
/// is 8.1, ADR-0011; these union-coercion rules have been stable since 8.0). The
/// reproduction snippets and outputs (union members, both modes):
///
/// ```text
/// COERCIVE (error iff the value coerces to NO member):
///   1.5   -> int|string  => OK  (becomes int 1; the string sink also accepts)
///   1.5   -> string|bool => OK  (becomes '1.5')
///   true  -> int|string  => OK  (becomes int 1)
///   "abc" -> int|float    => TypeError   (non-numeric string, no string sink)
///   "abc" -> int|false    => TypeError   (false-literal accepts only `false`)
///   "5"   -> int|float    => OK  (numeric string coerces)
///   false -> string|false => OK  (matches the `false` literal member exactly)
///   true  -> string|false => OK  (becomes '1' via the string member)
///   null  -> int|string   => TypeError   (non-nullable)
///   0/""/true -> false     => TypeError   (no coercion into a bool-literal)
/// STRICT (value must match SOME member; only int->float widening is implicit):
///   1.5   -> int|string  => TypeError   (float, no float member)
///   true  -> int|string  => TypeError   (bool, no bool/bool-literal member)
///   5     -> int|float    => OK  (int member; also OK via int->float widening)
///   false -> string|false => OK  (matches the `false` literal member)
///   true  -> string|false => TypeError   (`false` literal ≠ `true`; no bool)
///   5     -> string|false => TypeError   (int, no int member)
/// ```
///
/// Uncertain cells resolve to "not an error" (silence is always safe; ADR-0002).
///
/// ADR-0043 stage 3 opens two definite-No arms, both riding the trinary is-a
/// oracle on `cx`: a **proven object value** (`new` / enum case) errors iff every
/// union member provenly rejects its exact class, and a **scalar value** now sees
/// through any `Instance` union members (an object member never accepts a scalar —
/// no coercion exists — so the verdict rests on the scalar members, exactly as the
/// empirically-verified `member_accepts_*` tables already encode via their
/// `Instance => false` arms). A `null` value against an object-bearing type stays
/// silent (null-vs-object is out of scope; preserves the pre-stage-3 behavior and
/// sidesteps the `has_null_default` implicit-nullable interplay).
fn is_type_error(cx: &Cx, ty: &NativeType, arg: &ArgValue) -> bool {
    let strict = cx.strict();
    match arg {
        // `null` is accepted iff the type is nullable (`?T` / `null` member). An
        // object-bearing type stays silent on `null` (unchanged from stage 1).
        ArgValue::Null => !ty.nullable && !ty.has_instance(),
        // A concrete non-null literal: an error iff no member accepts it. `Instance`
        // members contribute nothing (they never accept a scalar) — ADR-0043 stage-3
        // scalar-vs-object opening (e.g. a raw string where an enum is required).
        ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Str(_) | ArgValue::Bool(_) => {
            if strict {
                !ty.members.iter().any(|m| member_accepts_strict(m, arg))
            } else {
                !ty.members.iter().any(|m| member_accepts_coercive(m, arg))
            }
        }
        // A proven object value (ADR-0043 stage 3): a definite No iff every union
        // member provenly rejects an object of its exact class. An unresolvable /
        // ambiguous class stays unproven (silent).
        ArgValue::New(..) | ArgValue::EnumCase(..) => match cx.proven_object_class(arg) {
            Some(class) => cx.object_is_type_error(ty, &class),
            None => false,
        },
        // An array is never a native scalar/union finding (arrays only ever fail
        // the phpdoc contract relation, checked separately).
        ArgValue::Array(_) => false,
        // Non-provable carriers: silent (a `Ternary` is resolved to a concrete arm
        // before this point; a `ClassConst` is resolved upstream to an enum case /
        // literal, so an unresolved one is genuinely unproven).
        ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Coalesce(..)
        | ArgValue::OffsetRead { .. }
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        | ArgValue::ClassConst(..)
        // A closure value against a scalar/union param is never a scalar finding
        // (a `callable`/`Closure` param is not a native scalar type this checks).
        | ArgValue::Closure(_)
        | ArgValue::Other => false,
    }
}

/// ADR-0043 stage 3 — the object-world native definite-No is **guard-blind inside
/// a binding descent** and must be suppressed there. A descent rebinds a callee's
/// parameter to a hypothetical caller value, but the callee's in-body `instanceof`
/// / type guards that would narrow that value are unmodeled (e.g. Carbon's
/// `if ($x instanceof DateTimeInterface) { … $x … }` is dead for a string `$x`,
/// yet the walk cannot prove it because the guard flows through an intermediate
/// boolean). Checking an object-world mismatch on a descent-bound value is
/// therefore unsound — exactly the reason descent-bound property writes are also
/// unchecked (see `apply_prop_assign`). Scalar-vs-scalar descent checks, whose
/// guards the walk *can* evaluate, are unaffected: only a judgment that touches an
/// object type (an `Instance`-bearing param, or a proven object value) is
/// suppressed. In the non-descent direct/propagation passes this is always `false`.
fn object_world_guard_blind(in_descent: bool, ty: &NativeType, value: &ArgValue) -> bool {
    in_descent
        && (ty.has_instance() || matches!(value, ArgValue::New(..) | ArgValue::EnumCase(..)))
}

/// Strict mode: does a single union `member` accept the non-null literal `arg`
/// *exactly* (the only implicit conversion PHP allows in strict mode is
/// int→float, so a `float` member also accepts an `int` arg)?
fn member_accepts_strict(m: &TypeMember, arg: &ArgValue) -> bool {
    match m {
        TypeMember::Scalar(ScalarType::Int) => matches!(arg, ArgValue::Int(_)),
        TypeMember::Scalar(ScalarType::Float) => matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
        TypeMember::Scalar(ScalarType::String) => matches!(arg, ArgValue::Str(_)),
        TypeMember::Scalar(ScalarType::Bool) => matches!(arg, ArgValue::Bool(_)),
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if v == b),
        // Object member (ADR-0043): no scalar literal is a member of a class type
        // or an object intersection. Unreachable in stage 1 (the `has_instance`
        // guard in `is_type_error` short-circuits before any member is inspected);
        // explicit for stage 3.
        TypeMember::Instance { .. } | TypeMember::InstanceInter(_) => false,
    }
}

/// Coercive mode: could the non-null literal `arg` be coerced into this single
/// union `member`? `string`/`bool` are universal sinks for scalars; numeric
/// members accept int/float/bool and numeric strings only; a bool-literal member
/// accepts **only** the exact matching bool value (no coercion into it).
fn member_accepts_coercive(m: &TypeMember, arg: &ArgValue) -> bool {
    match m {
        // Any scalar coerces to `string` or to `bool`.
        TypeMember::Scalar(ScalarType::String) | TypeMember::Scalar(ScalarType::Bool) => true,
        // Numeric members accept numbers, bools, and numeric strings; a
        // non-numeric string is the only scalar that fails.
        TypeMember::Scalar(ScalarType::Int) | TypeMember::Scalar(ScalarType::Float) => match arg {
            ArgValue::Str(s) => php_is_numeric(s),
            ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_) => true,
            _ => false,
        },
        // No value coerces *into* a bool-literal; only the exact bool matches.
        TypeMember::BoolLiteral(b) => matches!(arg, ArgValue::Bool(v) if v == b),
        // Object member (ADR-0043): no scalar coerces into a class type or an
        // object intersection. See `member_accepts_strict` — unreachable in stage 1,
        // explicit for stage 3.
        TypeMember::Instance { .. } | TypeMember::InstanceInter(_) => false,
    }
}

/// The value a parameter of type `ty` holds when `value` is passed under
/// `strict`, or `None` when the pass fatals at entry or the coercion is
/// uncertain (silence is safe — ADR-0002).
///
/// For a single-scalar type the exact per-scalar coercion is reproduced (keeping
/// interprocedural binding precise). For a **union** the value is bound only when
/// it already matches a member's own type exactly — Steins does not guess which
/// member PHP would coerce a mismatched value into, so it stops the descent
/// (silent) rather than risk an unsound bound value.
fn coerce_into_param(cx: &Cx, ty: &NativeType, value: &ArgValue) -> Option<ArgValue> {
    // ADR-0043 stage 1 — an object-bearing type binds the value verbatim, exactly
    // as the pre-ADR-0043 `None`-typed (untracked) parameter did: the caller's
    // `None => bind raw value` path is reproduced here so an object parameter does
    // not abort the interprocedural descent. No scalar coercion applies to objects.
    if ty.has_instance() {
        return Some(value.clone());
    }
    if is_type_error(cx, ty, value) {
        return None;
    }
    if matches!(value, ArgValue::Null) {
        return Some(ArgValue::Null);
    }
    if let [TypeMember::Scalar(scalar)] = ty.members.as_slice() {
        return coerce_scalar(*scalar, value);
    }
    // Union: bind only on an exact-type member match; otherwise silence.
    if ty.members.iter().any(|m| member_matches_exact(m, value)) {
        return Some(value.clone());
    }
    None
}

/// Whether a union `member` matches the *runtime type* of the non-null literal
/// `value` exactly (no coercion) — used to decide when a union binding is safe.
fn member_matches_exact(m: &TypeMember, value: &ArgValue) -> bool {
    match (m, value) {
        (TypeMember::Scalar(ScalarType::Int), ArgValue::Int(_))
        | (TypeMember::Scalar(ScalarType::Float), ArgValue::Float(_))
        | (TypeMember::Scalar(ScalarType::String), ArgValue::Str(_))
        | (TypeMember::Scalar(ScalarType::Bool), ArgValue::Bool(_)) => true,
        (TypeMember::BoolLiteral(b), ArgValue::Bool(v)) => v == b,
        // Object member (ADR-0043): scalar literals never match a class type.
        _ => false,
    }
}

/// The value a single-scalar parameter holds after coercion (the per-scalar
/// PHP 8 coercion table), or `None` when the conversion is uncertain.
fn coerce_scalar(scalar: ScalarType, value: &ArgValue) -> Option<ArgValue> {
    Some(match (scalar, value) {
        (ScalarType::Int, ArgValue::Int(_))
        | (ScalarType::Float, ArgValue::Float(_))
        | (ScalarType::String, ArgValue::Str(_))
        | (ScalarType::Bool, ArgValue::Bool(_)) => value.clone(),

        (ScalarType::Float, ArgValue::Int(i)) => ArgValue::Float(*i as f64),

        (ScalarType::Int, ArgValue::Str(s)) => ArgValue::Int(php_str_to_int(s)?),
        (ScalarType::Float, ArgValue::Str(s)) => ArgValue::Float(php_str_to_float(s)?),
        (ScalarType::Int, ArgValue::Float(f)) => ArgValue::Int(php_float_to_int(*f)?),
        (ScalarType::Int, ArgValue::Bool(b)) => ArgValue::Int(i64::from(*b)),
        (ScalarType::Float, ArgValue::Bool(b)) => ArgValue::Float(if *b { 1.0 } else { 0.0 }),
        (ScalarType::Bool, ArgValue::Int(i)) => ArgValue::Bool(*i != 0),
        (ScalarType::Bool, ArgValue::Float(f)) => ArgValue::Bool(*f != 0.0),
        (ScalarType::Bool, ArgValue::Str(s)) => ArgValue::Bool(!(s.is_empty() || s == "0")),
        (ScalarType::String, ArgValue::Int(i)) => ArgValue::Str(i.to_string()),
        (ScalarType::String, ArgValue::Bool(b)) => {
            ArgValue::Str(if *b { "1".to_owned() } else { String::new() })
        }

        _ => return None,
    })
}

/// Whitespace PHP trims before interpreting a numeric string.
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
}

/// Convert a PHP numeric string to the int it coerces to.
fn php_str_to_int(s: &str) -> Option<i64> {
    let t = php_trim(s);
    if let Ok(i) = t.parse::<i64>() {
        return Some(i);
    }
    php_float_to_int(t.parse::<f64>().ok()?)
}

/// Convert a PHP numeric string to the float it coerces to.
fn php_str_to_float(s: &str) -> Option<f64> {
    php_trim(s).parse::<f64>().ok()
}

/// Truncate a float toward zero to an int (PHP scalar coercion).
fn php_float_to_int(f: f64) -> Option<i64> {
    f.is_finite().then(|| f.trunc() as i64)
}

/// PHP 8 `is_numeric` semantics.
fn php_is_numeric(s: &str) -> bool {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    let bytes = s.as_bytes();
    let mut i = 0;

    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }

    let mut saw_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        saw_digit = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return false;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut saw_exp = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_exp = true;
        }
        if !saw_exp {
            return false;
        }
    }

    i == bytes.len()
}

// ---------------------------------------------------------------------------
// PHPDoc declared-contract acceptance (ADR-0029/0030 relation #1).
//
// A separate acceptance relation from the runtime one above: **pure set
// semantics, NO coercion** (a numeric string `"5"` does NOT satisfy `int`). The
// judgment is trinary — `Yes`/`No`/`Maybe` — and only a definite `No` (proven
// non-membership) is ever reported; `Maybe` is silent (the zero-FP side).
// ---------------------------------------------------------------------------

/// Intersection-style combine: `No` dominates, then `Maybe`, else `Yes`. Used
/// when *every* sub-obligation must hold (element/key membership, shape items).
/// This is exactly [`Certainty::and`], kept as a free function for the existing
/// call sites.
fn combine(a: Certainty, b: Certainty) -> Certainty {
    a.and(b)
}

/// A convenience alias inside this module: the phpdoc contract acceptance code
/// (ADR-0030) was written against a local `Tri`; it now shares the one project-wide
/// [`Certainty`] type (ADR-0031 — one trinary, never parallel ones).
use Certainty as Tri;

/// A proven value in contract terms: a scalar literal, an array of proven values
/// (normalized keys), or an object of an exact class (a `New` fact).
///
/// An object additionally carries its **class-level generic type-argument values**
/// (ADR-0032 tier 3, issue #10): the per-`@template` values that flowed into the
/// object at its `new` site (tier-1 propagation), in template-declaration order.
/// The vector is empty when the class is non-generic, or when the arguments could
/// not be proven — an empty carry is the honest floor (acceptance then answers
/// `Maybe` on the argument half, never a manufactured `No`). These live in the
/// contract lane, not the object-free value lattice (ADR-0035/0043 §4).
enum CVal {
    Scalar(ArgValue),
    Array(Vec<(NormKey, CVal)>),
    Object(String, Vec<CVal>),
}

/// The `@param`/`@return` phpdoc envelopes parsed off one declaration's docblock.
struct Envelopes {
    /// Parameter name (no `$`) → declared phpdoc type.
    params: Vec<(String, PType)>,
    ret: Option<PType>,
    /// Parameter names (no `$`) that an assertion tag (`@phpstan-assert` &c.)
    /// targets on this same declaration — the function is an **assertion helper**
    /// for them (see [`check_phpdoc_param`]). Property/`$this->…` assertion targets
    /// are excluded (they say nothing about a call-site argument).
    assert_params: HashSet<String>,
    /// The full assertion specs on this declaration (Feature D): the asserted type
    /// applied to the caller's env after a call (`Always`), or in guard position
    /// (`IfTrue`/`IfFalse`). Property/`$this` targets are excluded (as above).
    asserts: Vec<AssertSpec>,
}

/// One `@phpstan-assert[-if-true|-if-false] [!]<type> $param` spec (Feature D).
struct AssertSpec {
    /// Target parameter name (no `$`).
    param: String,
    /// The asserted phpdoc type.
    ty: PType,
    /// Unconditional / conditional-on-true / conditional-on-false.
    kind: AssertKind,
    /// The negated form (`@phpstan-assert !T $x`): asserts NOT `T`.
    negated: bool,
}

impl Envelopes {
    fn param(&self, name: &str) -> Option<&PType> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Whether `name` is an assertion target on this declaration, in which case its
    /// `@param` states a **post**-condition and checking arguments against it is a
    /// category error (see [`check_phpdoc_param`]).
    fn is_assert_target(&self, name: &str) -> bool {
        self.assert_params.contains(name)
    }

    /// Neutralize every envelope identifier naming a template from `shadow` (issue
    /// #5): a `@template`-declared name is a template parameter, not a class, so it
    /// must not lower to a class contract that would reject real arguments. Applied
    /// in two idempotent stages — [`parse_envelopes`] applies this declaration's own
    /// `@template` names, then a member-check site applies the enclosing class-like's
    /// class-level `@template` names — so a `no-op` empty set costs nothing.
    fn shadow_templates(&mut self, shadow: &HashSet<String>) {
        if shadow.is_empty() {
            return;
        }
        for (_, t) in &mut self.params {
            neutralize_templates(t, shadow);
        }
        if let Some(t) = &mut self.ret {
            neutralize_templates(t, shadow);
        }
        for s in &mut self.asserts {
            neutralize_templates(&mut s.ty, shadow);
        }
    }
}

/// The lowercased set of `@template` names a docblock declares — the *shadow set*
/// (issue #5). A name here is a template parameter, not a class, inside the
/// docblock's own `@param`/`@return`/`@var` types (and, when this is a class-like's
/// docblock, inside every member docblock).
///
/// **Case-insensitive by decision.** PHPStan treats template names as
/// case-sensitive identifiers, so strictly `@template Model` would not shadow a
/// `@param model`. Steins folds case instead, for three reasons: (1) zero-FP is
/// paramount and over-shadowing only ever *silences* a diagnostic — it can never
/// manufacture one; (2) the whole identifier pipeline (`is_known_class`, contract
/// lowering, `accepts_identifier`) already normalizes to lowercase, so a folded
/// shadow set composes uniformly instead of needing a lone case-sensitive path; (3)
/// the only observable divergence from PHPStan is Steins staying silent where
/// PHPStan would still resolve the class — and silence is the safe side (ADR-0029).
fn template_names_of(docblock: Option<&str>) -> HashSet<String> {
    docblock
        .map(|t| {
            steins_phpdoc::scan_template_names(t)
                .into_iter()
                .map(|n| n.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Rewrite every **bare, unqualified** identifier naming a template from `shadow`
/// to an opaque node (issue #5). The neutral node is [`PKind::Unsupported`], which
/// lowers to `ContractTy::Opaque` and rides `accepts` as `Maybe` — exactly the
/// silence a template already gets today when it names no existing class (ADR-0032:
/// templates are transparent/thin where propagation does not reach). A `\`-qualified
/// or namespaced reference (`\Model`, `App\Model`) is **never** shadowed —
/// qualification opts out of the template namespace. Idempotent; recurses through
/// every composite so a nested `list<Model>` / `array{a: Model}` is neutralized too.
fn neutralize_templates(ty: &mut PType, shadow: &HashSet<String>) {
    match &mut ty.kind {
        PKind::Identifier(name) => {
            if !name.contains('\\') && shadow.contains(&name.to_ascii_lowercase()) {
                let raw = std::mem::take(name);
                ty.kind = PKind::Unsupported(raw);
            }
        }
        PKind::Nullable(inner) | PKind::Array(inner) => neutralize_templates(inner, shadow),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            for t in types {
                neutralize_templates(t, shadow);
            }
        }
        PKind::Generic { args, .. } => {
            for a in args {
                neutralize_templates(&mut a.ty, shadow);
            }
        }
        PKind::OffsetAccess { base, offset } => {
            neutralize_templates(base, shadow);
            neutralize_templates(offset, shadow);
        }
        PKind::ArrayShape(s) => {
            for it in &mut s.items {
                neutralize_templates(&mut it.value, shadow);
            }
        }
        PKind::ObjectShape(items) => {
            for it in items {
                neutralize_templates(&mut it.value, shadow);
            }
        }
        PKind::This | PKind::Callable(_) | PKind::Const(_) | PKind::Conditional(_)
        | PKind::Unsupported(_) => {}
    }
}

/// Parse the `@param`/`@return` envelopes from a raw docblock, or `None` when the
/// declaration carries no docblock or no envelope-bearing tag. A tag whose type
/// fails to parse (or carries an `Unsupported` node) contributes no envelope; the
/// other tags are unaffected (ADR-0029). `@var`/`@throws` are out of scope.
fn parse_envelopes(docblock: Option<&str>) -> Option<Envelopes> {
    let text = docblock?;
    // A `@phpstan-`/`@psalm-` prefixed tag overrides the plain one for the same
    // target (PHPStan precedence; ADR-0029). Track whether each recorded envelope
    // came from a prefixed tag so a later prefixed tag wins but a plain one never
    // displaces a prefixed one.
    let mut params: Vec<(String, PType)> = Vec::new();
    let mut param_prefixed: HashSet<String> = HashSet::new();
    let mut ret: Option<PType> = None;
    let mut ret_prefixed = false;
    let mut assert_params: HashSet<String> = HashSet::new();
    let mut asserts: Vec<AssertSpec> = Vec::new();
    for tag in scan_docblock(text) {
        // An assertion tag targeting a parameter marks it an assert-helper param
        // (its `@param` is a post-condition; ADR-0030). Property targets are inert.
        // All three kinds (Always/IfTrue/IfFalse) and the negated form exempt alike:
        // whatever the type or condition, the parameter is not being *constrained*
        // on entry, so a call-site argument cannot violate it. The spec is also
        // recorded (with its parsed type) for post-call application (Feature D).
        if let TagKind::Assert { kind: akind, negated } = tag.kind
            && !tag.assert_property_target
            && let Some(var) = &tag.var_name
        {
            let name = var.trim_start_matches('$').to_owned();
            assert_params.insert(name.clone());
            if let Some(ty) = parse_tag_type(&tag.type_text) {
                asserts.push(AssertSpec { param: name, ty, kind: akind, negated });
            }
            continue;
        }
        match tag.kind {
            TagKind::Param => {
                let Some(var) = &tag.var_name else { continue };
                let name = var.trim_start_matches('$').to_owned();
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                match params.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => {
                        // Replace only if we are not downgrading precedence.
                        if tag.prefixed || !param_prefixed.contains(&name) {
                            slot.1 = ty;
                        }
                    }
                    None => params.push((name.clone(), ty)),
                }
                if tag.prefixed {
                    param_prefixed.insert(name);
                }
            }
            TagKind::Return => {
                let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
                if tag.prefixed || (ret.is_none() && !ret_prefixed) {
                    ret = Some(ty);
                    ret_prefixed = tag.prefixed;
                }
            }
            TagKind::Var | TagKind::Throws => {}
            // Assertion tags are consumed above (collected into `assert_params`);
            // they never contribute a `@param`/`@return` envelope.
            TagKind::Assert { .. } => {}
        }
    }
    // Return an envelope set whenever there is anything to check *or* any assertion
    // to remember: an assert-only docblock still carries the exemption fact, so a
    // sibling `@param` (added later, or resolved in another pass) sees it.
    if !(!params.is_empty() || ret.is_some() || !assert_params.is_empty()) {
        return None;
    }
    let mut env = Envelopes { params, ret, assert_params, asserts };
    // Shadow this declaration's own `@template` names in its envelope types (issue
    // #5). A member-check site additionally applies the enclosing class-like's
    // class-level templates (idempotent second stage).
    env.shadow_templates(&template_names_of(Some(text)));
    Some(env)
}

/// Parse one tag's type text into a phpdoc [`PType`], or `None` on a parse error
/// or an `Unsupported` node (no envelope — silence is safe).
fn parse_tag_type(text: &str) -> Option<PType> {
    let parsed = parse_type(text).ok()?;
    (!kind_has_unsupported(&parsed.ty.kind)).then_some(parsed.ty)
}

/// Whether a phpdoc type subtree contains an `Unsupported` node anywhere.
fn kind_has_unsupported(kind: &PKind) -> bool {
    match kind {
        PKind::Unsupported(_) => true,
        PKind::Nullable(t) | PKind::Array(t) => kind_has_unsupported(&t.kind),
        PKind::Union { types, .. } | PKind::Intersection(types) => {
            types.iter().any(|t| kind_has_unsupported(&t.kind))
        }
        PKind::Generic { args, .. } => args.iter().any(|a| kind_has_unsupported(&a.ty.kind)),
        PKind::OffsetAccess { base, offset } => {
            kind_has_unsupported(&base.kind) || kind_has_unsupported(&offset.kind)
        }
        PKind::ArrayShape(s) => s.items.iter().any(|i| kind_has_unsupported(&i.value.kind)),
        PKind::ObjectShape(items) => items.iter().any(|i| kind_has_unsupported(&i.value.kind)),
        _ => false,
    }
}

impl<'a> Cx<'a> {
    /// Resolve a call/return value to a proven [`CVal`] (scalars, arrays of proven
    /// values, or a `New` exact-class object), or `None` when not provable.
    fn resolve_cval(
        &self,
        value: &ArgValue,
        env: &HashMap<String, Known>,
        store: &Store,
        poisoned: bool,
        folder: &mut dyn Folder,
    ) -> Option<CVal> {
        match value {
            v if v.is_literal() => Some(CVal::Scalar(v.clone())),
            // `new Class(args)` — a proven object of exactly `Class`, carrying the
            // generic type-argument values that flow into it (ADR-0032 tier 3,
            // issue #10). Empty carry when non-generic / unprovable (FP-safe).
            ArgValue::New(class_ref, args) if !poisoned => {
                let class = self.class_fqn(class_ref);
                let targs = self.infer_generic_args(&class, args, env, store, poisoned, folder);
                Some(CVal::Object(class, targs))
            }
            // ADR-0043 stage 4: an enum case is an object value of its enum class,
            // so it can ride the is-a oracle against enum/interface phpdoc contracts.
            // An enum is never `@template`-parameterized → no generic carry.
            ArgValue::EnumCase(fqn, _) => Some(CVal::Object(fqn.clone(), Vec::new())),
            // A class-const access (`Foo::BAR`, `Suit::Hearts`) resolves env-free:
            // an enum case becomes an object, a literal const its value. `self`/
            // `parent` need an enclosing class, absent here → unresolved (silent).
            ArgValue::ClassConst(sc, name) => match self.resolve_class_const(sc, name, None)? {
                ArgValue::EnumCase(fqn, _) => Some(CVal::Object(fqn, Vec::new())),
                lit => self.resolve_cval(&lit, env, store, poisoned, folder),
            },
            ArgValue::Array(items) => {
                let normalized = normalize_array(items);
                let mut out = Vec::with_capacity(normalized.len());
                for (k, v) in normalized {
                    out.push((k, self.resolve_cval(&v, env, store, poisoned, folder)?));
                }
                Some(CVal::Array(out))
            }
            ArgValue::Var(name) if !poisoned => {
                if let Some(k) = env.get(name) {
                    // A `OneOf` fact is not one proven value → not a `CVal`.
                    let v = k.singleton()?;
                    self.resolve_cval(&v, env, store, poisoned, folder)
                } else if store.is_exact(name) {
                    // Only an EXACT object becomes a `CVal::Object` (audit G1): the
                    // phpdoc-acceptance consumer draws a No-side `is_a` conclusion from
                    // it, which a lower-bound `$this` would make unsound. An inexact
                    // object stays unresolved (silent) — acceptance never fires on it.
                    //
                    // Generic type arguments are NOT carried through a variable binding
                    // this slice (the heap object records no type-arg carry); a
                    // `$x = new Box('x'); f($x)` therefore judges only its class half.
                    // Stage 1 scopes the argument half to the direct `new` argument
                    // position (the conformance fixtures' shape) — empty carry here.
                    store.class_of(name).map(|c| CVal::Object(c.to_owned(), Vec::new()))
                } else {
                    None
                }
            }
            ArgValue::Call(..) => {
                self.resolve_literal(value, env, poisoned, folder).map(CVal::Scalar)
            }
            _ => None,
        }
    }

    /// Resolve a phpdoc class name to its FQN in the callee file `cfile`'s context
    /// (offset `coff` picks the namespace/use scope where the docblock was written).
    fn resolve_pclass(&self, cfile: usize, coff: u32, name: &str) -> String {
        let raw = name.trim_start_matches('\\').to_owned();
        let kind = if name.starts_with('\\') {
            RefKind::FullyQualified
        } else if raw.contains('\\') {
            RefKind::Qualified
        } else {
            RefKind::Unqualified
        };
        self.units[cfile].tree.resolve_class_fqn(&NameRef { raw, kind, offset: coff })
    }

    /// Whether `fqn` names a **known class** — a Unique project class or a
    /// catalogued builtin (ADR-0043 stage 4). This is the same closure predicate
    /// the is-a oracle uses ([`Self::ancestors_of`] returns `Some`): only a known
    /// class may make a proven scalar a definite non-member. An unresolved bare
    /// identifier (a `@template` param, a `@phpstan-type` alias, or an uncatalogued
    /// external) is *not* known, so a scalar against it stays silent.
    fn is_known_class(&self, fqn: &str) -> bool {
        self.ancestors_of(fqn.trim_start_matches('\\')).is_some()
    }

    // -----------------------------------------------------------------------
    // ADR-0043 stage 3 — native object acceptance (definite-No opening).
    // -----------------------------------------------------------------------

    /// The proven exact class (namespace-resolved FQN) of an object-valued
    /// [`ArgValue`], or `None` when it is not a proven object. `New` resolves its
    /// written class reference in this file's context (matching the ADR-0036 heap
    /// `class_of`); an `EnumCase` already carries the resolved enum FQN.
    fn proven_object_class(&self, v: &ArgValue) -> Option<String> {
        match v {
            ArgValue::New(r, _) => Some(self.class_fqn(r)),
            ArgValue::EnumCase(fqn, _) => Some(fqn.clone()),
            _ => None,
        }
    }

    /// ADR-0043 stage 3 — does an object of exact class `class_fqn` **provably
    /// violate** the native type `ty`? A definite-No: `true` only when *every*
    /// union member definitively rejects an object of that class (any `Unknown`
    /// or accepting member makes the whole verdict silent). `nullable` is
    /// irrelevant to an object value — an object is never `null`.
    fn object_is_type_error(&self, ty: &NativeType, class_fqn: &str) -> bool {
        ty.members.iter().all(|m| self.member_rejects_object(m, class_fqn))
    }

    /// Whether a single union `member` **definitively rejects** an object of exact
    /// class `class_fqn`.
    ///
    /// Verified against PHP 8.5.8 (`php -r`):
    /// - `int`/`float`/`bool` (and `false`/`true` literals): no object — **not
    ///   even one with `__toString`** — coerces into these in either mode; passing
    ///   any object `TypeError`s → an unconditional definite reject.
    /// - `string`: a `__toString` object *does* coerce to a `string` parameter in
    ///   **coercive** mode (no error), while a plain object and **any** object in
    ///   **strict** mode `TypeError`. Steins does not (yet) prove the *absence* of
    ///   `__toString` across a class hierarchy, so a `string` member is a definite
    ///   reject only in **strict** mode; in coercive mode it stays silent
    ///   (Unknown), the FP-safe choice.
    /// - `Instance { fqn, .. }`: rejects iff the trinary is-a oracle proves non-membership
    ///   (`IsA::No`); `Yes` accepts and `Unknown` (incomplete hierarchy) is silent.
    fn member_rejects_object(&self, m: &TypeMember, class_fqn: &str) -> bool {
        match m {
            TypeMember::Instance { fqn, .. } => self.is_a(class_fqn, fqn) == IsA::No,
            // An intersection (`A&B&…`) demands membership in **every** conjunct,
            // so it definitively rejects the object the moment the is-a oracle
            // proves non-membership (`IsA::No`) in **any** one of them. An
            // incomplete hierarchy on the remaining conjuncts stays silent — the
            // one proven `No` is already a sound definite reject.
            TypeMember::InstanceInter(cs) => {
                cs.iter().any(|c| self.is_a(class_fqn, &c.fqn) == IsA::No)
            }
            TypeMember::Scalar(ScalarType::String) => self.strict(),
            TypeMember::Scalar(_) | TypeMember::BoolLiteral(_) => true,
        }
    }

    /// Resolve a [`StaticClass`] class-expression to its FQN (ADR-0043). `Named`
    /// resolves in this file's namespace context (source-cased); `self`/`parent`
    /// need the enclosing class; `static` (late static binding) stays unproven.
    fn resolve_static_class_fqn(&self, sc: &StaticClass, enclosing: Option<&str>) -> Option<String> {
        match sc {
            StaticClass::Named(r) => Some(self.class_fqn(r)),
            StaticClass::SelfKw => enclosing.map(str::to_owned),
            StaticClass::Parent => self.parent_fqn(enclosing?),
            StaticClass::Static => None,
        }
    }

    /// Resolve a class-constant / enum-case access `Class::NAME` to a proven value
    /// (ADR-0043 §2), or `None` when unresolvable/non-literal (→ silent).
    ///
    /// - `Class::class` → the FQN **string** literal. Only a written name
    ///   (`Named`) is resolved: it preserves the declared source casing (verified
    ///   against php 8.5.8 — `::class` yields the `use`-target's declared casing).
    ///   `self`/`parent`/`static::class` resolve only to the lowercase-normalized
    ///   index FQN, so emitting them would risk a wrong-case string — left
    ///   unproven (documented deferral).
    /// - An enum case → an [`ArgValue::EnumCase`] **object** value of the enum
    ///   class (never its backing scalar — an enum case is an object).
    /// - A class constant with a literal initializer → that literal, resolved
    ///   through the class/interface hierarchy (child overrides parent).
    fn resolve_class_const(&self, sc: &StaticClass, name: &str, enclosing: Option<&str>) -> Option<ArgValue> {
        if name.eq_ignore_ascii_case("class") {
            return match sc {
                StaticClass::Named(r) => {
                    Some(ArgValue::Str(self.class_fqn(r).trim_start_matches('\\').to_owned()))
                }
                _ => None,
            };
        }
        let fqn = self.resolve_static_class_fqn(sc, enclosing)?;
        if let Some((_, cd)) = self.find_class(&fqn)
            && cd.is_enum
            && cd.enum_cases.iter().any(|c| c.name == name)
        {
            return Some(ArgValue::EnumCase(cd.fqn.clone(), name.to_owned()));
        }
        self.resolve_const_literal(&fqn, name)
    }

    /// Resolve a class constant `fqn::name` to its literal value by walking the
    /// class's own consts, its directly-implemented interfaces' consts, then its
    /// parent chain (most-derived first, matching PHP constant override). Returns
    /// `None` on an unresolvable node or a name with no proven literal.
    fn resolve_const_literal(&self, fqn: &str, name: &str) -> Option<ArgValue> {
        let mut cur = fqn.to_owned();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(cur.to_ascii_lowercase()) {
                return None;
            }
            let (file, cd) = self.find_class(&cur)?;
            if let Some((_, v)) = cd.consts.iter().find(|(n, _)| n == name) {
                return Some(v.clone());
            }
            for iref in &cd.implements {
                let ifqn = self.units[file].tree.resolve_class_fqn(iref);
                if let Some((_, icd)) = self.find_class(&ifqn)
                    && let Some((_, v)) = icd.consts.iter().find(|(n, _)| n == name)
                {
                    return Some(v.clone());
                }
            }
            let pref = cd.parent.as_ref()?;
            cur = self.units[file].tree.resolve_class_fqn(pref);
        }
    }

    /// Resolve an [`ArgValue`] to a proven value **without an environment** — a
    /// self-evident literal, a proven object (`new` / enum case), or a resolved
    /// class constant (ADR-0043). Feeds the native definite-No checks at the
    /// call/return sites. `enclosing` supplies `self`/`parent` for class-const
    /// resolution inside a method body (`None` at file scope).
    fn resolve_static_value(&self, v: &ArgValue, enclosing: Option<&str>) -> Option<ArgValue> {
        match v {
            _ if v.is_literal() => Some(v.clone()),
            ArgValue::New(..) | ArgValue::EnumCase(..) => Some(v.clone()),
            ArgValue::ClassConst(sc, name) => self.resolve_class_const(sc, name, enclosing),
            _ => None,
        }
    }

    /// The **trinary is-a oracle** (ADR-0043 §3): is a value of exact class
    /// `sub_fqn` an instance of `super_fqn`?
    ///
    /// - **`Yes`** — a supertype path exists: the parent chain *and* the
    ///   transitive `implements` closure (class→interface and interface→interface,
    ///   since a lowered interface's extends become parent+implements). Reflexive
    ///   (`sub == super` is `Yes`).
    /// - **`No`** — only under a **completely enumerated hierarchy**: every
    ///   ancestor edge reachable from `sub` resolved either Unique in-project or in
    ///   the catalog's builtin tree, and `super` is absent from that closed
    ///   ancestor set. This is the Certainty discipline applied to subtyping —
    ///   non-membership is provable only under closure.
    /// - **`Unknown`** — the enumeration is incomplete: some ancestor is
    ///   unresolvable/ambiguous, or the chain leaves the project into an
    ///   uncatalogued builtin, or `sub`/`super` is itself unknown.
    ///
    /// Enums (ADR-0043): a lowered enum is-a its explicit `implements` plus the
    /// implicit `UnitEnum` interface, and a *backed* enum additionally is-a
    /// `BackedEnum` (which the catalog records as extending `UnitEnum`).
    ///
    /// A `use`d trait does **not** force `Unknown`: in PHP a trait adds methods,
    /// never types, so it cannot change the is-a relation — [`Self::ancestors_of`]
    /// simply ignores trait use and reports the class's real parent/interfaces.
    fn is_a(&self, sub_fqn: &str, super_fqn: &str) -> IsA {
        self.is_a_tracked(sub_fqn, super_fqn).0
    }

    /// [`Self::is_a`], additionally reporting whether the verdict was **catalog-
    /// backed** — whether any ancestor edge on the walk resolved through the builtin
    /// catalog ([`steins_catalog::builtin_class_supers`]) rather than in-project
    /// source. ADR-0052 A11 reads this: a catalog-backed verdict used for arm
    /// deletion is demoted to `Unknown` on a PHP-minor skew (the builtin edge set
    /// may differ from the catalog pin). A reflexive or purely in-project verdict is
    /// never catalog-backed (`false`), so a project's own `A|B` union narrows under
    /// A11 exactly as before — the demotion touches only builtin-dependent edges.
    fn is_a_tracked(&self, sub_fqn: &str, super_fqn: &str) -> (IsA, bool) {
        let target = super_fqn.trim_start_matches('\\');
        // `Stringable` is implicitly implemented by any class with a `__toString`
        // method (PHP 8.0+), which the explicit parent/`implements` closure does
        // not see. For this target only: a proven `__toString` on any visited class
        // is a definite `Yes`, and a visited trait-using class (whose merged methods
        // are unmodeled — it *might* declare `__toString`) forces `Unknown` rather
        // than an unsound `No`.
        let stringable_target = target.eq_ignore_ascii_case("Stringable");
        let mut queue: Vec<String> = vec![sub_fqn.trim_start_matches('\\').to_owned()];
        let mut seen: HashSet<String> = HashSet::new();
        // Whether every ancestor edge inspected so far resolved — the closure
        // condition for a sound `No`. A single unresolvable node taints it.
        let mut complete = true;
        // Whether a visited class may implicitly gain `Stringable` via a trait.
        let mut maybe_stringable = false;
        // Whether any traversed ancestor edge came from the builtin catalog (A11).
        let mut catalog = false;
        while let Some(cur) = queue.pop() {
            if cur.eq_ignore_ascii_case(target) {
                return (IsA::Yes, catalog);
            }
            if !seen.insert(cur.to_ascii_lowercase()) {
                continue;
            }
            if stringable_target
                && let Some((_, cd)) = self.find_class(&cur)
            {
                if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case("__toString")) {
                    return (IsA::Yes, catalog);
                }
                if cd.uses_traits {
                    maybe_stringable = true;
                }
            }
            // An edge resolved through the catalog (not an in-project class) marks
            // the whole verdict catalog-backed.
            let in_project = self.find_class(&cur).is_some();
            match self.ancestors_of(&cur) {
                Some(supers) => {
                    if !in_project {
                        catalog = true;
                    }
                    queue.extend(supers);
                }
                None => complete = false,
            }
        }
        if stringable_target && maybe_stringable {
            return (IsA::Unknown, catalog);
        }
        (if complete { IsA::No } else { IsA::Unknown }, catalog)
    }

    /// The **direct** supertypes (parent + `implements`, plus an enum's implicit
    /// interfaces) of `fqn`, or `None` when `fqn` is an unknown external (not a
    /// Unique project class, not a catalogued builtin) — which makes the is-a
    /// enumeration incomplete. A resolvable class with no supertypes returns an
    /// empty vector (fully enumerated, a root).
    fn ancestors_of(&self, fqn: &str) -> Option<Vec<String>> {
        if let Some((file, cd)) = self.find_class(fqn) {
            let tree = &self.units[file].tree;
            let mut supers = Vec::new();
            if let Some(pref) = &cd.parent {
                supers.push(tree.resolve_class_fqn(pref));
            }
            for imp in &cd.implements {
                supers.push(tree.resolve_class_fqn(imp));
            }
            if cd.is_enum {
                supers.push("UnitEnum".to_owned());
                if cd.enum_backing.is_some() {
                    supers.push("BackedEnum".to_owned());
                }
            }
            Some(supers)
        } else {
            steins_catalog::builtin_class_supers(fqn)
                .map(|s| s.into_iter().map(str::to_owned).collect())
        }
    }
}

/// The verdict of the trinary is-a oracle ([`Cx::is_a`], ADR-0043 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsA {
    /// A supertype path exists (membership is proven).
    Yes,
    /// The hierarchy is completely enumerated and the target is absent from it
    /// (non-membership is proven under closure).
    No,
    /// The hierarchy is incomplete — no verdict (the FP-safe silence).
    Unknown,
}

/// The **project** is-a oracle for contract-arm subtraction (ADR-0052 N4): the
/// steins-infer implementor of steins-contract's [`normalize::IsaOracle`] seam.
/// It wraps the real trinary hierarchy ([`Cx::is_a_tracked`]) and applies the A11
/// version-skew demotion — keeping steins-contract free of any steins-infer /
/// catalog dependency (the polarity law stays in steins-contract; the hierarchy
/// and version knowledge stay here).
struct ProjectIsa<'c, 'a> {
    cx: &'c Cx<'a>,
    /// Whether a catalog-backed verdict must demote to `Unknown` (A11 skew).
    demote_catalog: bool,
}

impl normalize::IsaOracle for ProjectIsa<'_, '_> {
    fn is_a(&self, sub: &str, sup: &str) -> Certainty {
        let (verdict, catalog) = self.cx.is_a_tracked(sub, sup);
        let c = match verdict {
            IsA::Yes => Certainty::Yes,
            IsA::No => Certainty::No,
            IsA::Unknown => Certainty::Maybe,
        };
        // A11: a decisive but catalog-backed verdict falls to `Unknown` on a minor
        // skew — the arm is then kept in both polarities (the FP-safe side).
        if self.demote_catalog && catalog && c != Certainty::Maybe { Certainty::Maybe } else { c }
    }

    fn is_final(&self, fqn: &str) -> bool {
        // Only an in-project `final` class or enum is provably closed; a builtin
        // (finality untracked in the catalog) stays open → the positive branch keeps
        // its arm (FP-safe).
        self.cx.find_class(fqn).is_some_and(|(_, cd)| cd.is_final || cd.is_enum)
    }
}

/// Contract acceptance (ADR-0030): does the proven value `v` inhabit the phpdoc
/// type `ty`? Class names in `ty` resolve in the callee file `cfile` at `coff`.
fn accepts(cx: &Cx, cfile: usize, coff: u32, ty: &PType, v: &CVal) -> Tri {
    match &ty.kind {
        PKind::Identifier(name) => accepts_identifier(cx, cfile, coff, name, v),
        PKind::This => Tri::Maybe, // `$this` — silent this slice
        PKind::Nullable(inner) => match v {
            CVal::Scalar(ArgValue::Null) => Tri::Yes,
            _ => accepts(cx, cfile, coff, inner, v),
        },
        // Union: `Yes` if any member accepts, `No` only if all definitely reject.
        PKind::Union { types, .. } => {
            let (mut any_yes, mut any_maybe) = (false, false);
            for t in types {
                match accepts(cx, cfile, coff, t, v) {
                    Tri::Yes => any_yes = true,
                    Tri::Maybe => any_maybe = true,
                    Tri::No => {}
                }
            }
            if any_yes {
                Tri::Yes
            } else if any_maybe {
                Tri::Maybe
            } else {
                Tri::No
            }
        }
        PKind::Intersection(_) => Tri::Maybe, // class intersections — silent
        // `T[]` — an array (any keys) whose values inhabit `T`.
        PKind::Array(inner) => match v {
            CVal::Array(entries) => {
                let mut r = Tri::Yes;
                for (_, cv) in entries {
                    r = combine(r, accepts(cx, cfile, coff, inner, cv));
                    if r == Tri::No {
                        return Tri::No;
                    }
                }
                r
            }
            _ => Tri::No,
        },
        PKind::Generic { base, args } => accepts_generic(cx, cfile, coff, base, args, v),
        PKind::ArrayShape(shape) => accepts_shape(cx, cfile, coff, shape, v),
        PKind::Const(c) => accepts_const(c, v),
        // Callables, offset-access, conditionals, object-shapes → silent.
        PKind::Callable(_) | PKind::OffsetAccess { .. } | PKind::Conditional(_)
        | PKind::ObjectShape(_) | PKind::Unsupported(_) => Tri::Maybe,
    }
}

/// Acceptance for a bare identifier type: the scalar keyword table, the string/int
/// predicate refinements, `mixed`/`scalar`/`array-key`/`object`, or a class name.
fn accepts_identifier(cx: &Cx, cfile: usize, coff: u32, name: &str, v: &CVal) -> Tri {
    let scalar = match v {
        CVal::Scalar(s) => Some(s),
        _ => None,
    };
    let yes_no = |b: bool| if b { Tri::Yes } else { Tri::No };
    match name.to_ascii_lowercase().as_str() {
        "mixed" => Tri::Yes,
        "int" => yes_no(matches!(scalar, Some(ArgValue::Int(_)))),
        // `int` is accepted by `float` (PHPStan core semantics).
        "float" => yes_no(matches!(scalar, Some(ArgValue::Float(_) | ArgValue::Int(_)))),
        "string" => yes_no(matches!(scalar, Some(ArgValue::Str(_)))),
        "bool" => yes_no(matches!(scalar, Some(ArgValue::Bool(_)))),
        "true" => yes_no(matches!(scalar, Some(ArgValue::Bool(true)))),
        "false" => yes_no(matches!(scalar, Some(ArgValue::Bool(false)))),
        "null" => yes_no(matches!(scalar, Some(ArgValue::Null))),
        "scalar" => yes_no(matches!(
            scalar,
            Some(ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Str(_) | ArgValue::Bool(_))
        )),
        "array-key" => yes_no(matches!(scalar, Some(ArgValue::Int(_) | ArgValue::Str(_)))),
        "positive-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i > 0),
            _ => Tri::No,
        },
        "negative-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i < 0),
            _ => Tri::No,
        },
        "non-negative-int" => match scalar {
            Some(ArgValue::Int(i)) => yes_no(*i >= 0),
            _ => Tri::No,
        },
        "numeric-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(php_is_numeric(s)),
            _ => Tri::No,
        },
        "non-empty-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(!s.is_empty()),
            _ => Tri::No,
        },
        "non-falsy-string" | "truthy-string" => match scalar {
            Some(ArgValue::Str(s)) => yes_no(!s.is_empty() && s != "0"),
            _ => Tri::No,
        },
        "array" => yes_no(matches!(v, CVal::Array(_))),
        "object" => yes_no(matches!(v, CVal::Object(..))),
        // `iterable`: a proven array satisfies it; an object might be Traversable
        // (unprovable) → silent; a scalar is silent too (not in the checked set).
        "iterable" => match v {
            CVal::Array(_) => Tri::Yes,
            _ => Tri::Maybe,
        },
        // Types we deliberately keep silent this slice (class-string, self/static,
        // callable-string, void/never, …).
        "class-string" | "self" | "static" | "parent" | "void" | "never" | "callable-string"
        | "interface-string" | "trait-string" | "enum-string" | "literal-string"
        | "callable" | "closure" | "resource" | "empty" | "value-of" | "key-of" => Tri::Maybe,
        // A class-name type (ADR-0043 stage 4). Rides the trinary is-a oracle for a
        // proven object value (`Yes`→Yes, `No`→No, `Unknown`→Maybe) and rejects a
        // proven scalar against a *known* class — phpdoc acceptance is pure set
        // membership (ADR-0030 registry 1, no coercion), so no scalar is ever a
        // class instance, in either mode. The `is_known_class` gate is the safety
        // valve: an unresolved bare identifier may be a `@template` param or a
        // `@phpstan-type` alias (which can denote a scalar), so it stays silent —
        // the same closure discipline the is-a oracle applies to non-membership.
        _ => {
            let target = cx.resolve_pclass(cfile, coff, name);
            match v {
                CVal::Object(obj, _) => match cx.is_a(obj, &target) {
                    IsA::Yes => Tri::Yes,
                    // A definite `No` requires a *known* target: an object whose own
                    // hierarchy is closed is-a-No against an unresolved name, but that
                    // name may be a `@template`/`@phpstan-type` alias the object *does*
                    // satisfy — so gate on `is_known_class`, as for the scalar arm.
                    IsA::No if cx.is_known_class(&target) => Tri::No,
                    IsA::No | IsA::Unknown => Tri::Maybe,
                },
                CVal::Scalar(_) if cx.is_known_class(&target) => Tri::No,
                // An array is likewise never a class instance, but it is left
                // silent this slice (out of the stage-4 scope).
                _ => Tri::Maybe,
            }
        }
    }
}

/// Acceptance for a literal constant type (`'foo'`, `123`, `1.5`, `true`, …) by
/// value equality; a const-fetch (`Foo::BAR`) is unresolved → silent.
fn accepts_const(c: &ConstExpr, v: &CVal) -> Tri {
    // A const-fetch type (`Foo::BAR`, `self::CONST`, or an enum-case type like
    // `Suit::Hearts`) is unresolved here — its denotation is unknown, so it must
    // stay silent for *every* value (ADR-0043 stage 4): an enum-case object or the
    // referenced literal may well inhabit it, and a returned/passed value that *is*
    // that very constant must never be manufactured into a `No`. This guards the
    // class-const value resolution (which now flows enum cases and const literals
    // into the contract check) from firing on `@return self::CONST { return
    // self::CONST; }` tautologies.
    if matches!(c, ConstExpr::Fetch { .. }) {
        return Tri::Maybe;
    }
    let scalar = match v {
        CVal::Scalar(s) => s,
        _ => return Tri::No,
    };
    let yes_no = |b: bool| if b { Tri::Yes } else { Tri::No };
    match c {
        ConstExpr::Int(s) => match (s.parse::<i64>().ok(), scalar) {
            (Some(n), ArgValue::Int(i)) => yes_no(*i == n),
            _ => Tri::No,
        },
        ConstExpr::Float(s) => match (s.parse::<f64>().ok(), scalar) {
            (Some(n), ArgValue::Float(f)) => yes_no(*f == n),
            _ => Tri::No,
        },
        ConstExpr::Str(lit) => match scalar {
            ArgValue::Str(s) => yes_no(s == string_lit_value(lit)),
            _ => Tri::No,
        },
        ConstExpr::True => yes_no(matches!(scalar, ArgValue::Bool(true))),
        ConstExpr::False => yes_no(matches!(scalar, ArgValue::Bool(false))),
        ConstExpr::Null => yes_no(matches!(scalar, ArgValue::Null)),
        ConstExpr::Fetch { .. } => Tri::Maybe,
    }
}

fn string_lit_value(lit: &StringLit) -> &str {
    match lit {
        StringLit::Single(s) | StringLit::Double(s) => s,
    }
}

/// Acceptance for a generic type: `array<…>`/`list<…>`/`non-empty-*<…>` (per the
/// phpstan#14939 list semantics), simple `int<lo, hi>` ranges; everything else
/// (`Collection<…>`, `iterable<…>`, template generics) is silent.
fn accepts_generic(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    base: &str,
    args: &[steins_phpdoc::ast::GenericArg],
    v: &CVal,
) -> Tri {
    let base_lc = base.to_ascii_lowercase();
    match base_lc.as_str() {
        "array" | "non-empty-array" | "list" | "non-empty-list" => {
            let CVal::Array(entries) = v else { return Tri::No };
            let non_empty = base_lc.starts_with("non-empty");
            let require_list = base_lc.ends_with("list");
            // list<V> / non-empty-list<V>: 1 arg (value). array<V>: 1 arg (value);
            // array<K, V>: 2 args (key, value).
            let (key_ty, val_ty) = match (require_list, args) {
                (_, [v1]) => (None, &v1.ty),
                (false, [k, v2]) => (Some(&k.ty), &v2.ty),
                (true, [_, v2]) => (None, &v2.ty), // list<int, V> is unusual; ignore key
                _ => return Tri::Maybe,
            };
            check_arraylike(cx, cfile, coff, entries, key_ty, val_ty, require_list, non_empty)
        }
        // `int<lo, hi>` with simple integer/`min`/`max` bounds.
        "int" => match (args, v) {
            ([lo, hi], CVal::Scalar(ArgValue::Int(i))) => {
                match (int_bound(&lo.ty, i64::MIN), int_bound(&hi.ty, i64::MAX)) {
                    (Some(lo), Some(hi)) => {
                        if *i >= lo && *i <= hi { Tri::Yes } else { Tri::No }
                    }
                    _ => Tri::Maybe,
                }
            }
            ([_, _], CVal::Scalar(_)) => Tri::No, // non-int can't inhabit an int range
            _ => Tri::Maybe,
        },
        // A class-level generic `Class<A, …>` (ADR-0032 tier 3, issue #10).
        _ => accepts_class_generic(cx, cfile, coff, base, args, v),
    }
}

/// Acceptance of a value against a class-level generic contract `Class<A, …>`
/// (ADR-0032 tier 3, issue #10). The class half rides the trinary is-a oracle
/// exactly as the bare-class identifier path; the argument half judges ONLY when
/// the object carries provable, arity-matched per-position type-argument values
/// (from `new`-site propagation — see [`Cx::infer_generic_args`]).
///
/// Honesty bounds (zero-FP, stage 1):
/// - A **non-object** value is silent (`Maybe`): the bare-class identifier path
///   owns scalar-vs-class `No`; a *generic* spelling never manufactures it here.
/// - The class half only **gates**: a `No`/`Unknown` is-a answers `Maybe`, never a
///   manufactured `No` — generic-class *class-mismatch* reporting is deferred, so
///   the sole `No` this arm yields comes from a provable **argument-half** violation
///   on an object that **is** the required class.
/// - An **empty** carry or an **arity mismatch** between declared arguments and
///   carried values answers `Maybe` (no provable knowledge / library-author
///   inconsistency stays a thin lint, per ADR-0032).
fn accepts_class_generic(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    base: &str,
    args: &[steins_phpdoc::ast::GenericArg],
    v: &CVal,
) -> Tri {
    let CVal::Object(obj_class, targs) = v else { return Tri::Maybe };
    let target = cx.resolve_pclass(cfile, coff, base);
    // Class half: proceed only on a proven is-a; otherwise stay silent.
    if cx.is_a(obj_class, &target) != IsA::Yes {
        return Tri::Maybe;
    }
    // Argument half: needs provable, arity-matched per-position knowledge.
    if targs.is_empty() || targs.len() != args.len() {
        return Tri::Maybe;
    }
    let mut r = Tri::Yes;
    for (declared, actual) in args.iter().zip(targs.iter()) {
        r = combine(r, accepts(cx, cfile, coff, &declared.ty, actual));
        if r == Tri::No {
            return Tri::No;
        }
    }
    r
}

/// The bound of a simple `int<…>` argument: an integer literal, or `min`/`max`
/// keywords (mapped to `default`). Anything else → `None` (not a simple bound).
fn int_bound(ty: &PType, default: i64) -> Option<i64> {
    match &ty.kind {
        PKind::Const(ConstExpr::Int(s)) => s.parse::<i64>().ok(),
        PKind::Identifier(name) if name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max") => {
            Some(default)
        }
        _ => None,
    }
}

/// Membership for an `array`/`list` generic (per phpstan#14939): a value is a list
/// iff its normalized keys are exactly `0..n-1` in order; element (and, for
/// `array<K, V>`, key) membership is checked recursively; an uncertain element
/// makes the whole check `Maybe` (silent).
#[allow(clippy::too_many_arguments)]
fn check_arraylike(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    entries: &[(NormKey, CVal)],
    key_ty: Option<&PType>,
    val_ty: &PType,
    require_list: bool,
    non_empty: bool,
) -> Tri {
    if non_empty && entries.is_empty() {
        return Tri::No;
    }
    if require_list && !is_list_shaped(entries) {
        return Tri::No;
    }
    let mut r = Tri::Yes;
    for (k, cv) in entries {
        if let Some(kt) = key_ty {
            r = combine(r, accepts(cx, cfile, coff, kt, &normkey_cval(k)));
            if r == Tri::No {
                return Tri::No;
            }
        }
        r = combine(r, accepts(cx, cfile, coff, val_ty, cv));
        if r == Tri::No {
            return Tri::No;
        }
    }
    r
}

/// Whether normalized `entries` form a list: keys exactly `0, 1, …, n-1` in order.
fn is_list_shaped(entries: &[(NormKey, CVal)]) -> bool {
    entries
        .iter()
        .enumerate()
        .all(|(i, (k, _))| matches!(k, NormKey::Int(n) if *n == i as i64))
}

/// A normalized key as a scalar [`CVal`] (for key membership).
fn normkey_cval(k: &NormKey) -> CVal {
    match k {
        NormKey::Int(i) => CVal::Scalar(ArgValue::Int(*i)),
        NormKey::Str(s) => CVal::Scalar(ArgValue::Str(s.clone())),
    }
}

/// Membership for an array-shape / list-shape (per phpstan#14939): `array{…}` is an
/// order-agnostic required-key map (optional `?` keys may be absent; sealed unless
/// `…`); `list{…}` is positional. A missing required key, a definite element-type
/// violation, or an extra key in a sealed shape → `No`. An unresolvable shape key
/// (a const-fetch) makes the whole check `Maybe`.
fn accepts_shape(cx: &Cx, cfile: usize, coff: u32, shape: &steins_phpdoc::ast::ArrayShape, v: &CVal) -> Tri {
    let CVal::Array(entries) = v else { return Tri::No };
    let non_empty =
        matches!(shape.kind, ArrayShapeKind::NonEmptyArray | ArrayShapeKind::NonEmptyList);
    if non_empty && entries.is_empty() {
        return Tri::No;
    }
    let require_list = matches!(shape.kind, ArrayShapeKind::List | ArrayShapeKind::NonEmptyList);
    if require_list && !is_list_shaped(entries) {
        return Tri::No;
    }

    // Assign each shape item its normalized key (positional next-int for keyless
    // items, explicit keys otherwise). An unresolvable key → the whole shape maybe.
    let mut expected: Vec<(NormKey, &PType, bool)> = Vec::with_capacity(shape.items.len());
    let mut next_auto: i64 = 0;
    for item in &shape.items {
        let key = match &item.key {
            None => {
                let k = NormKey::Int(next_auto);
                next_auto += 1;
                k
            }
            Some(sk) => {
                let Some(k) = shape_key_norm(sk) else { return Tri::Maybe };
                if let NormKey::Int(i) = k
                    && i >= next_auto
                {
                    next_auto = i + 1;
                }
                k
            }
        };
        expected.push((key, &item.value, item.optional));
    }

    let mut r = Tri::Yes;
    let mut used: HashSet<NormKey> = HashSet::new();
    for (k, ety, optional) in &expected {
        used.insert(k.clone());
        match entries.iter().find(|(ek, _)| ek == k) {
            Some((_, cv)) => {
                r = combine(r, accepts(cx, cfile, coff, ety, cv));
                if r == Tri::No {
                    return Tri::No;
                }
            }
            None => {
                if !optional {
                    return Tri::No; // missing required key
                }
            }
        }
    }

    // Extra keys: a sealed shape rejects them (PHPStan parity); an unsealed `…<V>`
    // checks their values against the tail type.
    if shape.sealed {
        if entries.iter().any(|(k, _)| !used.contains(k)) {
            return Tri::No;
        }
    } else if let Some(u) = &shape.unsealed {
        for (k, cv) in entries {
            if !used.contains(k) {
                r = combine(r, accepts(cx, cfile, coff, &u.value, cv));
                if r == Tri::No {
                    return Tri::No;
                }
            }
        }
    }
    r
}

/// The normalized runtime key a phpdoc shape key denotes, or `None` for an
/// unresolvable const-fetch key. Bareword and string keys fold integer-like
/// spellings to `Int` (PHP key normalization).
fn shape_key_norm(k: &ShapeKey) -> Option<NormKey> {
    match k {
        ShapeKey::Int(s) => s.parse::<i64>().ok().map(NormKey::Int),
        ShapeKey::Str(lit) => Some(norm_str_key(string_lit_value(lit))),
        ShapeKey::Ident(s) => Some(norm_str_key(s)),
        ShapeKey::ConstFetch { .. } => None,
    }
}

/// Fold an integer-like string key to `Int`, else keep it a `Str` key.
fn norm_str_key(s: &str) -> NormKey {
    match s.parse::<i64>() {
        Ok(i) if i.to_string() == s => NormKey::Int(i),
        _ => NormKey::Str(s.to_owned()),
    }
}

/// The phpdoc contract-acceptance check for one argument at a call site. Runs only
/// when the native check did **not** fire at this site (no double-report). Reports
/// `phpdoc.param-mismatch` iff the proven value provably does not inhabit the
/// `@param` type. `cfile`/`coff` locate the callee's docblock context (class-name
/// resolution). Returns nothing for `Maybe`/`Yes`.
///
/// # Assertion-helper exemption (ADR-0030)
///
/// A function/method whose docblock carries an assertion tag (`@phpstan-assert`
/// and its `-if-true`/`-if-false`/negated variants) targeting parameter `$x` is an
/// **assertion helper for `$x`**: its `@param` for `$x` states a *post*-condition
/// that the helper establishes about `$x`, not a precondition its callers must
/// satisfy. Checking a call-site argument against it is therefore semantically
/// wrong — the whole point of such a helper is to be called with a *wider* value
/// (e.g. `mixed`/`string|int`) and to narrow it. So we skip `phpdoc.param-mismatch`
/// for that parameter. This holds for all three assert kinds and the negated form.
///
/// Scope of the exemption, deliberately narrow:
/// - **Other parameters** of the same function are still checked (the tag exempts
///   only its own target).
/// - **`@return`** checking is unaffected (a different relation entirely).
/// - **Native** runtime checks are unaffected: a native type hint is a real
///   runtime gate regardless of any docblock assertion, so it still fires (and,
///   firing first, already suppresses this phpdoc check at that site).
///
/// This slice does **not** implement the *positive* refinement effect — applying
/// the asserted type to the caller's environment after the call. That is a
/// branch-analysis capability and lands with the structured trace tree / value
/// domain (ADR-0031, ADR-0035); here we only suppress the incorrect precondition
/// reading.
#[allow(clippy::too_many_arguments)]
fn check_phpdoc_param(
    cx: &Cx,
    folder: &mut dyn Folder,
    envelopes: &Envelopes,
    param: &Param,
    cfile: usize,
    coff: u32,
    callee: &str,
    arg_offset: u32,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) {
    // Assertion-helper exemption (see the doc comment above): this parameter's
    // `@param` is a post-condition, so a call-site argument cannot violate it.
    if envelopes.is_assert_target(&param.name) {
        return;
    }
    let Some(ty) = envelopes.param(&param.name) else { return };
    let param_name = &param.name;
    let rendered = match cx.resolve_cval(value, env, store, poisoned, folder) {
        Some(cv) => {
            // A parameter that is nullable by its native type, or implicitly nullable
            // via a `= null` default, accepts `null` regardless of a non-nullable
            // `@param` spelling — PHP/PHPStan honor this, so reporting it would be a
            // false positive.
            if matches!(cv, CVal::Scalar(ArgValue::Null))
                // ADR-0043 stage 1: consult native nullability only for scalar-value
                // types. An object-bearing type contributes no native-nullable signal
                // here (it lowered to `None` before ADR-0043), so `?Foo` does not
                // change which `null` arguments this guard accepts.
                && (param.has_null_default
                    || param.ty.as_ref().is_some_and(|t| t.nullable && !t.has_instance()))
            {
                return;
            }
            if accepts(cx, cfile, coff, ty, &cv) != Tri::No {
                return;
            }
            // ADR-0043 stage 4: a class-touching verdict is guard-blind inside a
            // binding descent (mirror of the native `object_world_guard_blind`) —
            // the callee's in-body type guards that would narrow the rebound value
            // are unmodeled. Scalar-vs-scalar phpdoc checks stay live.
            if phpdoc_object_guard_blind(in_descent, ty, Some(&cv)) {
                return;
            }
            rendered_cval(&cv)
        }
        // Abstract-fact path (Feature E, ADR-0030/0035): an argument that resolves
        // to an abstract fact (not a proven value — e.g. a native-seeded param or a
        // guard-refined var) is judged by the domain's **set** acceptance via
        // `steins_contract::admits_fact`. Only a definite `No` (every value the fact
        // admits is rejected) reports; `Maybe` is silent.
        None => {
            let Some(fact) = arg_abstract_fact(value, env, poisoned) else { return };
            let cty = steins_contract::lower(ty);
            // ADR-0043 stage 4 — the class valve. A class-touching contract used to
            // stay silent against every fact. It opens for exactly one sound case:
            // a **pure class contract of known classes** (`Foo`, `Foo|null`, `A|B`)
            // against a definite scalar fact. The abstract-fact domain is scalar-only
            // (ADR-0035/0038 — no object inhabitant), so any fact here is a definite
            // scalar, and a scalar is never a member of a class type (pure set
            // membership, no coercion). It stays shut for an unknown identifier — a
            // `@template` param or `@phpstan-type` alias may denote a scalar — and,
            // like the proven path, for any class-touching verdict inside a descent.
            let open_class_valve = is_pure_class_contract(cx, cfile, coff, ty)
                && !phpdoc_object_guard_blind(in_descent, ty, None);
            if contract_touches_class(&cty) && !open_class_valve {
                return;
            }
            if steins_contract::admits_fact(&cty, fact) != Certainty::No {
                return;
            }
            describe_fact(fact)
        }
    };
    let pos = cx.tree().position(arg_offset);
    let message = format!(
        "argument {rendered} to {callee}() violates declared @param {ty} ${param_name} — declared contract violation",
    );
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
    });
}

/// The kind of callable-signature incompatibility a bound closure / first-class
/// callable exhibits against a declared `callable(...)` contract (issue #11).
#[derive(Debug, Clone, Copy)]
enum CallableViolation {
    /// The closure's declared parameter at this position is narrower than the
    /// contract supplies (parameter contravariance broken).
    Param(usize),
    /// The closure's declared return is provably incompatible with the contract's
    /// (return covariance broken).
    Return,
    /// The closure requires more parameters than the contract supplies, so the
    /// callee's invocation would `ArgumentCountError` (arity).
    Arity,
}

/// Lower a native scalar/union type to a [`ContractTy`] for the callable-signature
/// variance check (issue #11). Scalars and bool-literals map to their contract
/// arm; an object member maps to a class arm (which [`normalize::subsumes`] judges
/// only reflexively, so cross-class comparisons stay `Maybe` — silent); a nullable
/// hint adds a `null` arm. A [`NativeType`] is always representable — the syntax
/// layer already dropped `mixed`/`iterable`/`callable`/intersection hints to
/// `None`, so nothing here needs an `Opaque` escape.
fn native_to_contract(nt: &NativeType) -> ContractTy {
    let mut arms: Vec<ContractTy> = nt
        .members
        .iter()
        .map(|m| match m {
            TypeMember::Scalar(ScalarType::Int) => ContractTy::Base(steins_domain::Base::Int),
            TypeMember::Scalar(ScalarType::Float) => ContractTy::Base(steins_domain::Base::Float),
            TypeMember::Scalar(ScalarType::String) => ContractTy::Base(steins_domain::Base::String),
            TypeMember::Scalar(ScalarType::Bool) => ContractTy::Base(steins_domain::Base::Bool),
            TypeMember::BoolLiteral(b) => ContractTy::LitBool(*b),
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
            TypeMember::InstanceInter(cs) => {
                ContractTy::Inter(cs.iter().map(|c| ContractTy::Class(c.fqn.clone())).collect())
            }
        })
        .collect();
    if nt.nullable {
        arms.push(ContractTy::Null);
    }
    match arms.len() {
        1 => arms.pop().expect("len checked"),
        _ => ContractTy::Union(arms),
    }
}

/// Whether a contract arm is decidable by the **scalar** overlap relation — the
/// only positions the callable-variance check will fire a definite `No` on
/// (issue #11). A bare identifier in a callable signature (`callable(T): T`) is
/// syntactically indistinguishable from a class name, and is far more often an
/// unbound `@template` than a real class (ADR-0032/0051 — no call-site template
/// solver), so a `Class`/`ObjectAny`/`Opaque`/array/callable arm is treated as
/// undecidable here and stays silent (zero-FP). Only scalar/literal/null arms —
/// where `subsumes` gives a sound `No` and no template can hide — are judged.
/// `StrOpaque` (`class-string` et al.) and `Mixed` never yield a decidable `No`
/// anyway, so excluding them costs nothing.
fn scalar_decidable(ty: &ContractTy) -> bool {
    match ty {
        ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::Null
        | ContractTy::Never => true,
        ContractTy::Union(m) | ContractTy::Inter(m) => m.iter().all(scalar_decidable),
        _ => false,
    }
}

/// Judge a bound closure's declared native signature against a `callable(...)`
/// contract (issue #11), returning the first definite incompatibility or `None`
/// when compatible or undecidable (zero-FP silence).
///
/// This is the **declared-contract** relation (ADR-0030 divergence #1 — envelope
/// checking, no runtime coercion; PHP does *not* enforce a `callable(int): string`
/// docblock at runtime, verified with `php -r`, so the claim is contract-layer),
/// and it reuses the single overlap relation [`normalize::subsumes`] (the
/// `isSuperTypeOf` shape) as its comparator rather than a bespoke one:
///
/// - **Parameters are contravariant.** At each contract position the closure's
///   declared parameter must accept everything the contract supplies:
///   `subsumes(closure_param, contract_param)`. A closure accepting WIDER than the
///   contract is fine; one requiring NARROWER is the violation. Only a definite
///   `No` (a scalar mismatch such as `string` vs `int`) reports; an undeclared
///   parameter, a template, or a cross-class comparison is `Maybe` → silent. A
///   by-reference position (either side) is skipped — by-ref callable semantics
///   are unverified, so Steins stays silent (zero-FP).
/// - **Return is covariant.** The closure's declared return must be subsumed by
///   the contract's: `subsumes(contract_ret, closure_ret)`. A closure returning
///   narrower/equal is fine; a provably-disjoint return (e.g. `int` vs `string`)
///   is the violation. Only a definite `No` reports; an undeclared return is silent.
/// - **Arity.** A closure REQUIRING more parameters (no default, non-variadic)
///   than the contract supplies would `ArgumentCountError` when the callee invokes
///   it with the contract's arity — verified against PHP 8.5 (`Too few arguments`).
///   PHP ignores surplus arguments, so a closure with FEWER params, or extra
///   OPTIONAL/variadic params, is fine. Skipped when the contract is itself
///   variadic (the callee may pass any number of arguments).
fn callable_sig_violation(
    sig: &steins_contract::CallableSig,
    closure_params: &[Param],
    closure_ret: Option<&NativeType>,
) -> Option<CallableViolation> {
    // Parameter contravariance, positional.
    for (i, cparam) in sig.params.iter().enumerate() {
        if cparam.by_ref || cparam.variadic {
            continue;
        }
        let Some(closure_param) = closure_params.get(i) else { continue };
        if closure_param.by_ref {
            continue;
        }
        let Some(pty) = closure_param.ty.as_ref() else { continue };
        let closure_ty = native_to_contract(pty);
        if scalar_decidable(&closure_ty)
            && scalar_decidable(&cparam.ty)
            && normalize::subsumes(&closure_ty, &cparam.ty) == Certainty::No
        {
            return Some(CallableViolation::Param(i));
        }
    }
    // Return covariance.
    if let Some(ret) = closure_ret {
        let closure_ret_ty = native_to_contract(ret);
        if scalar_decidable(&sig.ret)
            && scalar_decidable(&closure_ret_ty)
            && normalize::subsumes(&sig.ret, &closure_ret_ty) == Certainty::No
        {
            return Some(CallableViolation::Return);
        }
    }
    // Arity: the closure demands more parameters than the contract will supply.
    let contract_variadic = sig.params.iter().any(|p| p.variadic);
    if !contract_variadic {
        let required =
            closure_params.iter().filter(|p| !p.has_default && !p.variadic).count();
        if required > sig.params.len() {
            return Some(CallableViolation::Arity);
        }
    }
    None
}

/// Check a closure / first-class-callable argument at a call site against a
/// declared `callable(...)` `@param` contract (issue #11), emitting at most one
/// `phpdoc.param-mismatch`. Silent unless the contract carries a signature AND the
/// bound callable's declared *native* signature (Verified — ADR-0052 N2) provably
/// violates it. Reuses the `phpdoc.param-mismatch` lane: a callable argument that
/// breaks the declared callable signature *is* a violation of the callee's
/// `@param $callback` (id-choice recorded in the commit body).
///
/// The closure's declared signature is a static CST fact — it does not depend on
/// the call-site environment (captures do not change the parameter/return hints),
/// so this rides the env-free direct pass (no overlap with the propagation pass).
fn check_callable_arg(
    cx: &Cx,
    envelopes: &Envelopes,
    param: &Param,
    callee: &str,
    arg_offset: u32,
    closure: &ClosureRef,
    out: &mut Vec<Diagnostic>,
) {
    let Some(ty) = envelopes.param(&param.name) else { return };
    let ContractTy::CallableTy(Some(sig)) = steins_contract::lower(ty) else { return };

    // Resolve the bound callable's declared native signature. Anonymous closures
    // address their own scope by definition offset; a first-class callable naming
    // a user function reuses the function-resolution leg (S5) — a builtin or
    // unresolvable name has no ground-truth signature, so it stays silent (Maybe).
    let (closure_params, closure_ret): (&[Param], Option<&NativeType>) = match closure {
        ClosureRef::Anonymous { def_offset, .. } => {
            let Some(scope) = cx.closure_scope(*def_offset) else { return };
            (scope.params.as_slice(), scope.ret_ty.as_ref())
        }
        ClosureRef::FunctionName(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                let decl = cx.fn_decl(site);
                (decl.params.as_slice(), decl.ret.as_ref())
            }
            _ => return,
        },
    };

    let Some(violation) = callable_sig_violation(&sig, closure_params, closure_ret) else {
        return;
    };
    let param_name = &param.name;
    let message = match violation {
        CallableViolation::Param(i) => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — parameter #{} type is incompatible (callable parameter contravariance)",
            i + 1,
        ),
        CallableViolation::Return => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — return type is incompatible (callable return covariance)",
        ),
        CallableViolation::Arity => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — it requires more parameters than the callable signature supplies",
        ),
    };
    let pos = cx.tree().position(arg_offset);
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
    });
}

/// The abstract fact an argument resolves to: a bare `$var` whose env fact is an
/// abstract layer (no finite members). Finite/proven values go through
/// `resolve_cval` instead, so this is the disjoint "abstract" arm of Feature E.
fn arg_abstract_fact<'e>(
    value: &ArgValue,
    env: &'e HashMap<String, Known>,
    poisoned: bool,
) -> Option<&'e Fact> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let f = env.get(name)?.fact.as_ref()?;
    f.finite_members().is_none().then_some(f)
}

/// Whether a lowered contract type contains a class-name node — a bare identifier
/// that may actually be a template or a type-alias. The abstract-fact check stays
/// silent on these (see [`check_phpdoc_param`]).
fn contract_touches_class(ty: &steins_contract::ContractTy) -> bool {
    use steins_contract::ContractTy as C;
    match ty {
        C::Class(_) => true,
        C::Union(m) | C::Inter(m) => m.iter().any(contract_touches_class),
        C::ListOf { elem, .. } => contract_touches_class(elem),
        C::MapOf { key, val, .. } | C::IterableOf { key, val } => {
            contract_touches_class(key) || contract_touches_class(val)
        }
        C::Shape { fields, unsealed, .. } => {
            fields.iter().any(|f| contract_touches_class(&f.ty))
                || unsealed.as_ref().is_some_and(|(k, v)| {
                    k.as_ref().is_some_and(|k| contract_touches_class(k))
                        || contract_touches_class(v)
                })
        }
        _ => false,
    }
}

/// ADR-0043 stage 4 — the phpdoc-side analogue of [`object_world_guard_blind`]. A
/// class-touching phpdoc verdict is unsound inside a binding descent: the callee's
/// in-body type guards on the rebound value are unmodeled (the same reason the
/// native object-world check is suppressed there). "Touches a class" means the
/// proven value is an object, or the contract references a class name (a bare
/// identifier the lowering treats as a class). Scalar-vs-scalar phpdoc checks —
/// whose guards the walk *can* evaluate — are unaffected. Always `false` outside a
/// descent.
fn phpdoc_object_guard_blind(in_descent: bool, ty: &PType, cv: Option<&CVal>) -> bool {
    in_descent
        && (matches!(cv, Some(CVal::Object(..)))
            || contract_touches_class(&steins_contract::lower(ty)))
}

/// ADR-0043 stage 4 — is `ty` a **pure class contract**: a known class name, or a
/// union/nullable built only from known class names and `null` (e.g. `Foo`,
/// `Foo|null`, `?Foo`, `A|B`)? Only such a contract may let a definite scalar fact
/// open the [`contract_touches_class`] valve. The `is_known_class` gate is the
/// safety valve — an unresolved bare identifier may be a `@template` param or a
/// `@phpstan-type` alias (which can denote a scalar), so it disqualifies the whole
/// contract. A contract touching an array/generic/shape/intersection/callable, or
/// any scalar/pseudo-type keyword, is *not* pure-class (returns `false`): those
/// cases keep the existing silence.
fn is_pure_class_contract(cx: &Cx, cfile: usize, coff: u32, ty: &PType) -> bool {
    fn walk(cx: &Cx, cfile: usize, coff: u32, ty: &PType, saw_class: &mut bool) -> bool {
        match &ty.kind {
            PKind::Identifier(name) => {
                // A `null` companion (the `class|null` shape) is allowed but is not
                // itself the class that satisfies the "at least one class" rule.
                if name.eq_ignore_ascii_case("null") {
                    return true;
                }
                let target = cx.resolve_pclass(cfile, coff, name);
                if cx.is_known_class(&target) {
                    *saw_class = true;
                    true
                } else {
                    false
                }
            }
            PKind::Nullable(inner) => walk(cx, cfile, coff, inner, saw_class),
            PKind::Union { types, .. } => {
                types.iter().all(|t| walk(cx, cfile, coff, t, saw_class))
            }
            _ => false,
        }
    }
    let mut saw_class = false;
    walk(cx, cfile, coff, ty, &mut saw_class) && saw_class
}

/// A short, phpdoc-flavored description of an abstract fact for a diagnostic
/// message (`a value of type int`, `a non-empty-string value`, `an int|null
/// value`). Finite facts never reach here (they render as concrete values).
fn describe_fact(f: &Fact) -> String {
    let base_kw = |b: Base| match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    };
    let (name, nullable) = match f {
        Fact::General { base, nullable } => (base_kw(*base).to_owned(), *nullable),
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            let n = if *r == IntRange::POSITIVE {
                "positive-int".to_owned()
            } else if *r == IntRange::NEGATIVE {
                "negative-int".to_owned()
            } else if *r == IntRange::NON_NEGATIVE {
                "non-negative-int".to_owned()
            } else {
                format!("int<{}, {}>", r.lo(), r.hi())
            };
            (n, *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            let n = if p.contains_all(StrPreds::NON_FALSY) {
                "non-falsy-string"
            } else if p.contains_all(StrPreds::NUMERIC) {
                "numeric-string"
            } else if p.contains_all(StrPreds::NON_EMPTY) {
                "non-empty-string"
            } else {
                "string"
            };
            (n.to_owned(), *nullable)
        }
        Fact::Refined { base, nullable, .. } => (base_kw(*base).to_owned(), *nullable),
        // Finite facts do not reach here.
        Fact::Singleton(_) | Fact::OneOf(_) => ("value".to_owned(), false),
    };
    if nullable {
        format!("a value of type {name}|null")
    } else {
        format!("a value of type {name}")
    }
}

/// Render a proven [`CVal`] for a diagnostic message (delegates arrays/scalars to
/// [`ArgValue::render`]; objects show `new Class()`).
fn rendered_cval(v: &CVal) -> String {
    match v {
        CVal::Scalar(s) => s.render(),
        CVal::Object(class, _) => format!("new {}()", class.rsplit('\\').next().unwrap_or(class)),
        CVal::Array(entries) => {
            // Rebuild an `ArgValue::Array` with explicit keys so the shared compact
            // renderer applies (it re-normalizes; explicit keys round-trip).
            let items: Vec<(ArrayKey, ArgValue)> = entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect();
            ArgValue::Array(items).render()
        }
    }
}

/// A best-effort [`ArgValue`] reconstruction of a [`CVal`], for rendering only.
fn cval_to_argvalue(v: &CVal) -> ArgValue {
    match v {
        CVal::Scalar(s) => s.clone(),
        CVal::Object(..) => ArgValue::Other,
        CVal::Array(entries) => ArgValue::Array(
            entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod domain_tests {
    //! Unit tests for the ADR-0031/0035 domain skeleton: the unified [`Certainty`]
    //! algebra, [`Fact`] joins (agree / OneOf / cap overflow), and the empirically
    //! settled PHP comparison primitives.
    use super::*;
    use steins_syntax::ArgValue;

    fn sing(v: ArgValue) -> Fact {
        singleton_fact(&v).expect("literal converts")
    }

    #[test]
    fn certainty_algebra() {
        use Certainty::{Maybe, No, Yes};
        // not swaps the poles, fixes Maybe.
        assert_eq!(Yes.not(), No);
        assert_eq!(No.not(), Yes);
        assert_eq!(Maybe.not(), Maybe);
        // and: No dominates, then Maybe.
        assert_eq!(Yes.and(Yes), Yes);
        assert_eq!(Yes.and(No), No);
        assert_eq!(Yes.and(Maybe), Maybe);
        assert_eq!(No.and(Maybe), No);
        // or: Yes dominates, then Maybe.
        assert_eq!(No.or(No), No);
        assert_eq!(No.or(Yes), Yes);
        assert_eq!(No.or(Maybe), Maybe);
        assert_eq!(Yes.or(Maybe), Yes);
    }

    #[test]
    fn fact_join_agree_keeps_singleton() {
        // The env now stores `steins_domain::Fact`; joins go through the domain
        // algebra. Equal singletons stay a Singleton and resolve to the value.
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(5))).unwrap();
        assert!(matches!(j, Fact::Singleton(Val::Int(5))));
        let k = Known::value(j, 0, None);
        assert_eq!(k.singleton(), Some(ArgValue::Int(5)));
    }

    #[test]
    fn fact_join_differ_forms_oneof_and_dedups() {
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j, Fact::OneOf(vs) if vs.len() == 2));
        // A OneOf never resolves to a single proven value.
        assert_eq!(Known::value(j.clone(), 0, None).singleton(), None);
        // Re-joining an already-present value dedups.
        let j2 = j.join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j2, Fact::OneOf(vs) if vs.len() == 2));
    }

    #[test]
    fn fact_join_overflow_widens_to_refined() {
        // Beyond the OneOf cap the domain widens to a *computed* Refined summary
        // (an int interval), rather than dropping — abstract facts now flow
        // through the env (ADR-0035 stage 2). The widened fact resolves no value.
        let full = Fact::from_vals((0..steins_domain::CAP as i64).map(Val::Int).collect()).unwrap();
        assert!(matches!(full, Fact::OneOf(_)));
        let widened = full.join(&sing(ArgValue::Int(999))).unwrap();
        assert!(matches!(widened, Fact::Refined { base: Base::Int, .. }));
        assert_eq!(Known::value(widened, 0, None).singleton(), None);
    }

    #[test]
    fn loose_eq_measured_cells_php_8_5_8() {
        use ArgValue::{Bool, Int, Null, Str};
        let s = |x: &str| Str(x.to_owned());
        // A representative slice of the recorded PHP 8.5.8 table.
        assert_eq!(php_loose_eq(&Null, &Null), Some(true));
        assert_eq!(php_loose_eq(&Null, &Int(0)), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("")), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("0")), Some(false)); // the PHP 8 trap
        assert_eq!(php_loose_eq(&Null, &Bool(false)), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("0")), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("abc")), Some(false));
        assert_eq!(php_loose_eq(&Bool(true), &s("abc")), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("abc")), Some(false)); // PHP 8, not PHP 7
        assert_eq!(php_loose_eq(&Int(0), &s("0")), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("")), Some(false));
        assert_eq!(php_loose_eq(&s("0"), &s("")), Some(false));
        assert_eq!(php_loose_eq(&s("5"), &s("5")), Some(true));
        assert_eq!(php_loose_eq(&s("5"), &Int(5)), Some(true));
    }

    #[test]
    fn truthiness_edge_cells() {
        use ArgValue::{Array, Float, Int, Null, Str};
        assert_eq!(php_truthy(&Str("0".to_owned())), Some(false)); // "0" is falsy
        assert_eq!(php_truthy(&Str("0.0".to_owned())), Some(true)); // but "0.0" is truthy
        assert_eq!(php_truthy(&Str(String::new())), Some(false));
        assert_eq!(php_truthy(&Int(0)), Some(false));
        assert_eq!(php_truthy(&Float(0.0)), Some(false));
        assert_eq!(php_truthy(&Null), Some(false));
        assert_eq!(php_truthy(&Array(vec![])), Some(false)); // [] is falsy
    }

    #[test]
    fn identical_is_type_strict() {
        use ArgValue::{Float, Int};
        assert_eq!(php_identical(&Int(5), &Int(5)), Some(true));
        assert_eq!(php_identical(&Int(5), &Float(5.0)), Some(false)); // 5 === 5.0 is false
    }
}

#[cfg(test)]
mod oracle_tests {
    //! Unit tests for the ADR-0043 §3 trinary is-a oracle ([`Cx::is_a`]): the
    //! parent chain, the transitive `implements` closure, interface-extends, the
    //! builtin exception tree, the enum interface roots, the closed-set `No`, and
    //! every `Unknown` condition. The oracle is exercised directly against a
    //! one-file project so its verdicts are asserted without routing through
    //! instanceof branch analysis (integration tests cover that path separately).
    use super::*;

    fn is_a(src: &str, sub: &str, sup: &str) -> IsA {
        let tree = SourceTree::parse(src);
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        Cx::new(&units, &index, 0).is_a(sub, sup)
    }

    #[test]
    fn reflexive_and_parent_chain() {
        let src = "<?php class A {} class B extends A {} class C extends B {}";
        assert_eq!(is_a(src, "c", "c"), IsA::Yes, "reflexive");
        assert_eq!(is_a(src, "c", "a"), IsA::Yes, "grandparent via chain");
        assert_eq!(is_a(src, "b", "a"), IsA::Yes);
        // Fully enumerated, unrelated direction → No.
        assert_eq!(is_a(src, "a", "c"), IsA::No, "a is not a c (closed set)");
    }

    #[test]
    fn transitive_implements_and_interface_extends() {
        let src = "<?php
interface I {}
interface J extends I {}
class Base implements J {}
class Foo extends Base {}";
        assert_eq!(is_a(src, "foo", "j"), IsA::Yes, "class implements via parent");
        assert_eq!(is_a(src, "foo", "i"), IsA::Yes, "transitive interface-extends");
        assert_eq!(is_a(src, "base", "i"), IsA::Yes);
        assert_eq!(is_a(src, "j", "i"), IsA::Yes, "interface extends interface");
        // A class with no relation to K, fully enumerated → No.
        let src2 = "<?php interface I {} interface K {} class Foo implements I {}";
        assert_eq!(is_a(src2, "foo", "k"), IsA::No);
    }

    #[test]
    fn builtin_exception_tree_closed() {
        let src = "<?php class MyEx extends \\RuntimeException {}";
        // Chain leaves the project into the catalogued exception tree — enumerated.
        assert_eq!(is_a(src, "myex", "runtimeexception"), IsA::Yes);
        assert_eq!(is_a(src, "myex", "exception"), IsA::Yes);
        assert_eq!(is_a(src, "myex", "throwable"), IsA::Yes);
        // A catalogued exception is provably NOT a LogicException (both under the
        // fully-known SPL tree).
        assert_eq!(is_a(src, "myex", "logicexception"), IsA::No);
        // PHP 8.0+: `Throwable extends Stringable`, so every Throwable IS-A
        // Stringable (verified against PHP 8.5). A `No` here would be unsound.
        assert_eq!(is_a(src, "myex", "stringable"), IsA::Yes);
    }

    #[test]
    fn enum_is_a_its_interfaces_and_roots() {
        let src = "<?php
interface HasLabel {}
enum Suit: string implements HasLabel { case H = 'h'; }
enum Dir { case Up; }";
        // A backed enum is-a UnitEnum, BackedEnum, and its explicit interface.
        assert_eq!(is_a(src, "suit", "unitenum"), IsA::Yes);
        assert_eq!(is_a(src, "suit", "backedenum"), IsA::Yes);
        assert_eq!(is_a(src, "suit", "haslabel"), IsA::Yes);
        // A pure enum is-a UnitEnum but NOT BackedEnum (closed enumeration).
        assert_eq!(is_a(src, "dir", "unitenum"), IsA::Yes);
        assert_eq!(is_a(src, "dir", "backedenum"), IsA::No);
        assert_eq!(is_a(src, "dir", "haslabel"), IsA::No);
    }

    #[test]
    fn unknown_when_chain_leaves_project() {
        // Parent is an uncatalogued external → enumeration incomplete → Unknown.
        let src = "<?php class Foo extends \\Vendor\\Base {}";
        assert_eq!(is_a(src, "foo", "vendor\\base"), IsA::Yes, "the named parent is still Yes");
        assert_eq!(is_a(src, "foo", "somethingelse"), IsA::Unknown, "beyond the unknown parent");
    }

    #[test]
    fn unknown_when_sub_or_super_unknown() {
        let src = "<?php class A {}";
        // Sub is an unknown external → Unknown (unless reflexively equal).
        assert_eq!(is_a(src, "ghost", "a"), IsA::Unknown);
        assert_eq!(is_a(src, "ghost", "ghost"), IsA::Yes, "reflexive even when unknown");
        // Sub known+enumerated, super an unknown name absent from the closed set → No.
        assert_eq!(is_a(src, "a", "ghost"), IsA::No);
    }

    #[test]
    fn ambiguous_sub_is_unknown() {
        // Two definitions of the same FQN → ambiguous → not Unique → Unknown.
        let src = "<?php class Dup {} class Dup {}";
        assert_eq!(is_a(src, "dup", "whatever"), IsA::Unknown);
    }

    #[test]
    fn trait_use_does_not_force_unknown() {
        // A `use`d trait adds no type; the class is still fully enumerated (its real
        // parent/interfaces), so a `No` verdict stands.
        let src = "<?php trait T {} class A {} class Foo extends A { use T; }";
        assert_eq!(is_a(src, "foo", "a"), IsA::Yes);
        assert_eq!(is_a(src, "foo", "unrelated"), IsA::No, "trait use keeps closure complete");
    }
}

#[cfg(test)]
mod n4_carrier_tests {
    //! ADR-0052 N4 — contract facts, class facts, and instanceof subtraction at the
    //! carrier level (the walk-integration path is covered by the `narrowing_n4`
    //! integration test). Each adversarial drift direction of the slice prompt has a
    //! test: argument-order (`is_a(M,T)`), positive-branch non-final survival,
    //! Unknown-keeps-both, emptied-lane-is-no-fact, Asserted-never-launders, and the
    //! A11 catalog-skew demotion scoped to arm deletion.
    use super::*;

    /// Build a `Cx` over a one-file project and run `f` against it. `php_minor` seeds
    /// the A11 version input.
    fn with_cx<R>(src: &str, php_minor: Option<(u16, u16)>, f: impl FnOnce(&Cx) -> R) -> R {
        let tree = SourceTree::parse(src);
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        let cx = Cx::new_with(&units, &index, 0, &EMPTY_DAM, false, true, php_minor);
        f(&cx)
    }

    fn cls(s: &str) -> ContractTy {
        ContractTy::Class(s.to_owned())
    }
    fn arm(ty: ContractTy, stratum: Stratum) -> ContractArm {
        ContractArm { ty, stratum }
    }
    fn oracle<'c, 'a>(cx: &'c Cx<'a>) -> ProjectIsa<'c, 'a> {
        ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() }
    }

    /// The identity class resolver for the global-namespace seeding tests: the
    /// lowered phpdoc names are already the normalized FQNs there.
    fn id_resolve(n: &str) -> String {
        n.to_ascii_lowercase()
    }

    // ---- native_arms / flatten_arms / seeding -------------------------------

    #[test]
    fn native_arms_lowers_scalars_instances_and_null() {
        let src = "<?php function f(?int $a, User|Guest $b): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let params = cx.scope_params(scope).unwrap();
            // `?int` → [int, null] Verified.
            assert_eq!(
                seed_contract_arms(&params[0], None, &id_resolve),
                Some(vec![arm(ContractTy::Base(Base::Int), Stratum::Verified), arm(ContractTy::Null, Stratum::Verified)])
            );
            // `User|Guest` native (object instances) → [User, Guest] Verified.
            assert_eq!(
                seed_contract_arms(&params[1], None, &id_resolve),
                Some(vec![arm(cls("user"), Stratum::Verified), arm(cls("guest"), Stratum::Verified)])
            );
        });
    }

    #[test]
    fn seed_phpdoc_refines_at_asserted_stratum() {
        // `object $value` (native None) + `@param User|Guest` → phpdoc arms, Asserted.
        let src = "<?php /** @param User|Guest $value */ function f(object $value): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let p = &cx.scope_params(scope).unwrap()[0];
            let env = cx.scope_envelopes(scope).unwrap();
            let seeded = seed_contract_arms(p, env.param("value"), &id_resolve).unwrap();
            assert_eq!(
                seeded,
                vec![arm(cls("user"), Stratum::Asserted), arm(cls("guest"), Stratum::Asserted)]
            );
        });
    }

    #[test]
    fn seed_phpdoc_arm_backed_by_native_stays_verified() {
        // `int $x` + `@param int $x`: the `int` arm the native ALSO proves keeps the
        // Verified stratum (no needless downgrade); a phpdoc-only refinement would be
        // Asserted.
        let src = "<?php /** @param int $x */ function f(int $x): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let p = &cx.scope_params(scope).unwrap()[0];
            let env = cx.scope_envelopes(scope).unwrap();
            assert_eq!(
                seed_contract_arms(p, env.param("x"), &id_resolve),
                Some(vec![arm(ContractTy::Base(Base::Int), Stratum::Verified)])
            );
        });
    }

    #[test]
    fn dedup_contract_arms_ties_keep_min_stratum() {
        // Two arm_eq arms (a Verified `int` and an Asserted `int`, as a join would
        // produce): the survivor keeps the WEAKER (Asserted) stratum — no laundering.
        let mut arms = vec![
            arm(ContractTy::Base(Base::Int), Stratum::Verified),
            arm(ContractTy::Base(Base::Int), Stratum::Asserted),
        ];
        dedup_contract_arms(&mut arms);
        assert_eq!(arms, vec![arm(ContractTy::Base(Base::Int), Stratum::Asserted)]);
    }

    #[test]
    fn dedup_collapses_identical_opaque_arms() {
        // Survey non-termination regression (nextcloud `core/Migrations`): the
        // non-extensional arms (`ArrayAny`/`CallableTy`/`StrOpaque`/`Opaque`)
        // have `subsumes(x, x) == Maybe`, so `arm_eq` alone could NOT collapse two
        // identical copies — a branch-union then doubled the pile at every join,
        // reaching 2^depth. Structural equality must collapse them. A whole pile of
        // one opaque arm dedups to a single arm regardless of count.
        // (`Mixed`/`ObjectAny` are arm_eq-reflexive already, so were never affected.)
        // The two arms observed exploding in the survey (`array $options`,
        // `\Closure $schemaClosure`) plus the other non-extensional floors. Each is an
        // arm `arm_eq` cannot prove equal to ITSELF (`subsumes(x, x) == Maybe`), so
        // before the fix a 64-copy pile stayed 64 and doubled at the next join.
        for ty in [
            ContractTy::ArrayAny { non_empty: false },
            ContractTy::CallableTy(None),
            ContractTy::StrOpaque,
            ContractTy::Opaque,
        ] {
            assert!(!normalize::arm_eq(&ty, &ty), "{ty:?} is expectedly non-arm_eq-reflexive");
            let mut arms: Vec<ContractArm> =
                (0..64).map(|_| arm(ty.clone(), Stratum::Verified)).collect();
            dedup_contract_arms(&mut arms);
            assert_eq!(arms, vec![arm(ty.clone(), Stratum::Verified)], "{ty:?} pile must collapse to one");
        }
    }

    #[test]
    fn dedup_identical_opaque_keeps_min_stratum() {
        // The structural-equality collapse still honors the derivation clause: a
        // Verified + Asserted pair of the SAME opaque arm survives at Asserted.
        let mut arms = vec![
            arm(ContractTy::CallableTy(None), Stratum::Verified),
            arm(ContractTy::CallableTy(None), Stratum::Asserted),
            arm(ContractTy::CallableTy(None), Stratum::Verified),
        ];
        dedup_contract_arms(&mut arms);
        assert_eq!(arms, vec![arm(ContractTy::CallableTy(None), Stratum::Asserted)]);
    }

    // ---- the deliverable: else-of-instanceof leaves {Guest} -----------------

    const FIXTURE: &str = "<?php interface Named { public function name(): string; } \
        final class User implements Named { public function name(): string { return 'u'; } } \
        final class Guest { public function guestId(): int { return 1; } }";

    #[test]
    fn negative_branch_leaves_guest_arm_asserted() {
        // The conformance deliverable, at the carrier level: a `User|Guest` lane, the
        // else of `instanceof User` subtracts User (is_a(User,User)=Yes), leaving
        // {Guest} — and Guest keeps its Asserted stratum (came from `@param`).
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert(
                "value".into(),
                vec![arm(cls("user"), Stratum::Asserted), arm(cls("guest"), Stratum::Asserted)],
            );
            subtract_contract_lane(
                &mut store,
                "value",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("value"), Some([arm(cls("guest"), Stratum::Asserted)].as_slice()));
        });
    }

    #[test]
    fn negative_branch_argument_order_is_m_then_t() {
        // `Named` is a supertype of `User`. else of `instanceof User` over a lane
        // holding `Named` asks is_a(Named, User) = No (a Named need not be a User) →
        // the arm SURVIVES. A reversed is_a(User, Named)=Yes would wrongly delete it.
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("named"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("v"), Some([arm(cls("named"), Stratum::Verified)].as_slice()));
        });
    }

    #[test]
    fn positive_branch_deletes_final_nonmember_keeps_open() {
        // then of `instanceof User` over `Guest|Named`: Guest is final and
        // is_a(Guest,User)=No → deleted; Named is NOT final → survives (an unseen
        // Named subclass could be a User). Guards both positive-branch drifts.
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("guest"), Stratum::Verified), arm(cls("named"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: true },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("v"), Some([arm(cls("named"), Stratum::Verified)].as_slice()));
        });
    }

    #[test]
    fn emptied_lane_drops_to_no_fact() {
        // A `!== null` on a `null`-only lane empties it → the lane is REMOVED (no
        // key), never a death signal (§2: the verdict owns death).
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(ContractTy::Null, Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &normalize::Subtrahend::Null, &oracle(cx));
            assert_eq!(store.contract_arms("v"), None, "emptied lane is no-fact, not present-and-empty");
        });
    }

    // ---- Member fact + eval_instanceof implication (§3b) --------------------

    #[test]
    fn member_implication_yes_no_maybe() {
        with_cx(FIXTURE, None, |cx| {
            // yes:[User], test `instanceof Named`: is_a(User,Named)=Yes → Yes.
            let m = Member { yes: vec!["user".into()], no: vec![] };
            assert_eq!(member_instanceof(cx, Some(&m), "named"), Certainty::Yes);
            // no:[Named], test `instanceof User`: is_a(User,Named)=Yes so a User would
            // be a Named, which the guard excluded → No.
            let m2 = Member { yes: vec![], no: vec!["named".into()] };
            assert_eq!(member_instanceof(cx, Some(&m2), "user"), Certainty::No);
            // yes:[Guest], test `instanceof Named`: is_a(Guest,Named)=No, no exclusion
            // matches → Maybe.
            let m3 = Member { yes: vec!["guest".into()], no: vec![] };
            assert_eq!(member_instanceof(cx, Some(&m3), "named"), Certainty::Maybe);
            // No fact → Maybe.
            assert_eq!(member_instanceof(cx, None, "named"), Certainty::Maybe);
        });
    }

    // ---- A11 catalog version-skew demotion ----------------------------------

    #[test]
    fn a11_catalog_backed_deletion_demoted_only_on_skew() {
        // Empty project: `ArrayObject`/`Traversable` resolve through the builtin
        // CATALOG. else of `instanceof Traversable` over an `ArrayObject` arm asks
        // is_a(ArrayObject, Traversable) = Yes (catalog-backed).
        let sub = normalize::Subtrahend::Class { fqn: "traversable".into(), polarity: false };
        // Pinned minor (matches catalog) → verdict stands → arm deleted.
        with_cx("<?php", Some(steins_catalog::PINNED_PHP), |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("arrayobject"), Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &sub, &oracle(cx));
            assert_eq!(store.contract_arms("v"), None, "matching minor: catalog verdict stands, arm deleted");
        });
        // Skewed minor → catalog-backed verdict demotes to Unknown → arm KEPT.
        with_cx("<?php", Some((steins_catalog::PINNED_PHP.0, steins_catalog::PINNED_PHP.1 - 1)), |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("arrayobject"), Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &sub, &oracle(cx));
            assert_eq!(
                store.contract_arms("v"),
                Some([arm(cls("arrayobject"), Stratum::Verified)].as_slice()),
                "skewed minor: catalog-backed deletion demoted, arm kept (FP-safe)"
            );
        });
    }

    #[test]
    fn a11_in_project_deletion_not_demoted_on_skew() {
        // A purely in-project `User|Guest` union narrows the SAME under a skewed minor
        // — A11 touches only catalog-backed edges, never in-project source.
        with_cx(FIXTURE, Some((steins_catalog::PINNED_PHP.0, steins_catalog::PINNED_PHP.1 - 1)), |cx| {
            assert!(cx.a11_demote_catalog(), "skew is active");
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Verified), arm(cls("guest"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(
                store.contract_arms("v"),
                Some([arm(cls("guest"), Stratum::Verified)].as_slice()),
                "in-project is_a(User,User)=Yes not catalog-backed → deletion stands under skew"
            );
        });
    }

    #[test]
    fn parse_php_minor_reads_major_minor() {
        assert_eq!(parse_php_minor("8.5.8"), Some((8, 5)));
        assert_eq!(parse_php_minor("8.4.10-dev"), Some((8, 4)));
        assert_eq!(parse_php_minor("nonsense"), None);
    }

    // ---- join semantics -----------------------------------------------------

    #[test]
    fn join_unions_contract_arms_and_intersects_members() {
        // A branch with lane {User} and Member{yes:[User]} joined with a branch with
        // lane {Guest} and Member{yes:[Guest]}: the merged lane is {User,Guest} (a
        // value live on EITHER path is possible), and the Member intersection is empty
        // (no bound holds on both) → dropped.
        let mut a = Store::default();
        a.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Asserted)]);
        a.members.insert("v".into(), Member { yes: vec!["user".into()], no: vec![] });
        let mut b = Store::default();
        b.contract.insert("v".into(), vec![arm(cls("guest"), Stratum::Asserted)]);
        b.members.insert("v".into(), Member { yes: vec!["guest".into()], no: vec![] });
        let j = join_stores(&a, &[&b]);
        let mut got = j.contract.get("v").cloned().unwrap();
        got.sort_by(|x, y| format!("{:?}", x.ty).cmp(&format!("{:?}", y.ty)));
        assert_eq!(got, vec![arm(cls("guest"), Stratum::Asserted), arm(cls("user"), Stratum::Asserted)]);
        assert_eq!(j.members.get("v"), None, "disjoint members intersect to empty → dropped");
    }

    #[test]
    fn unbind_forgets_narrowing_carriers() {
        // Reassignment (`store.unbind`) voids both new carriers for the var.
        let mut store = Store::default();
        store.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Verified)]);
        store.members.insert("v".into(), Member { yes: vec!["user".into()], no: vec![] });
        store.unbind("v");
        assert_eq!(store.contract_arms("v"), None);
        assert_eq!(store.member_of("v"), None);
    }
}

#[cfg(test)]
mod dump_render_tests {
    //! ADR-0053 §7 — the dump fact renderer and its annotate-parity pin: a finite
    //! fact's dump rendering byte-equals the ONE shared speller's output for that
    //! fact (`spell_arms(summarize_vals(members))`), and the abstract layers render
    //! the honest keyword ladder. Rendering, not the walk, is under test here; the
    //! end-to-end emitter is covered by the `dump_surface` integration test.
    use super::*;

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.to_owned())
    }

    /// The parity pin (ADR-0053 §7): the dump's rendering of a finite fact is exactly
    /// the shared speller's output for that fact — one spelling, no second renderer.
    fn assert_parity(vals: &[Val]) {
        let fact = Fact::from_vals(vals.to_vec()).expect("nonempty");
        let via_speller = normalize::summarize_vals(fact.finite_members().expect("finite"))
            .and_then(|arms| steins_contract::spell::spell_arms(&arms))
            .unwrap_or_else(|| DUMP_UNKNOWN.to_owned());
        assert_eq!(render_dump_fact(&fact), via_speller, "dump vs D2 speller for {vals:?}");
    }

    #[test]
    fn finite_facts_byte_equal_the_shared_speller() {
        // Singleton int / string, OneOf int / string-enum, dedup, nullable, bool.
        assert_parity(&[i(5)]);
        assert_parity(&[s("abc")]);
        assert_parity(&[i(1), i(2), i(3)]);
        assert_parity(&[s("GET"), s("POST")]);
        assert_parity(&[i(1), i(2), i(1)]);
        assert_parity(&[i(1), Val::Null]);
        assert_parity(&[Val::Bool(true), Val::Bool(false)]);
        assert_parity(&[Val::Bool(true)]);
        // An all-numeric string set collapses to the numeric-string class.
        assert_parity(&[s("12"), s("34"), i(1)]);
    }

    #[test]
    fn singleton_renders_the_honesty_spelling_not_a_php_literal() {
        // The honesty renderer collapses an int literal to its base (`int`) and keeps
        // a string literal precise (`'abc'`) — deliberately NOT PHPStan's `5` literal
        // spelling (§9 quarantines that to assertType). This is the shared speller.
        assert_eq!(render_dump_fact(&Fact::Singleton(i(5))), "int");
        assert_eq!(render_dump_fact(&Fact::Singleton(s("abc"))), "'abc'");
        assert_eq!(render_dump_fact(&Fact::Singleton(Val::Null)), "null");
    }

    #[test]
    fn abstract_layers_render_the_honest_keyword_ladder() {
        // General: bare base, with nullability.
        assert_eq!(render_dump_fact(&Fact::General { base: Base::Int, nullable: false }), "int");
        assert_eq!(
            render_dump_fact(&Fact::General { base: Base::String, nullable: true }),
            "string|null"
        );
        // Refined int range: the named predicate class.
        assert_eq!(
            render_dump_fact(&Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false)),
            "positive-int"
        );
        // Refined string: reuse the speller's own preds_keyword so a refined-string
        // dump and its spell_arms rendering agree.
        let numeric = Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
        assert_eq!(
            render_dump_fact(&numeric),
            steins_contract::spell::preds_keyword(StrPreds::NUMERIC.close())
        );
    }

    #[test]
    fn array_bearing_fact_is_honest_unknown() {
        // A set the domain cannot faithfully spell (an array member) dumps `unknown`,
        // never a guess (§7).
        let fact = Fact::Singleton(Val::Array(vec![]));
        assert_eq!(render_dump_fact(&fact), DUMP_UNKNOWN);
    }
}
