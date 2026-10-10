//! The fold seam (ADR-0004 / ADR-0024 / ADR-0066): [`Folder`] is what the checker
//! asks to fold a builtin call; [`FoldEngine`] is the transport (three questions, no
//! judgment); [`EngineFolder`] is the policy over any engine — the allowlist, the
//! width and shape admission, the resource brakes, the refusal notes and the
//! [`SurfaceSummary`] the CLI prints. [`FoldPosture`] reports what the surface
//! delivered over a run. The two transports live in [`crate::fold_process`] and
//! [`crate::fold_table`]; argument lowering in [`crate::fold_args`].

use std::collections::{HashMap, HashSet};

use steins_catalog::{RefusalAxis, ResourceParam, ResourceReturn};
use steins_domain::Fact;
use steins_sidecar::{
    BuiltinParam, ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldKey, FoldResult,
    PregCompile, Reflection,
};
use steins_syntax::ArgValue;

use crate::builtin_returns::admit_return_fact;
use crate::fold_args::{arg_to_fold, fold_value_to_arg, parse_php_minor};
use crate::fold_budget::fold_within_allocation_budget;

/// What the fold surface actually delivered over one whole run (issue #245).
///
/// The two notices above are *events*, printed once at the moment the posture
/// changes — not enough for a harness that reports a number at the end, since a
/// count measured after the child died looks identical to one measured before.
/// So the posture is also carried as data, reported beside the number it
/// qualifies (ADR-0004: incompleteness is never silent).
///
/// Counters are edges, not requests: `losses` counts requests that ended with
/// the child dead or silent (never retried, per ADR-0024); `restarts` counts the
/// replacements that followed. `losses == 0` is the only shape that licenses
/// reading a count as sidecar-backed throughout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldPosture {
    /// Whether a live engine was ever reached at all. `false` covers both
    /// `--no-php` and a `php` that could not be started — the plain sound
    /// subset, which the [`SOUND_SUBSET_NOTICE`] already names at the top of
    /// the run. A `php` that started and failed its boot handshake (issue #891)
    /// reports `engaged: true`, `losses: 1`, `abandoned: true`: it was reached,
    /// and it lost the run its answers.
    ///
    /// [`SOUND_SUBSET_NOTICE`]: crate::SOUND_SUBSET_NOTICE
    pub engaged: bool,
    /// Requests that ended with the transport dead or silent. Each is one lost
    /// answer, never retried.
    pub losses: u32,
    /// Children replaced after such a loss. `restarts == losses` means every
    /// loss was repaired and the run finished on a live child.
    pub restarts: u32,
    /// The respawn budget is spent and the transport is dead: every remaining
    /// request widens, and the run ended as the sound subset.
    pub abandoned: bool,
}

impl FoldPosture {
    /// Whether every request this run put to the engine was answered by a live
    /// engine — the one condition under which an absolute count is comparable
    /// with a sidecar-backed baseline.
    ///
    /// A run that never engaged an engine is **not** "throughout": it is the
    /// sound subset, which is a different posture with a different notice, not a
    /// clean bill of health.
    #[must_use]
    pub fn sidecar_backed_throughout(&self) -> bool {
        self.engaged && self.losses == 0
    }
}

// ---------------------------------------------------------------------------
// Folding seam (ADR-0004 / ADR-0024).
// ---------------------------------------------------------------------------

/// Something that can fold a builtin call to a concrete literal value, and (from
/// ADR-0049 S2) answer the runtime boot surface for the absence-proof family.
pub trait Folder {
    /// Fold `name(args...)` to a literal, or `None` to widen.
    fn fold(&mut self, name: &str, args: &[ArgValue], strict: bool) -> Option<ArgValue>;

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

    // global constants (ADR-0078, issue #198)
    /// Ask the project's own PHP whether the global constant `name` is defined —
    /// the `constant.undefined` ladder's boot-surface leg. `Some(true)` is a
    /// homonym (an extension constant, or one a loaded bootstrap defined) and
    /// forces silence; `Some(false)` is definitively absent; `None` is
    /// unanswerable (no sidecar / a mid-run failure ⇒ silence). The default is
    /// `None` — the sound subset (ADR-0004).
    ///
    /// Unlike [`Self::boot_surface_function`] and
    /// [`Self::boot_surface_class_like`], `name` is **case-preserved**: PHP
    /// constant names are case-sensitive (the case-insensitive third argument to
    /// `define()` was removed in 8.0, and the workspace floor is 8.1 per
    /// ADR-0011), so a lowercased query would be a different question.
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        let _ = name;
        None
    }
    // end global constants (ADR-0078, issue #198)

    /// Ask the project's own PHP whether the extension `name` is loaded — what
    /// `extension_loaded('name')` answers on the boot surface (issue #928), from the
    /// sidecar `env()`'s loaded-extension list. Case-insensitive, as PHP's own lookup
    /// is. `Some(true)` — loaded; `Some(false)` — not loaded; `None` — unanswerable
    /// (no sidecar / `--no-php` / a mid-run failure ⇒ the guard folds to `Maybe`).
    /// The default is `None`: the sound subset (ADR-0004).
    fn boot_surface_extension(&mut self, name: &str) -> Option<bool> {
        let _ = name;
        None
    }

    // reflected class world (ADR-0024 `reflect`, issue #269)

    /// The **declaration** the project's own PHP holds for `fqn`, when neither a
    /// source declaration nor a builtin-catalog row answers it — the class an
    /// installed extension provides (`Redis`, `Random\Randomizer`, `Dom\Element`).
    ///
    /// `Some(r)` with `r.declaration == Some(..)` is the runtime's own declaration:
    /// members, signatures, constants, hierarchy edges, origin (`internal` +
    /// `extension`). `Some(r)` with no declaration is a definitive not-found.
    /// `None` is unanswerable (no sidecar, `--no-php`, a timeout, a poisoned
    /// child, a runner too old to implement the method) and reads as silence. The
    /// default is `None`: the sound subset (ADR-0004).
    ///
    /// # This answer resolves; it does not convict (owner ruling, 2026-08-09)
    ///
    /// A reflected declaration is **envelope-grade**: the runtime's claim about its
    /// own class, not a proof about the program. It may restore coverage and buy
    /// correct silence, and a member check may consume it as an envelope — but
    /// **no absence-family finding may be premised on its completeness**.
    /// `call.undefined-method`, `property.undefined`, `class-const.undefined`,
    /// `class.undefined` and the arity family all require a chain that is
    /// source-declared and uniquely resolved (`Cx::find_class`); this query is
    /// deliberately unreachable from any of them, since a reflected class never
    /// enters the project index. Convictions over reflected declarations are a
    /// separate slice behind their own ADR (runtime answers refute, not convict —
    /// `constant.undefined`'s `defined()` precedent).
    ///
    /// `fqn` is the resolved name; PHP class names are case-insensitive, so the
    /// memo keys on the lowercased form.
    fn reflected_class(&mut self, fqn: &str) -> Option<ClassReflection> {
        let _ = fqn;
        None
    }

    // end reflected class world (issue #269)

    /// The project's own PHP `(major, minor)` from the sidecar `env()` — the
    /// ADR-0052 A11 version-skew input. `None` (the default / sound subset) when no
    /// sidecar answers: an unknown minor is treated as "no detectable skew", so the
    /// catalog pin stands and arm deletion behaves exactly as it did before A11.
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        None
    }

    /// The boot surface's self-description for the existence-id message register
    /// (ADR-0049 §9): `PHP 8.5.8 (32 extensions)`, sourced from the sidecar `env()`.
    /// `None` (the default / sound subset) when no sidecar answers — the emitter
    /// then falls back to a version-agnostic phrasing. The existence ids only fire
    /// when [`Self::absence_family_available`] is `true`, so a live label is the
    /// normal case at a firing site.
    fn boot_surface_label(&mut self) -> Option<String> {
        None
    }

    /// The value-domain **return fact** of a uniquely-resolved builtin `name`
    /// (ADR-0056 R1): the reflected return envelope, refined by an admitted
    /// curated row. `None` when nothing may be seeded — no sidecar, a
    /// runtime-redefinition extension loaded (ADR-0049 A9, value-domain edition:
    /// a monkey-patched builtin disowns its declared type), a name the runtime
    /// does not know as a function, a return type not representable as a single
    /// value-domain [`Fact`] (a multi-base union such as `int|false` — deferred
    /// to the contract-lane arms), or no return type at all. Default `None` (sound
    /// subset, ADR-0004). Always seeded at the `Verified` stratum — native, off
    /// the engine's own arginfo (§2), never demoted to Asserted. `name` is the
    /// call's simple name (the fold path's key).
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        let _ = name;
        None
    }

    /// The **reflected return-type declaration** of a uniquely-resolved builtin
    /// `name` — the `(string)` rendering of `ReflectionFunction::getReturnType()`
    /// (`"array"`, `"string|int|null"`). [`Self::builtin_return_fact`]'s raw
    /// sibling, for transfers whose result is not a scalar the value domain names
    /// in a single `Fact` (ADR-0062 §4's positional projections return arrays;
    /// `array_key_first` returns `int|string|null`). Same gates as above, so a
    /// rule admitted through it rests on the same ADR-0061 §2 evidence,
    /// unweakened. Default `None`: the sound subset withholds the rule.
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        let _ = name;
        None
    }

    /// The resource-producer row of the builtin `name` (ADR-0056 §8): whether
    /// that return carries a `false` failure arm, and the kind of handle it
    /// returns (ADR-0097 §2.1).
    ///
    /// The one return fact that cannot ride the reflected envelope — PHP has no
    /// syntax to declare it (`fopen` reports no return type and never will). §7's
    /// gate replaces the envelope's authority with three conditions this method
    /// checks together: the catalog row (php-src stub at the pin), the engine
    /// declaring NO return type for the name (the resource-to-object migration
    /// tripwire — an engine answering `CurlHandle|false` has disowned the row),
    /// and the project minor equalling the catalog pin. Default `None`.
    fn builtin_resource_return(&mut self, name: &str) -> Option<ResourceReturn> {
        let _ = name;
        None
    }

    /// The **reflected parameter counts** of a uniquely-resolved builtin `name` —
    /// `(getNumberOfParameters(), getNumberOfRequiredParameters())`.
    ///
    /// The **arity second leg** of ADR-0064's mixed-pin ruling: the declaration
    /// pin ([`Self::builtin_return_type`]) countersigns a structural transfer, but
    /// a name declaring bare `mixed` (the array read-position family: `current`,
    /// `array_pop`, `array_first`, …) pins nothing — `mixed` is compatible with
    /// any rule output, degenerating the check to a presence test. Such a rule is
    /// inadmissible on the declaration alone and must additionally pin the live
    /// signature. Same gates as the declaration; default `None` (no arity, no
    /// rule — also what an older runner or a pre-arity replay table yields).
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        let _ = name;
        None
    }

    /// The **reflected parameter list** of a uniquely-resolved builtin `name` —
    /// `ReflectionFunction::getParameters()` per position, in declaration order
    /// (ADR-0056 §9, R1's parameter twin).
    ///
    /// The counts above pin a *rule*; this judges an *argument*. It is the source
    /// the builtin arm of the argument check reads, and it is Verified for the same
    /// reason the return envelope is: the running engine's own arginfo, read off the
    /// engine that will run the code, so it is version-correct by construction and
    /// carries none of the rot a signature map would (§6).
    ///
    /// Same gates as [`Self::builtin_return_type`], and the same silences: `None`
    /// without a live engine (`--no-php`), with an ADR-0049 A9 monkey-patcher
    /// loaded, for a name the runtime does not know as a function, and for a replay
    /// table recorded before the field. `Some(vec![])` is a zero-parameter function
    /// — an empty list is an answer, `None` never is. Default `None` (sound subset,
    /// ADR-0004); the ADR-0069 static floor answers nothing here, ever, and §9.5
    /// says why.
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        let _ = name;
        None
    }

    /// What the builtin `name` demands of a **resource** argument at its 0-based
    /// position `index` (ADR-0097 §2.5) — the parameter twin of
    /// [`Self::builtin_resource_return`], gated the same way.
    ///
    /// `resource` is the one parameter type PHP cannot spell, so the reflected
    /// list above reports no type at every such position and the stub docblock
    /// is the only record of the demand. A row is answered only when all of the
    /// gate holds: the catalog has the row (the stub reading at the pin), the
    /// engine has the function and **reports no declared type at the position**
    /// — the tripwire: the day `fwrite` declares `Stream $stream`, the engine
    /// speaks and the row is disowned — the position is neither by-reference
    /// nor variadic, and the project minor equals the catalog pin. Default
    /// `None` (sound subset, ADR-0004): `--no-php` admits nothing.
    fn builtin_resource_param(&mut self, name: &str, index: usize) -> Option<ResourceParam> {
        let _ = (name, index);
        None
    }

    // preg pattern refusal (ADR-0078, issue #189)

    /// The project's own PCRE **refusal** of `pattern`, as PCRE's own words — or
    /// `None` when no refusal is proven.
    ///
    /// `pattern` is the whole pattern PHP would receive, delimiters and modifiers
    /// included, because those are exactly the parts PCRE can reject. The answer is
    /// the running engine's, obtained by compiling the pattern there (ADR-0004): it
    /// is never derived from Steins' own pattern reader, whose dislikes are not
    /// PCRE's. The returned string carries no `<function>(): ` prefix — the emitter
    /// re-attaches the name of the function at the call site, which is the one PHP
    /// would name.
    ///
    /// **`None` deliberately conflates "compiles" with "cannot answer"**, because a
    /// consumer must treat them identically: only a refusal licenses a finding, and
    /// everything else is silence. The sound subset (no sidecar / `--no-php`) is
    /// therefore the default, like every other engine question here.
    fn preg_pattern_refusal(&mut self, pattern: &str) -> Option<String> {
        let _ = pattern;
        None
    }
}

/// The runtime-redefinition extensions that void the absence family (ADR-0049 A9):
/// with any of them loaded, a defined class can gain a method and a missing name
/// can be minted at runtime, so no absence claim holds. Matched case-insensitively
/// against the sidecar's loaded-extension list. Public so `doctor` (ADR-0054 §9.1)
/// can name a loaded monkey-patch extension from the same single source of truth.
pub const MONKEY_PATCH_EXTENSIONS: &[&str] = &["uopz", "runkit7", "runkit", "componere"];

/// The sound-subset folder: never folds anything. This is what the salsa
/// [`diagnostics`] query uses, keeping that query deterministic.
///
/// [`diagnostics`]: crate::diagnostics
pub struct NoFold;

impl Folder for NoFold {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
}

// ---------------------------------------------------------------------------
// The fold seam, split transport-from-policy (ADR-0066).
//
// `FoldEngine` is the transport: three questions, no judgment. `EngineFolder` is
// the policy: every memo, every gate, the whole ADR-0056 admission sequence — and
// it exists EXACTLY ONCE, generic over the transport. That is the point of the
// split. The browser's replay transport (issue #64) and the native process
// transport answer the same questions, so they cannot disagree about what the
// answers MEAN; issue #63 was a bug in precisely this seam, found only because a
// second caller reached the policy by a second path.
// ---------------------------------------------------------------------------

/// The **transport** half of the fold seam: the three questions the analysis puts
/// to the project's own PHP (ADR-0004/0024), with no policy attached.
///
/// An implementation answers or declines; it never decides what an answer licenses.
/// [`ProcessEngine`] talks to a resident `php` child, and [`TableEngine`] answers
/// from a supplied memo table and records its misses (ADR-0066).
///
/// Declining is spelled the way the wire spells it: `None` for `env`/`reflect`,
/// [`FoldResult::Widen`] for `fold`. No implementation may fabricate an answer —
/// a wrong answer here becomes a wrong diagnostic, which is the one thing the
/// zero-FP contract forbids.
///
/// [`ProcessEngine`]: crate::fold_process::ProcessEngine
/// [`TableEngine`]: crate::fold_table::TableEngine
pub trait FoldEngine {
    /// The engine's environment (version, loaded extensions, SAPI, integer width).
    fn env(&mut self) -> Option<EnvInfo>;
    /// Whether the engine knows `target` as a resident function / class-like, plus
    /// its declared return type when it is a function.
    fn reflect(&mut self, target: &str) -> Option<Reflection>;
    /// The engine's **declaration** for a resident class-like — members, signatures,
    /// constants, hierarchy edges, origin (issue #269). `None` declines; a parsed
    /// reply with no declaration is a definitive not-found.
    fn reflect_class(&mut self, target: &str) -> Option<ClassReflection>;
    /// Run `name(args)` on the engine and report the outcome.
    fn fold(&mut self, name: &str, args: &[FoldArg], strict: bool) -> FoldResult;
    /// Whether the engine's own PCRE accepts `pattern` (ADR-0078, issue #189).
    /// `None` declines — no engine, a failed request, or a `false` return the
    /// runner could not attribute to a compile refusal.
    fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile>;
    // global constants (ADR-0078, issue #198)
    /// Whether the engine has the global constant `name` — its `defined($name)`
    /// (ADR-0078, issue #198). `name` is the resolved FQN with case preserved:
    /// constants are case-sensitive, so unlike `reflect` this question is not
    /// case-blind. `None` declines.
    fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined>;
    // end global constants (ADR-0078, issue #198)

    /// How many times the transport underneath has been **replaced** this run
    /// (issue #245) — a generation counter, not a health check.
    ///
    /// A decline is only ever a statement about the engine that declined. Where a
    /// caller memoizes a decline as a *whole-run* answer, a bumped generation says
    /// the engine that gave it no longer exists, so the answer may be asked once
    /// more of the one that replaced it — [`EngineFolder`]'s `refresh_env_memos`
    /// is the only consumer. Bounded by how often the child is replaced, which is
    /// what keeps "ask again" from becoming "ask at every call site": the
    /// respawn cap bounds replacements in a row, and the process engine stops
    /// asking a callee that killed a child (issue #783).
    ///
    /// The default `0` is right for every transport that cannot be replaced
    /// mid-run ([`TableEngine`], and any wasm engine): its declines are as
    /// permanent as it is.
    ///
    /// [`TableEngine`]: crate::fold_table::TableEngine
    fn restarts(&self) -> u32 {
        0
    }
}

/// The **policy** half of the fold seam: a [`Folder`] over any [`FoldEngine`],
/// carrying every per-run memo and every admission gate.
///
/// Two folders ship on top of it — [`SidecarFolder`] (the resident `php` child)
/// and [`TableFolder`] (the replay table) — and they differ ONLY in their engine.
/// Every question of *what an answer licenses* is decided here: the A9
/// monkey-patch veto, the issue-#28 target/runtime agreement, the ADR-0056 curated
/// admission gates, the issue-#64 integer-width gate. A gate added here is added
/// for both transports at once, which is the property the split exists to buy.
///
/// [`SidecarFolder`]: crate::fold_process::SidecarFolder
/// [`TableFolder`]: crate::fold_table::TableFolder
pub struct EngineFolder<E: FoldEngine> {
    pub(crate) engine: E,
    /// Keyed by `(name, args, strict)`: the calling convention is part of the
    /// question, not context around it — the same name and arguments answer
    /// differently under `declare(strict_types=1)`.
    memo: HashMap<(String, Vec<ArgValue>, bool), Option<ArgValue>>,
    /// Cached ADR-0049 A9 verdict: whether the absence family is available (a live
    /// engine and no monkey-patch extension). Computed once from `env` and then
    /// memoized — a whole-run property (ADR-0048 query answer).
    absence_available: Option<bool>,
    /// Per-FQN memo of the A2ii homonym oracle so a repeated chain class never
    /// triggers duplicate `reflect` traffic.
    boot_surface_memo: HashMap<String, Option<bool>>,
    /// Per-FQN memo of the arity family's function-homonym oracle (ADR-0049 §6),
    /// the function-namespace analogue of [`Self::boot_surface_memo`].
    boot_surface_fn_memo: HashMap<String, Option<bool>>,
    /// Memoized project PHP `(major, minor)` from `env` (ADR-0052 A11) — a
    /// whole-run query answer. `Some(None)` records "asked, unanswerable".
    php_minor: Option<Option<(u16, u16)>>,
    /// Memoized `PHP_INT_SIZE` from `env` (issue #64) — the engine's integer width
    /// in bytes. `Some(None)` records "asked, unanswerable". Engine-intrinsic, so
    /// unlike the target-dependent memos it survives [`Self::set_php_target`].
    int_size: Option<Option<u32>>,
    /// Memoized boot-surface description (`PHP 8.5.8 (32 extensions)`) from `env`
    /// — the ADR-0049 §9 message register's closure-evidence clause for the
    /// existence ids. `Some(None)` records "asked, unanswerable".
    boot_surface_label: Option<Option<String>>,
    /// Memoized loaded-extension set from `env` (issue #928), lowercased — what the
    /// `extension_loaded()` guard fold asks. An `env`-derived whole-run answer, dropped
    /// with the others by [`Self::refresh_env_memos`]. `Some(None)` records "asked,
    /// unanswerable".
    extensions: Option<Option<HashSet<String>>>,
    /// Per-name memo of the builtin return-fact (ADR-0056 R1) so a repeated call to
    /// the same builtin never triggers duplicate `reflect` traffic. Keyed by the
    /// lowercased simple name (PHP function names are case-insensitive).
    return_fact_memo: HashMap<String, Option<Fact>>,
    /// Per-name memo of the raw reflected return-type declaration (ADR-0062 S7's
    /// projection gate), the string sibling of [`Self::return_fact_memo`].
    return_type_memo: HashMap<String, Option<String>>,
    /// Per-name memo of the ADR-0056 §8 resource-return answer. Rides the same
    /// `reflect` reply as the two memos above — §7's tripwire is a question about
    /// that reply, not a second round trip.
    resource_return_memo: HashMap<String, Option<ResourceReturn>>,
    /// Per-name memo of the reflected `(total, required)` parameter counts —
    /// ADR-0064's mixed-pin second leg, riding the same `reflect` reply as the two
    /// memos above and following the same per-name pattern.
    param_counts_memo: HashMap<String, Option<(u32, u32)>>,
    /// Per-name memo of the reflected parameter list (ADR-0056 §9) — the fourth
    /// answer read off the same `reflect` reply, memoized on the same terms, so a
    /// builtin called in fifty files costs the round trip once.
    param_types_memo: HashMap<String, Option<Vec<BuiltinParam>>>,
    /// Per-`(name, position)` memo of the ADR-0097 §2.5 resource-parameter answer.
    /// Rides [`Self::param_types_memo`]'s reply — the tripwire is a question about
    /// that reply's position, not a second round trip.
    resource_param_memo: HashMap<(String, usize), Option<ResourceParam>>,
    /// Per-**pattern** memo of the PCRE compile verdict (ADR-0078, issue #189):
    /// the dedupe that makes a pattern repeated across a run cost exactly one
    /// `preg_compile` request. Keyed by the pattern verbatim — PCRE is
    /// case-sensitive about everything in a pattern, delimiters and modifier
    /// letters included, so unlike the name-keyed memos above nothing is
    /// lowercased.
    ///
    /// Engine-intrinsic, like `int_size`: what this engine's PCRE build accepts
    /// does not change with the project's declared target, so
    /// [`Self::set_php_target`] does not drop it. The target gate lives at the
    /// check, on `absence_family_available`.
    preg_refusal_memo: HashMap<String, Option<String>>,
    // global constants (ADR-0078, issue #198)
    /// Per-**name** memo of the constant-existence oracle (issue #198). Keyed by
    /// the name verbatim, with nothing lowercased: PHP constant names are
    /// case-sensitive, so `FOO` and `Foo` are genuinely different questions and
    /// folding them together would answer one with the other.
    ///
    /// Target-dependent, like the two boot-surface memos it sits beside: which
    /// constants exist is a property of the interrogated engine, and
    /// [`Self::set_php_target`] changes whether that engine may be believed.
    boot_surface_const_memo: HashMap<String, Option<bool>>,
    // end global constants (ADR-0078, issue #198)
    // reflected class world (issue #269)
    /// Per-**class** memo of the reflected declaration, keyed by the lowercased FQN
    /// (PHP class names are case-insensitive). A declaration is the largest reply
    /// on this wire (a fat extension class carries hundreds of methods), so
    /// asking twice for the same name is what this memo must prevent.
    ///
    /// Also keyed by the `env()` identity ([`Self::env_identity`]): an engine that
    /// changes underneath the run invalidates every prior answer rather than
    /// silently mixing two runtimes' class worlds. Per-run memoization only;
    /// cross-run persistence of engine answers lives in `fold_persist`, the
    /// generation-scoped fold table (ADR-0092 §4, issue #500).
    ///
    /// Engine-intrinsic, like `int_size` and `preg_refusal_memo`: [`Self::set_php_target`]
    /// does not drop it, since the target gates absence claims and a declaration
    /// is not one.
    class_reflect_memo: HashMap<String, Option<ClassReflection>>,
    /// The `env()` identity the [`Self::class_reflect_memo`] entries were taken at:
    /// PHP version plus the loaded-extension set, the two facts that determine which
    /// classes exist and what they contain. `Some(None)` records "asked,
    /// unanswerable". See [`Self::class_world_identity`].
    env_identity: Option<Option<String>>,
    /// The transport generation [`Self::env_identity`] was taken at — this memo's
    /// own stamp, separate from `env_generation` (see `class_world_identity`).
    class_env_generation: u32,
    // end reflected class world (issue #269)
    /// The project's declared target PHP range (issue #28), when the layout
    /// resolved one. Set by the CLI after layout discovery; gates the absence
    /// family (the boot surface interrogated must be a declared-supported
    /// version) and the curated return-fact admission.
    php_target: Option<steins_db::PhpTarget>,
    /// The transport generation ([`FoldEngine::restarts`]) the five `env`-derived
    /// memos above were taken at (issue #245). See [`Self::refresh_env_memos`].
    env_generation: u32,
}

impl<E: FoldEngine> EngineFolder<E> {
    /// Wrap `engine` in the shared policy. Every memo starts empty: a folder is a
    /// per-run object, and its answers are whole-run query answers (ADR-0048).
    pub fn with_engine(engine: E) -> Self {
        Self {
            engine,
            memo: HashMap::new(),
            absence_available: None,
            boot_surface_memo: HashMap::new(),
            boot_surface_fn_memo: HashMap::new(),
            php_minor: None,
            extensions: None,
            int_size: None,
            boot_surface_label: None,
            return_fact_memo: HashMap::new(),
            return_type_memo: HashMap::new(),
            resource_return_memo: HashMap::new(),
            param_counts_memo: HashMap::new(),
            param_types_memo: HashMap::new(),
            resource_param_memo: HashMap::new(),
            preg_refusal_memo: HashMap::new(),
            boot_surface_const_memo: HashMap::new(),
            class_reflect_memo: HashMap::new(),
            env_identity: None,
            class_env_generation: 0,
            php_target: None,
            env_generation: 0,
        }
    }

    /// The engine, for a caller that owns transport-specific state on it (the
    /// replay transport's pending list).
    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    /// Declare the project's target PHP range (issue #28), from the resolved
    /// layout. Target-dependent memos are dropped on a change, so a resident
    /// folder reused across projects (the corpus gate's thread-local) answers
    /// each project under its own target.
    ///
    /// The env-derived memos (`php_minor`, `int_size`) are NOT dropped: they
    /// describe the engine, not the project, and no target changes what version
    /// the engine is or how wide its integers are.
    pub fn set_php_target(&mut self, target: Option<steins_db::PhpTarget>) {
        if self.php_target != target {
            self.absence_available = None;
            self.return_fact_memo.clear();
            self.return_type_memo.clear();
            self.param_counts_memo.clear();
            self.param_types_memo.clear();
            self.resource_param_memo.clear();
            self.boot_surface_memo.clear();
            self.boot_surface_fn_memo.clear();
            self.boot_surface_const_memo.clear();
        }
        self.php_target = target;
    }

    /// Drop the five `env`-derived whole-run answers when the child that gave
    /// them has since been replaced (issue #245).
    ///
    /// The five memos below (`absence_available`, `php_minor`, `int_size`,
    /// `boot_surface_label`, `extensions`) are *whole-run* answers taken from a single `env()`
    /// reply, and each gates a whole family: a declined `env` would otherwise turn
    /// the entire absence family off for the rest of the run from one badly-timed
    /// request. The lost *request* stays lost (ADR-0024: never retried); what must
    /// not stay lost is the standing answer taken in that window.
    ///
    /// The generation counter bounds the re-asking: re-asking on "the memo holds a
    /// decline" would pay the ADR-0024 timeout at every call site against a merely
    /// hung sidecar; re-asking on "the engine that declined was replaced" costs at
    /// most one `env` per respawn, bounded as [`FoldEngine::restarts`] says. All
    /// five are dropped together rather than only the declines, since they come
    /// from one reply and a conditional would have to distinguish a decline from
    /// a real verdict (a loaded monkey-patch extension is a legitimate
    /// `absence_available == Some(false)`).
    ///
    /// The per-name memos are deliberately NOT dropped: a declined `reflect` costs
    /// exactly the one name asked about (the same magnitude ADR-0024 already
    /// accepts), while dropping them would re-issue thousands of requests per
    /// respawn.
    fn refresh_env_memos(&mut self) {
        let generation = self.engine.restarts();
        if generation == self.env_generation {
            return;
        }
        self.env_generation = generation;
        self.absence_available = None;
        self.php_minor = None;
        self.int_size = None;
        self.boot_surface_label = None;
        self.extensions = None;
    }

    // reflected class world (issue #269)

    /// The `env()` identity the reflected class world is keyed by — PHP version
    /// plus the loaded-extension set, the two facts that determine which classes
    /// are resident and what each contains. Re-taken when the transport has been
    /// replaced, **clearing [`Self::class_reflect_memo`] when it has changed** —
    /// so a changed runtime invalidates the answers rather than silently mixing
    /// two engines' class worlds inside one run.
    ///
    /// An unanswerable `env()` is a *distinct* identity from any answered one, so
    /// an answered memo is not carried across a window where the engine stopped
    /// describing itself. Own generation stamp rather than riding
    /// [`Self::refresh_env_memos`]: that helper drops its memos on a bumped
    /// generation, while here the point is to *compare* the re-taken value with
    /// the one the class memo holds.
    fn class_world_identity(&mut self) -> Option<String> {
        let generation = self.engine.restarts();
        if let Some(cached) = &self.env_identity
            && generation == self.class_env_generation
        {
            return cached.clone();
        }
        let taken = self.engine.env().map(|env| {
            let mut extensions: Vec<String> =
                env.extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
            // Sorted: `get_loaded_extensions()` reports load order, and a
            // reordering is not a different runtime.
            extensions.sort_unstable();
            format!("{}|{}", env.php_version, extensions.join(","))
        });
        if let Some(previous) = &self.env_identity
            && *previous != taken
        {
            self.class_reflect_memo.clear();
        }
        self.class_env_generation = generation;
        self.env_identity = Some(taken.clone());
        taken
    }

    // end reflected class world (issue #269)

    /// The engine's integer width in bytes (`PHP_INT_SIZE`), memoized like the
    /// other whole-run `env` answers. `None` = unanswerable, which every caller
    /// here treats as "not provably 64-bit".
    fn engine_int_size(&mut self) -> Option<u32> {
        self.refresh_env_memos();
        if let Some(cached) = self.int_size {
            return cached;
        }
        let answer = self.engine.env().and_then(|e| e.int_size);
        self.int_size = Some(answer);
        answer
    }

    /// Whether the engine's integer machine is the one every value rule here
    /// assumes: 64-bit (issue #64).
    ///
    /// A minor is not a machine. php-wasm 0.1.0 is PHP 8.5.2 (the pinned minor,
    /// admitted by every existing version gate) built 32-bit, where
    /// `ip2long('255.255.255.255')` is `-1`, `crc32('x')` is negative, `1 << 40`
    /// is `0`, `hexdec('FFFFFFFFF')` promotes to float, and
    /// `strtotime('2040-01-01')` is `false` — silently wrong *values*, not
    /// failures: nothing widens, nothing throws, and a fold would carry the wrong
    /// literal straight into a proof. So ADR-0056 curated admission requires a
    /// **provably** 64-bit engine; an unknown width declines.
    ///
    /// The CURATED-ROW leg, all-or-nothing on purpose: a curated row is a claim
    /// about a builtin's whole return domain, verified against the 64-bit engine
    /// at `PINNED_PHP`, with no per-call argument tuple to range-check it against
    /// — so there is nothing for the fold lane's portable subset (below) to be
    /// the analogue of.
    fn engine_is_64_bit(&mut self) -> bool {
        self.engine_int_size() == Some(8)
    }

    /// Gate 2 of the ADR-0056 §2 admission sequence, on its own: whether a
    /// **curated** return-fact row may refine the reflected envelope.
    ///
    /// A curated row is verified against the 64-bit engine at
    /// [`steins_catalog::PINNED_PHP`] and nowhere else, so it is admitted only
    /// when the analysis is about exactly that version *and* that machine. With a
    /// declared target (issue #28) the WHOLE range must be the pin; with no target
    /// the runtime-vs-pin comparison stands as before #28.
    ///
    /// Extracted so [`Self::surface_summary`] reports the same verdict the gate
    /// applies, not a second copy of it.
    fn curated_rows_admitted(&mut self) -> bool {
        self.engine_is_64_bit()
            && match &self.php_target {
                Some(t) => t.is_exactly(steins_catalog::PINNED_PHP),
                None => self.php_minor() == Some(steins_catalog::PINNED_PHP),
            }
    }

    /// Describe the engine surface **as this folder's own gates see it** — the
    /// data a frontend needs to state its precision boundary (issue #64 S3).
    ///
    /// Every field is read off the same helpers that decide admission
    /// ([`Self::boot_surface_label`], `engine_int_size`, `fold_lane_at_width`,
    /// `curated_rows_admitted`, [`Folder::absence_family_available`]), so a
    /// description and the behaviour it describes cannot drift: changing a gate
    /// changes what this reports, in the same commit.
    ///
    /// Asking is not free on the replay transport: an unanswered `env` is
    /// recorded as pending like any other miss, so a caller that summarizes
    /// *before* collecting pending gets a run that asks for the boot surface it
    /// could not describe, converging one iteration later. A converged run always
    /// has a complete summary.
    pub fn surface_summary(&mut self) -> SurfaceSummary {
        let label = self.boot_surface_label();
        let php_version = self.engine.env().map(|e| e.php_version);
        let int_size = self.engine_int_size();
        SurfaceSummary {
            label,
            php_version,
            int_size,
            fold_lane: fold_lane_at_width(int_size),
            curated_rows: self.absence_family_available() && self.curated_rows_admitted(),
            absence_family: self.absence_family_available(),
            fold_total: steins_catalog::foldable_entry_count(),
            fold_portable: steins_catalog::portable_names().len(),
            refused_folds: steins_catalog::refused_names(),
            refusals: steins_catalog::refused_names()
                .iter()
                .filter_map(|&name| {
                    steins_catalog::refusal(name)
                        .map(|r| RefusalNote { name, axis: r.axis, witness: r.witness })
                })
                .collect(),
            unverified_folds: steins_catalog::unverified_names(),
        }
    }

    /// Compute the builtin return fact for `key` (already lowercased) — the
    /// admission gate of ADR-0056 §2 assembled from three whole-run engine
    /// answers. Called once per name; [`Folder::builtin_return_fact`] memoizes.
    fn compute_builtin_return_fact(&mut self, key: &str) -> Option<Fact> {
        // Gate 1 — a live engine and no runtime-redefinition extension (ADR-0049
        // A9 applied to the value domain): a monkey-patched builtin
        // (uopz/runkit7/Componere) can return a type its native declaration
        // disowns. Also covers the no-engine sound subset.
        if !self.absence_family_available() {
            return None;
        }
        // Gate 2 (minor pin, ADR-0052 A11 / ADR-0056 §2, target-aware per issue
        // #28) governs the CURATED refinement only — the reflected envelope is
        // version-correct by construction and needs no pin. A curated row is
        // verified at PINNED_PHP and nowhere else: with a declared target the row
        // is admitted only when the WHOLE range is the pin; with no target, the
        // runtime-vs-pin comparison stands as before #28.
        //
        // The pin is a version AND a machine (issue #64): a 32-bit engine at the
        // same minor can violate a curated row (`hexdec` returning float where
        // the row says int). The reflected envelope is unaffected (a declared
        // return type is platform-independent), so an unpinned or
        // narrow-integer engine still SEEDS, just without curated refinement.
        let minor_matches_pin = self.curated_rows_admitted();
        // The reflected return envelope — the running engine's own declaration.
        let refl = self.engine.reflect(key)?;
        if !refl.function_exists {
            return None;
        }
        let return_type = refl.return_type.as_deref()?;
        let curated = steins_catalog::return_fact(key);
        admit_return_fact(return_type, curated, minor_matches_pin)
    }

    /// The raw reflected return-type declaration for `key` (already lowercased),
    /// under the same A9 / sound-subset gate [`Self::compute_builtin_return_fact`]
    /// applies. Called once per name; [`Folder::builtin_return_type`] memoizes.
    ///
    /// No integer-width gate: this is the engine's own declaration read back
    /// verbatim, and a declaration does not change with the integer machine.
    fn compute_builtin_return_type(&mut self, key: &str) -> Option<String> {
        if !self.absence_family_available() {
            return None;
        }
        let refl = self.engine.reflect(key)?;
        refl.function_exists.then_some(refl.return_type).flatten()
    }

    /// Compute the resource-return answer for `key` (already lowercased) — the
    /// ADR-0056 §8 gate, whose three conditions are checked here in the order that
    /// makes the reasoning readable. Called once per name; memoized by
    /// [`Folder::builtin_resource_return`].
    fn compute_builtin_resource_return(&mut self, key: &str) -> Option<ResourceReturn> {
        // Gate 1 — same live-engine / no-monkey-patching posture as every other
        // return rung (ADR-0049 A9): without an engine there is no tripwire to
        // check, and a row admitted without its tripwire is what §7 prevents.
        if !self.absence_family_available() {
            return None;
        }
        // Gate 2 — the minor pin: the stub was read at `PINNED_PHP` and says
        // nothing about any other minor (ADR-0056 §2).
        if !self.curated_rows_admitted() {
            return None;
        }
        let refl = self.engine.reflect(key)?;
        // A name this engine does not have is not this engine's resource
        // producer, whatever the pinned stubs said (an unloaded extension, or a
        // function removed since the pin).
        if !refl.function_exists {
            return None;
        }
        // Gate 3 — THE TRIPWIRE. Silence is the evidence: PHP cannot spell a
        // `resource` return, so a genuine resource producer declares nothing. An
        // engine that DOES declare something has migrated the function to an
        // object (`curl_init` → `CurlHandle|false`), and §1's precedence rule
        // says curation yields to the engine without exception. Refuses the 89
        // rotted `functionMap` names (ADR-0069 §5) with no denylist, and will
        // switch `fopen` off by itself the day some future PHP migrates it.
        if refl.return_type.is_some() {
            return None;
        }
        steins_catalog::resource_return(key)
    }

    /// The reflected `(total, required)` parameter counts for `key` (already
    /// lowercased), under the same gate the two computations above apply. Called
    /// once per name; [`Folder::builtin_param_counts`] memoizes.
    ///
    /// Both counts must be present: a reply carrying one and not the other is not a
    /// signature, and a half-known arity pins nothing.
    fn compute_builtin_param_counts(&mut self, key: &str) -> Option<(u32, u32)> {
        if !self.absence_family_available() {
            return None;
        }
        let refl = self.engine.reflect(key)?;
        if !refl.function_exists {
            return None;
        }
        Some((refl.params_total?, refl.params_required?))
    }

    /// The reflected parameter list for `key` (already lowercased), under the same
    /// gate the three computations above apply (ADR-0056 §9.1). Called once per
    /// name; [`Folder::builtin_param_types`] memoizes.
    ///
    /// No minor pin and no curated composition: unlike the return rung there is
    /// nothing to refine *within* — the engine's own signature is the whole answer,
    /// and §9.5 refuses the alternative outright.
    fn compute_builtin_param_types(&mut self, key: &str) -> Option<Vec<BuiltinParam>> {
        if !self.absence_family_available() {
            return None;
        }
        let refl = self.engine.reflect(key)?;
        if !refl.function_exists {
            return None;
        }
        refl.params
    }

    /// Compute the resource-parameter answer for `key` (already lowercased) at
    /// `index` — ADR-0097 §2.5's gate, which is [`Self::compute_builtin_resource_return`]'s
    /// gate in the parameter direction. Called once per `(name, position)`;
    /// memoized by [`Folder::builtin_resource_param`].
    fn compute_builtin_resource_param(&mut self, key: &str, index: usize) -> Option<ResourceParam> {
        // Gate 1 — the catalog row, asked first because it is free and it is the
        // common decline: most positions of most builtins are not resources.
        let row = steins_catalog::resource_param(key, index)?;
        // Gate 2 — the minor pin: the stub was read at `PINNED_PHP` and says
        // nothing about any other minor (ADR-0056 §2). Includes the live-engine /
        // no-monkey-patching posture through `builtin_param_types` below.
        if !self.curated_rows_admitted() {
            return None;
        }
        // Gate 3 — THE TRIPWIRE, read off the reflected parameter list (the same
        // reply and the same memo the ordinary builtin judgment consumes). A name
        // this engine does not have, or a list shorter than the position, is not
        // this engine's resource consumer whatever the pinned stub said. A
        // position that DECLARES a type has been migrated — the engine speaks,
        // curation yields (ADR-0056 §1) — and so is disowned; a by-reference or
        // variadic position is not a value position at all (ADR-0056 §9.4).
        let params = self.builtin_param_types(key)?;
        let bp = params.get(index)?;
        if bp.ty.is_some() || bp.by_ref || bp.variadic {
            return None;
        }
        Some(row)
    }
}

impl<E: FoldEngine> Folder for EngineFolder<E> {
    fn fold(&mut self, name: &str, args: &[ArgValue], strict: bool) -> Option<ArgValue> {
        // `strict` is part of the KEY, not just of the request. A strict call
        // site and a weak one ask different questions of the same name and
        // arguments — `substr("abcdef", "1")` is `'bcdef'` in one and a
        // `TypeError` in the other — so sharing a memo slot would let whichever
        // file was analyzed first answer for both.
        let key = (name.to_owned(), args.to_vec(), strict);
        if let Some(cached) = self.memo.get(&key) {
            return cached.clone();
        }
        let folded = self.fold_uncached(name, args, strict);
        self.memo.insert(key, folded.clone());
        folded
    }

    fn absence_family_available(&mut self) -> bool {
        self.refresh_env_memos();
        if let Some(cached) = self.absence_available {
            return cached;
        }
        // No live engine ⇒ the family is silent (the ADR-0004 sound subset covers
        // it — A2ii). Otherwise consult the loaded-extension list once (A9), and
        // (issue #28) require the runtime to BE a declared-supported version:
        // every absence claim is evidence about THIS boot surface, and proof of
        // absence on a version the project does not ship on proves nothing about
        // the versions it does. A runtime inside the declared range stays a
        // legitimate witness — absence on it is a break on a supported version.
        //
        // The integer width is NOT consulted: existence is not arithmetic. A
        // 32-bit engine knows exactly the same names as a 64-bit one at the same
        // version, so an absence claim from it is as good.
        let target_admits_runtime = |minor: Option<(u16, u16)>, t: &Option<steins_db::PhpTarget>| {
            match (t, minor) {
                (Some(t), Some(m)) => t.contains(m),
                (Some(_), None) => false, // a declared target, an unparseable runtime: no witness
                (None, _) => true,        // no declaration: the pre-#28 posture
            }
        };
        let verdict = match self.engine.env() {
            Some(env) => {
                let clean = !env.extensions.iter().any(|e| {
                    MONKEY_PATCH_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m))
                });
                let minor = parse_php_minor(&env.php_version);
                clean && target_admits_runtime(minor, &self.php_target)
            }
            None => false,
        };
        self.absence_available = Some(verdict);
        verdict
    }

    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        if let Some(cached) = self.boot_surface_memo.get(fqn) {
            return *cached;
        }
        let answer = self.engine.reflect(fqn).map(|r| r.class_like_exists);
        self.boot_surface_memo.insert(fqn.to_owned(), answer);
        answer
    }

    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        if let Some(cached) = self.boot_surface_fn_memo.get(fqn) {
            return *cached;
        }
        let answer = self.engine.reflect(fqn).map(|r| r.function_exists);
        self.boot_surface_fn_memo.insert(fqn.to_owned(), answer);
        answer
    }

    // global constants (ADR-0078, issue #198)
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        if let Some(cached) = self.boot_surface_const_memo.get(name) {
            return *cached;
        }
        let answer = self
            .engine
            .constant_defined(name)
            .map(|d| matches!(d, steins_sidecar::ConstantDefined::Defined));
        self.boot_surface_const_memo.insert(name.to_owned(), answer);
        answer
    }
    // end global constants (ADR-0078, issue #198)

    fn boot_surface_extension(&mut self, name: &str) -> Option<bool> {
        self.refresh_env_memos();
        if self.extensions.is_none() {
            let answer = self.engine.env().map(|e| {
                e.extensions.iter().map(|x| x.to_ascii_lowercase()).collect::<HashSet<String>>()
            });
            self.extensions = Some(answer);
        }
        let loaded = self.extensions.as_ref()?.as_ref()?;
        Some(loaded.contains(&name.to_ascii_lowercase()))
    }

    // reflected class world (issue #269)
    fn reflected_class(&mut self, fqn: &str) -> Option<ClassReflection> {
        // Taken FIRST, on every query: it is what may clear the memo below, and a
        // lookup that read a stale entry before the invalidation ran would be
        // exactly the cross-runtime mixing the key exists to prevent.
        //
        // The identity is not otherwise consulted. A run with no answerable `env()`
        // still asks — a live engine that cannot describe itself can still describe
        // a class, and declining here would trade a real answer for a posture.
        let _identity = self.class_world_identity();
        let key = fqn.to_ascii_lowercase();
        if let Some(cached) = self.class_reflect_memo.get(&key) {
            return cached.clone();
        }
        let answer = self.engine.reflect_class(&key);
        self.class_reflect_memo.insert(key, answer.clone());
        answer
    }
    // end reflected class world (issue #269)

    fn php_minor(&mut self) -> Option<(u16, u16)> {
        self.refresh_env_memos();
        if let Some(cached) = self.php_minor {
            return cached;
        }
        // Parse the engine-reported `php_version` (`"8.5.8"`) to `(major, minor)`;
        // an unparseable / absent report stays `None` (no detectable skew — A11).
        let answer = self.engine.env().and_then(|e| parse_php_minor(&e.php_version));
        self.php_minor = Some(answer);
        answer
    }

    fn boot_surface_label(&mut self) -> Option<String> {
        self.refresh_env_memos();
        if let Some(cached) = &self.boot_surface_label {
            return cached.clone();
        }
        let answer = self
            .engine
            .env()
            .map(|e| format!("PHP {} ({} extensions)", e.php_version, e.extensions.len()));
        self.boot_surface_label = Some(answer.clone());
        answer
    }

    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        let key = name.to_ascii_lowercase();
        if let Some(cached) = self.return_fact_memo.get(&key) {
            return cached.clone();
        }
        let answer = self.compute_builtin_return_fact(&key);
        self.return_fact_memo.insert(key, answer.clone());
        answer
    }

    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        let key = name.to_ascii_lowercase();
        if let Some(cached) = self.return_type_memo.get(&key) {
            return cached.clone();
        }
        let answer = self.compute_builtin_return_type(&key);
        self.return_type_memo.insert(key, answer.clone());
        answer
    }

    fn builtin_resource_return(&mut self, name: &str) -> Option<ResourceReturn> {
        let key = name.to_ascii_lowercase();
        if let Some(cached) = self.resource_return_memo.get(&key) {
            return *cached;
        }
        let answer = self.compute_builtin_resource_return(&key);
        self.resource_return_memo.insert(key, answer);
        answer
    }

    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        let key = name.to_ascii_lowercase();
        if let Some(cached) = self.param_counts_memo.get(&key) {
            return *cached;
        }
        let answer = self.compute_builtin_param_counts(&key);
        self.param_counts_memo.insert(key, answer);
        answer
    }

    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        let key = name.to_ascii_lowercase();
        if let Some(cached) = self.param_types_memo.get(&key) {
            return cached.clone();
        }
        let answer = self.compute_builtin_param_types(&key);
        self.param_types_memo.insert(key, answer.clone());
        answer
    }

    fn builtin_resource_param(&mut self, name: &str, index: usize) -> Option<ResourceParam> {
        let key = (name.to_ascii_lowercase(), index);
        if let Some(cached) = self.resource_param_memo.get(&key) {
            return *cached;
        }
        let answer = self.compute_builtin_resource_param(&key.0, index);
        self.resource_param_memo.insert(key, answer);
        answer
    }

    // preg pattern refusal (ADR-0078, issue #189)

    fn preg_pattern_refusal(&mut self, pattern: &str) -> Option<String> {
        if let Some(cached) = self.preg_refusal_memo.get(pattern) {
            return cached.clone();
        }
        // `Compiles` and a declined request collapse to the same `None` here — see
        // the trait doc. The engine is asked at most once per distinct pattern per
        // run, which is the whole point of the memo: a pattern written in fifty
        // files costs one round trip.
        let answer = match self.engine.preg_compile(pattern) {
            Some(PregCompile::Refuses { message }) => Some(message),
            Some(PregCompile::Compiles) | None => None,
        };
        self.preg_refusal_memo.insert(pattern.to_owned(), answer.clone());
        answer
    }
}

impl<E: FoldEngine> EngineFolder<E> {
    /// The uncached body of [`Folder::fold`].
    fn fold_uncached(&mut self, name: &str, args: &[ArgValue], strict: bool) -> Option<ArgValue> {
        // The integer-width gate (issue #64): a fold is a VALUE, and a 32-bit
        // engine answers arithmetic questions differently while failing at
        // nothing. Asked before the engine is dispatched to, so a narrow engine
        // is never handed a question its machine would answer wrongly.
        //
        // The width is asked FIRST, before the arguments are even encoded, so the
        // first iteration of a replay loop reports the `env` question and nothing
        // else — the round trip that establishes the machine.
        let width = self.engine_int_size();
        let fargs: Vec<FoldArg> = args.iter().filter_map(arg_to_fold).collect();
        if fargs.len() != args.len() {
            return None;
        }
        if !fold_admitted_at_width(width, name, &fargs) {
            return None;
        }
        // The shape gate (issue #382), asked before the runner: the allowlist
        // gates the CALLEE, and a callable argument is a second callee the seam
        // would hand to a real PHP process verbatim. The refusal carries the
        // carrying position; a fold decline is a widen either way.
        if fold_shape_refusal(name, &fargs).is_some() {
            return None;
        }
        // The ambient-setting gate (ADR-0101 §3.3), before the engine is asked.
        if fold_reads_ambient_setting(name, &fargs) || fold_coerces_float_to_string(name, &fargs) {
            return None;
        }
        if !fold_within_allocation_budget(name, &fargs) {
            return None;
        }
        match self.engine.fold(name, &fargs, strict) {
            FoldResult::Value(v) => fold_value_to_arg(&v),
            FoldResult::Throw { .. } | FoldResult::Widen { .. } => None,
        }
    }
}

/// The fold lane's integer-width admission (issue #64 S1.5) — a pure function of
/// the engine's reported `PHP_INT_SIZE`, the callee, and the encoded arguments.
///
/// Three cases, and only three:
///
/// * `Some(8)` — the machine every value rule here assumes. Admit everything, so
///   this is byte-identical to the pre-S1.5 behaviour on every native run.
/// * `Some(4)` — the **portable subset**. Admit `name` only when the catalog
///   certifies it ([`steins_catalog::portable`]) *and* every integer in the
///   arguments is inside [`I32_SAFE`] — both legs required, since the catalog
///   verdict is stated only for tuples the range guard admits. The catalog's
///   classification went three-valued in ADR-0028's 2026-08-14 amendment §4 and
///   this gate did not move: a `Refused` row and an `Unverified` one are both
///   "not certified" here, staying distinguishable only where evidence is
///   reported.
/// * anything else (`None`, or an unverified width) — **default-deny**: an old
///   or foreign runner is unknown, not assumed, and no 16-bit/128-bit subset is
///   verified because nobody has probed one.
///
/// Declining is spelled as always: the caller returns `None`, which widens.
fn fold_admitted_at_width(int_size: Option<u32>, name: &str, args: &[FoldArg]) -> bool {
    match fold_lane_at_width(int_size) {
        FoldLane::Full => true,
        FoldLane::PortableSubset => {
            steins_catalog::portable(name) && args.iter().all(fold_arg_fits_i32)
        }
        FoldLane::Declined => false,
    }
}

/// Why the shape gate refused a fold (issue #382) — the decline reason, carried
/// as data so it names a position rather than a sentence.
///
/// A fold decline is spelled `None` everywhere else, which is the right spelling
/// for a widen and the wrong one for an audit: "the seam declined" and "the seam
/// declined because parameter 1 of `array_filter` is a callee the engine would
/// run" are the same value. This type is the second half, and the shape gate
/// this module consults before dispatch is what produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldShapeRefusal {
    /// The callee has **no mined `param_facts` row**, so no shape gate can see
    /// where its callees would arrive. Declining is the only honest answer: the
    /// catalog asserts every foldable name is mined, so this costs nothing
    /// today and means an admission that skips the mining step declines rather
    /// than walking past a gate that cannot see it.
    Unmined,
    /// An argument reached a position that carries a userland callee.
    Carrier(steins_catalog::CallbackCarrier),
}

impl FoldShapeRefusal {
    /// The refusal in words, naming the carrying position.
    #[must_use]
    pub fn reason(self, name: &str) -> String {
        match self {
            Self::Unmined => {
                format!("{name} has no mined parameter row, so no callee position is visible")
            }
            Self::Carrier(c) => c.describe(name),
        }
    }
}

/// The **shape gate** (issue #382): whether this call's argument list leaves
/// every callee-carrying position empty, and which position refused it.
///
/// Consulted before the runner is asked, and **independent of the allowlist** —
/// [`steins_catalog::foldable`] gates the callee, and this gates the argument
/// list, so a name admitted to the allowlist tomorrow is gated today.
///
/// # Why the allowlist is not enough
///
/// The allowlist gates the **callee**. A builtin that takes a callable smuggles a
/// SECOND callee past that gate as an ordinary string argument, and the seam hands
/// string arguments to the runner verbatim — which calls them. Measured, on a
/// branch that briefly admitted `array_filter`:
///
/// * `array_filter(["a", "b"], "var_dump")` — the callback's output landed on
///   stdout ahead of the JSON-RPC reply, desynced the NDJSON stream and poisoned
///   the sidecar, degrading the whole run to the sound subset.
/// * `array_filter(["PATH"], "getenv")` — folded to `list{'PATH'}`, which is
///   `getenv` running inside the analysis with its answer reaching the value
///   domain. `system` and `unlink` are the same call.
///
/// Nothing about `array_filter` is impure; the *argument* is the problem. So the
/// rule is about the argument list, not the name.
///
/// # Where the positions come from
///
/// [`steins_catalog::callback_carriers`] — the **one** carrier rule, which the
/// by-value lane (ADR-0070 §2.3) reads for its own hazard. Its five routes are
/// argued there; what this function owns is what "empty" means at each of them,
/// since that is the only part a *call site* can answer:
///
/// | route | empty is |
/// | --- | --- |
/// | declared `callable`, `invocation_shape`, deferred `mixed` | absent, or a literal `null` |
/// | untyped variadic tail | absent — the call does not reach the tail |
/// | array of callables | absent, a literal `null`, or an EMPTY array |
///
/// A literal `null` is PHP's own "no callback" spelling — `array_filter($a,
/// null)` drops the falsy elements — and a literal string is exactly what a
/// callable argument looks like on this wire, so it declines. `ob_start()` with
/// no arguments carries nothing and `ob_start("var_dump")` carries a callee,
/// and the gate tells them apart without knowing anything about either name.
///
/// The empty array is the same rule one level down: measured with the name
/// force-admitted, `preg_replace_callback_array(["/a/" => "strtoupper"], "aaa")`
/// does not fold while `preg_replace_callback_array([], "aaa")` answers `'aaa'`.
///
/// **A name with no mined row does not fold at all** ([`FoldShapeRefusal::Unmined`]),
/// whatever the carrier rule says about it — the curated routes can name a
/// position without the arginfo, and a gate that cannot see the whole parameter
/// list should not be certifying one.
fn fold_shape_refusal(name: &str, args: &[FoldArg]) -> Option<FoldShapeRefusal> {
    use steins_catalog::CarrierShape;

    if steins_catalog::param_facts(name).is_none() {
        return Some(FoldShapeRefusal::Unmined);
    }
    steins_catalog::callback_carriers(name)
        .positions()
        .find(|c| {
            // The position is OCCUPIED — the call reaches it with something
            // that is not the name's own spelling of "no callback".
            match c.shape {
                // The tail rule is about the POSITION rather than the type: any
                // argument that REACHES an unargued `mixed ...$rest` is the
                // `array_udiff` comparator as far as this seam can tell.
                CarrierShape::UndeclaredTail => args.len() > c.position,
                // Every entry of the array is a callee, so only an empty one is
                // vacant. Anything that is not an array there is not the call
                // this name describes, and declining is the answer for that too.
                CarrierShape::InsideArray => match args.get(c.position) {
                    None | Some(FoldArg::Null) => false,
                    Some(FoldArg::Array(entries)) => !entries.is_empty(),
                    Some(_) => true,
                },
                CarrierShape::Declared | CarrierShape::Invoked | CarrierShape::DeferredMixed => {
                    !matches!(args.get(c.position), None | Some(FoldArg::Null))
                }
            }
        })
        .map(FoldShapeRefusal::Carrier)
}

/// Whether folding `name(args)` would bake an **ambient setting** into the
/// value (ADR-0101 §3.3): a printf-family call whose literal format keeps the
/// locale read, that is some `%f`, `%g` or `%G`, or a format the parser cannot
/// read as the engine does.
///
/// The allowlist is permission to ask the engine, not a promise that every
/// call is pure, and the runner always answers under `LC_NUMERIC=C`. A fold is
/// a claim about the project's runtime, which declared nothing about its
/// locale, so `sprintf('%.2f', 1.5)` is left to the row (`global.read.setting
/// .locale`) and not folded to `'1.50'`. A literal format that shows no reading
/// conversion (`'%d-%s'`, `'%.2F'`) folds as before.
///
/// A format that is not a string, or no argument at all, declines too: the
/// verdict reads a string literal and nothing else.
///
/// The same refusal holds a `preg_*` call whose literal pattern consults the locale's character
/// tables, or may (ADR-0101 §3.10): `\w`, `\s`, `\b`, a POSIX class, a caseless flag over a
/// letter, the `x` flag over a byte of `0x80..=0xFF`. The runner has never called `setlocale`,
/// so it answers under the C tables, which is a claim about the project's runtime the project
/// never made. A pattern the reader declines refuses too, as an unreadable format does: the
/// reader knows PCRE2's grammar as far as the corpus and the witnesses go, so a decline is not
/// proof that PCRE2 refuses the pattern, and an invalid one that no longer folds only widens to
/// the declared type. A pattern that is no string folds as before (the engine throws), and
/// `preg_quote` compiles nothing and keeps folding.
///
/// A float renderer (`strval`, `implode`, `json_encode` and the rest of
/// [`steins_catalog::precision_gate`], ADR-0101 §3.15) whose literal arguments hold a float at the
/// depth it renders them does not fold either: the runner renders under the default `precision`
/// and `serialize_precision`, which the project never promised, so `strval(1.5)` is left to the
/// row (`global.read.setting.precision`). A value that holds none folds as before. A non-finite
/// float counts as one (the entry cuts `-INF` to `-IN`), except for `json_encode`, `var_dump` and
/// `debug_zval_dump`, which write it whole or refuse it. `json_encode` with
/// `JSON_NUMERIC_CHECK` does not fold at all, since the flag turns a numeric string into a float.
fn fold_reads_ambient_setting(name: &str, args: &[FoldArg]) -> bool {
    if let Some(gate) = steins_catalog::precision_gate(name) {
        return fold_renders_float(gate, args);
    }
    if steins_catalog::preg::compiles_pattern_argument(name) {
        return match args.first() {
            Some(FoldArg::Str(pattern)) => {
                steins_catalog::preg::pattern_reads_locale(pattern) != Some(false)
            }
            _ => false,
        };
    }
    let Some(family) = steins_catalog::printf_family(name) else { return false };
    match args.get(family.format_position()) {
        Some(FoldArg::Str(format)) => steins_catalog::format_reads_locale(format),
        _ => true,
    }
}

/// Whether a float literal reaches a parameter the builtin declares as `string` (ADR-0101 §3.21,
/// #1024): weak mode coerces it to text with the ambient `precision`, which the runner leaves at
/// the default, so `strlen(1.5)` is left to the row (`global.read.setting.precision`) and not
/// folded to `3`. A union counts when it names `string` and neither `float` nor `mixed`, since
/// those keep the float as it is (`string|int` takes `1.5` as text). A non-finite float counts too.
/// A variadic tail takes the last declared type. A float inside an array argument is not looked
/// at: which builtins stringify the elements is not in the catalog, so `array_unique([1.5])`
/// keeps folding (the effect label stays unset, D4).
fn fold_coerces_float_to_string(name: &str, args: &[FoldArg]) -> bool {
    fn takes_text(declared: &str) -> bool {
        let (mut text, mut float) = (false, false);
        for member in declared.trim_start_matches('?').split('|') {
            match member.trim() {
                "string" => text = true,
                "float" | "mixed" => float = true,
                _ => {}
            }
        }
        text && !float
    }
    if !args.iter().any(|arg| matches!(arg, FoldArg::Float(_))) {
        return false;
    }
    let Some(facts) = steins_catalog::param_facts(name) else { return false };
    args.iter().enumerate().any(|(position, arg)| {
        let declared = facts.params.get(position).or_else(|| {
            facts.variadic.first().filter(|first| **first <= position).and(facts.params.last())
        });
        matches!(arg, FoldArg::Float(_)) && declared.is_some_and(|d| takes_text(d))
    })
}

/// Whether a call to a float renderer folds a float into its text ([`fold_reads_ambient_setting`]).
fn fold_renders_float(gate: steins_catalog::PrecisionGate, args: &[FoldArg]) -> bool {
    /// `JSON_NUMERIC_CHECK`.
    const NUMERIC_CHECK: i64 = 32;
    /// Whether `arg`, rendered at `depth`, writes a float the entry shapes.
    fn holds_float(arg: &FoldArg, depth: steins_catalog::RenderDepth) -> bool {
        use steins_catalog::RenderDepth::{Elements, Nested, NestedFinite, Value};
        match (arg, depth) {
            (FoldArg::Float(f), _) => f.is_finite() || depth.reads_non_finite(),
            (FoldArg::Array(entries), Elements) => {
                entries.iter().any(|(_, entry)| holds_float(entry, Value))
            }
            (FoldArg::Array(entries), Nested | NestedFinite) => {
                entries.iter().any(|(_, entry)| holds_float(entry, depth))
            }
            _ => false,
        }
    }
    if matches!(args.get(1), Some(FoldArg::Int(flags)) if flags & NUMERIC_CHECK != 0)
        && steins_catalog::precision_gate("json_encode") == Some(gate)
    {
        return true;
    }
    gate.values(args.len())
        .into_iter()
        .any(|(position, depth)| args.get(position).is_some_and(|arg| holds_float(arg, depth)))
}

/// Which fold lane an engine of this integer width gets — the width half of
/// [`fold_admitted_at_width`], named so a description of the boundary
/// ([`EngineFolder::surface_summary`]) reads the same three cases the gate
/// branches on instead of restating them. `pub(crate)` for the same reason:
/// the persisted fold table's engine identity (ADR-0092 §4) records the lane,
/// and it must record the gate's own verdict, not a restatement.
pub(crate) fn fold_lane_at_width(int_size: Option<u32>) -> FoldLane {
    match int_size {
        Some(8) => FoldLane::Full,
        Some(4) => FoldLane::PortableSubset,
        _ => FoldLane::Declined,
    }
}

/// The fold lane an engine's integer width admits (issue #64 / ADR-0066 §4 and its
/// S1.5 amendment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldLane {
    /// A provably 64-bit engine: the whole [`steins_catalog::foldable`] allowlist,
    /// which is the machine every value rule here assumes.
    Full,
    /// A provably 32-bit engine: [`steins_catalog::portable_names`] only, and
    /// only for argument tuples the range guard admits.
    PortableSubset,
    /// An unreported or unprobed width: nothing folds. Default-deny — an old or
    /// foreign runner is unknown, not assumed.
    Declined,
}

impl FoldLane {
    /// A stable machine-readable spelling, for an envelope that carries the lane
    /// as data (the playground's boot object).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::PortableSubset => "portable_subset",
            Self::Declined => "declined",
        }
    }
}

/// One refused row's reason, as [`SurfaceSummary`] carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalNote {
    /// The builtin's name, as the catalog spells it.
    pub name: &'static str,
    /// What kind of engine difference refused it.
    pub axis: RefusalAxis,
    /// The recorded divergence, in one line: the call and both answers.
    pub witness: &'static str,
}

/// The engine surface as the shared fold policy sees it — what
/// [`EngineFolder::surface_summary`] answers.
///
/// This is **data about the gates**, not prose about them: each field is the
/// verdict of a gate that is applied elsewhere in this file, read from the same
/// helper. A renderer decides what to say; nothing here decides for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceSummary {
    /// The ADR-0049 §9 boot-surface label (`PHP 8.5.2 (32 extensions)`), or `None`
    /// when no engine answered.
    pub label: Option<String>,
    /// The engine's own `PHP_VERSION`, verbatim.
    pub php_version: Option<String>,
    /// `PHP_INT_SIZE` in bytes. `None` = unreported, which every gate here reads
    /// as "not provably anything".
    pub int_size: Option<u32>,
    /// Which builtins may fold at that width.
    pub fold_lane: FoldLane,
    /// Why each refused row is refused, joined here rather than in a renderer:
    /// the catalog owns both the axis and the witness, and a surface that only
    /// carried names forced its readers to write the reason themselves. The
    /// playground's boundary panel did exactly that, and its sentence — every
    /// refused name renders an integer in the machine's own word — went false
    /// the moment `preg_split` was refused for a PCRE build option.
    ///
    /// The axis travels as the catalog's own [`RefusalAxis`], re-exported here
    /// so a consumer that must not name the catalog (the wasm module keeps it a
    /// dev-dependency on purpose) still gets the type rather than a string it
    /// would have to re-parse. Its wire spelling is the enum's own `as_str`.
    pub refusals: Vec<RefusalNote>,
    /// Whether a curated return-fact row may refine a reflected envelope — both
    /// ADR-0056 gates, conjoined as the admission sequence conjoins them.
    pub curated_rows: bool,
    /// Whether the ADR-0049 A9 absence family is available (a live engine, no
    /// monkey-patch extension, and a runtime the declared target admits).
    pub absence_family: bool,
    /// The size of the folding allowlist, and how much of it
    /// [`FoldLane::PortableSubset`] keeps — the catalog's own counts, so a
    /// renderer states the boundary without a number of its own.
    pub fold_total: usize,
    /// See [`Self::fold_total`].
    pub fold_portable: usize,
    /// The [`steins_catalog::PortabilityClass::Refused`] rows, by name — the folds a
    /// [`FoldLane::PortableSubset`] engine does not get **and can say why**, read
    /// from the catalog rather than restated here.
    ///
    /// Since ADR-0028's 2026-08-14 amendment this is no longer the whole
    /// `foldable ∧ !portable` complement: the unverified rows decline on the
    /// very same gate with nothing on record. They are deliberately not merged in.
    /// A renderer states these as "a divergence was measured, here it is", which
    /// is a sentence an unverified row cannot be given — and §4 of that amendment
    /// exists to stop the two being conflated, since the refused list's
    /// one-divergence-per-row discipline is the only thing that makes it
    /// auditable. Naming the unverified rows to a reader is
    /// [`Self::unverified_folds`]' job.
    pub refused_folds: &'static [&'static str],
    /// The [`steins_catalog::PortabilityClass::Unverified`] rows, by name — the folds a
    /// [`FoldLane::PortableSubset`] engine also does not get, but for the other
    /// reason: nothing was measured, so there is no divergence to cite. Kept
    /// apart from [`Self::refused_folds`] because ADR-0028's 2026-08-14
    /// amendment §4 forbids conflating the classes — a renderer owes these a
    /// different sentence ("not measured; folds only on a provably 64-bit
    /// engine"), not a seat in the divergence list.
    pub unverified_folds: &'static [&'static str],
}

/// The 32-bit argument range guard: `[-(2^31 - 1), 2^31 - 1]`.
///
/// Deliberately **not** `-2^31`. `PHP_INT_MIN` on a 32-bit engine is the one
/// integer whose magnitude is not representable, so it is the seed of every
/// boundary flip — `abs(-2147483648)` promotes to float there and stays `int` on
/// a 64-bit engine. Excluding it means no admitted integer has an out-of-range
/// magnitude, which is what makes the `abs`-shaped flip structurally unreachable
/// rather than merely unobserved. The cost is one value per call site.
const I32_SAFE: std::ops::RangeInclusive<i64> = -2_147_483_647..=2_147_483_647;

/// Whether every integer `arg` carries is inside [`I32_SAFE`] — recursively
/// through array literals, and over **keys as well as values**.
///
/// The keys matter as much as the values. `count([3000000000 => 'a', 'b'])` has no
/// out-of-range integer *value*, and yet the key decides what PHP's next-int rule
/// assigns to `'b'` and therefore whether the array has one element or two. Only
/// `FoldKey::Int` is charged: a `FoldKey::Str` is a string key by the time it
/// reaches the wire (lowering already applied PHP's key normalization), and a
/// `FoldArg::Float` is an IEEE double on both machines.
fn fold_arg_fits_i32(arg: &FoldArg) -> bool {
    match arg {
        FoldArg::Int(v) => I32_SAFE.contains(v),
        FoldArg::Array(entries) => entries.iter().all(|(k, v)| {
            let key_ok = !matches!(k, Some(FoldKey::Int(i)) if !I32_SAFE.contains(i));
            key_ok && fold_arg_fits_i32(v)
        }),
        FoldArg::Float(_) | FoldArg::Str(_) | FoldArg::Bool(_) | FoldArg::Null => true,
    }
}

#[cfg(test)]
mod shape_gate_tests {
    use super::{FoldArg, FoldShapeRefusal, fold_shape_refusal};
    use steins_catalog::CarrierShape;

    /// A string argument, which is what a callee looks like on this wire.
    fn s(v: &str) -> FoldArg {
        FoldArg::Str(v.to_owned())
    }

    /// One non-empty array argument, for the names that take one.
    fn arr() -> FoldArg {
        FoldArg::Array(vec![(None, s("a"))])
    }

    fn shape(name: &str, args: &[FoldArg]) -> Option<CarrierShape> {
        match fold_shape_refusal(name, args) {
            Some(FoldShapeRefusal::Carrier(c)) => Some(c.shape),
            Some(FoldShapeRefusal::Unmined) => panic!("{name} is mined"),
            None => None,
        }
    }

    /// The gate's whole reason for existing, in one table: the names the issue
    /// lists refuse a callee argument.
    ///
    /// **Independent of the allowlist by construction** — `usort`, `ob_start`
    /// and `array_udiff` are not foldable and are refused here anyway, which is
    /// the claim that a future admission cannot quietly walk past this gate.
    /// The end-to-end half, where a real engine is asked, is
    /// `array_filter_folds_only_with_no_callback` and
    /// `a_callback_carrier_never_reaches_the_runner`.
    #[test]
    fn the_callback_carriers_refuse_a_callee_argument() {
        for (name, args) in [
            ("array_filter", vec![arr(), s("var_dump")]),
            ("array_map", vec![s("strtoupper"), arr()]),
            ("usort", vec![arr(), s("strcmp")]),
            ("preg_replace_callback", vec![s("/a/"), s("strtoupper"), s("aaa")]),
            // The issue-#705 Deferred set: `mixed $callback`, so the `callable`
            // column is blank and only the curated route sees it.
            ("ob_start", vec![s("var_dump")]),
            ("pcntl_signal", vec![FoldArg::Int(2), s("var_dump")]),
            // The tail nothing declares: `array_udiff`'s comparator.
            ("array_udiff", vec![arr(), arr(), s("strcmp")]),
            // The array whose VALUES are callees.
            (
                "preg_replace_callback_array",
                vec![FoldArg::Array(vec![(None, s("strtoupper"))]), s("aaa")],
            ),
        ] {
            assert!(shape(name, &args).is_some(), "{name} handed a callee to the runner");
        }
    }

    /// …and the other half, which a gate that refused everything would also
    /// pass: a carrier-free argument list is not refused.
    #[test]
    fn an_empty_carrying_position_is_not_a_refusal() {
        let list = FoldArg::Array(vec![(None, FoldArg::Int(1)), (None, FoldArg::Int(0))]);
        for (name, args) in [
            // The acceptance case: no callback at all.
            ("array_filter", vec![list.clone()]),
            // PHP's own "no callback" spelling, which `array_filter` reads as
            // "drop the falsy elements".
            ("array_filter", vec![list.clone(), FoldArg::Null]),
            // A name with no carrying position at all.
            ("strtoupper", vec![s("ab")]),
            // The argued tail: `sprintf`'s is rendered BY the format string.
            ("sprintf", vec![s("%s-%d"), s("a"), FoldArg::Int(7)]),
            // An EMPTY array carries no callee, one level down.
            ("preg_replace_callback_array", vec![FoldArg::Array(vec![]), s("aaa")]),
            // The deferred set, with the position the call never reaches.
            ("ob_start", vec![]),
        ] {
            assert_eq!(fold_shape_refusal(name, &args), None, "{name} was refused with no callee");
        }
    }

    /// Four of the five routes are load-bearing on their own: delete one and a
    /// name only that route can see starts folding. The shape carried in the
    /// refusal is what makes the claim checkable — a test asserting only
    /// "refused" would pass with three routes doing four routes' work.
    ///
    /// The fifth, [`CarrierShape::Invoked`], has no name of its own **today**:
    /// every `invocation_shape` position is also declared `callable` in the
    /// engine's arginfo, which is the agreement
    /// `the_invocation_rows_are_all_declared_callable` pins from the catalog
    /// side. It is kept because the two tables are independent witnesses of the
    /// same fact and either may move first — a curated row for a name arginfo
    /// types `mixed` is exactly what issue #705 found three of.
    #[test]
    fn every_route_is_the_only_one_that_sees_its_name() {
        // Declared `callable`: arginfo's own column, the only route that sees
        // `array_filter`'s position 1 as a type.
        assert_eq!(shape("array_filter", &[arr(), s("var_dump")]), Some(CarrierShape::Declared));
        // Deferred `mixed` (issue #705): no declared type, no row, no tail.
        assert_eq!(shape("ob_start", &[s("var_dump")]), Some(CarrierShape::DeferredMixed));
        // The undeclared tail: `array_udiff`'s comparator, which nothing
        // declares and no fixed index can name.
        assert_eq!(
            shape("array_udiff", &[arr(), arr(), s("strcmp")]),
            Some(CarrierShape::UndeclaredTail)
        );
        // The array of callees, which no rule about types or positions sees.
        assert_eq!(
            shape(
                "preg_replace_callback_array",
                &[FoldArg::Array(vec![(None, s("strtoupper"))]), s("aaa")]
            ),
            Some(CarrierShape::InsideArray)
        );
    }

    /// The decline names the carrying POSITION, not just the name — the half of
    /// the gate an audit reads. The engine's own parameter name comes with it,
    /// since a bare index is the one thing a reader cannot check against the
    /// manual.
    #[test]
    fn the_decline_reason_names_the_carrying_position() {
        let refused = fold_shape_refusal("array_filter", &[arr(), s("var_dump")])
            .expect("a string at the callback position");
        assert_eq!(
            refused.reason("array_filter"),
            "array_filter carries a callee at parameter 1 ($callback): declared callable"
        );
        // An unmined name declines with the reason that it cannot be gated at
        // all, rather than with a position it does not have.
        assert_eq!(
            fold_shape_refusal("no_such_builtin", &[]),
            Some(FoldShapeRefusal::Unmined),
            "a name the arginfo table never had cannot be gated by a rule that reads it"
        );
        assert_eq!(
            FoldShapeRefusal::Unmined.reason("no_such_builtin"),
            "no_such_builtin has no mined parameter row, so no callee position is visible"
        );
    }
}

/// The ambient-setting gate (ADR-0101 §3.3, issue #991): a printf-family fold
/// whose literal format keeps the locale read never reaches the engine, and one
/// that shows no such conversion still does.
#[cfg(test)]
mod ambient_gate_tests {
    use super::{EngineFolder, FoldArg, FoldEngine, FoldResult, Folder, fold_reads_ambient_setting};
    use steins_sidecar::{
        ClassReflection, ConstantDefined, EnvInfo, FoldValue, PregCompile, Reflection,
    };
    use steins_syntax::ArgValue;

    fn s(v: &str) -> FoldArg {
        FoldArg::Str(v.to_owned())
    }

    /// An engine that answers every fold with `'ran'` and records the questions.
    #[derive(Default)]
    struct Counting {
        folds: Vec<String>,
    }

    impl FoldEngine for Counting {
        fn env(&mut self) -> Option<EnvInfo> {
            Some(EnvInfo {
                php_version: "8.5.11".to_owned(),
                extensions: Vec::new(),
                sapi: "cli".to_owned(),
                int_size: Some(8),
            })
        }
        fn reflect(&mut self, _target: &str) -> Option<Reflection> {
            None
        }
        fn reflect_class(&mut self, _target: &str) -> Option<ClassReflection> {
            None
        }
        fn fold(&mut self, name: &str, _args: &[FoldArg], _strict: bool) -> FoldResult {
            self.folds.push(name.to_owned());
            FoldResult::Value(FoldValue::Str("ran".to_owned()))
        }
        fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
            None
        }
        fn constant_defined(&mut self, _name: &str) -> Option<ConstantDefined> {
            None
        }
    }

    fn asked(name: &str, args: &[&str]) -> bool {
        let mut folder = EngineFolder::with_engine(Counting::default());
        let args: Vec<ArgValue> = args.iter().map(|a| ArgValue::Str((*a).into())).collect();
        folder.fold(name, &args, true);
        !folder.engine.folds.is_empty()
    }

    /// `f`, `g` and `G` refuse, whatever flags and width sit before them; every
    /// other conversion, `%%f` and a format with no conversion are admitted.
    #[test]
    fn a_format_that_keeps_the_read_does_not_fold() {
        for format in ["%f", "%.2f", "%05.1f", "%g", "%G", "%d %f", "%1$.3g", "%%%f"] {
            assert!(fold_reads_ambient_setting("sprintf", &[s(format), FoldArg::Float(1.5)]));
        }
        for format in ["%d-%s", "%F", "%.2F", "%e", "%E", "%h", "%H", "%%f", "plain", "", "%5s"] {
            assert!(!fold_reads_ambient_setting("sprintf", &[s(format), FoldArg::Int(1)]));
        }
    }

    /// What the parser cannot read keeps the read, and so does a format that is
    /// no string.
    #[test]
    fn an_unreadable_or_absent_format_does_not_fold() {
        for format in ["%q", "%d %q", "%", "%*d", "%.*F", "%0$s"] {
            assert!(fold_reads_ambient_setting("sprintf", &[s(format)]), "{format}");
        }
        assert!(fold_reads_ambient_setting("sprintf", &[FoldArg::Int(5)]));
        assert!(fold_reads_ambient_setting("sprintf", &[]));
    }

    /// The gate is about the printf family, not about the letter `f`: every other
    /// name passes through it, and `vsprintf` and `printf` share the reading
    /// even though the allowlist does not admit them.
    #[test]
    fn only_the_printf_family_is_held_to_the_format() {
        assert!(!fold_reads_ambient_setting("strtoupper", &[s("%f")]));
        assert!(!fold_reads_ambient_setting("str_replace", &[s("%f"), s("a"), s("b")]));
        assert!(fold_reads_ambient_setting("vsprintf", &[s("%f"), FoldArg::Array(vec![])]));
        assert!(fold_reads_ambient_setting("printf", &[s("%g")]));
        assert!(!fold_reads_ambient_setting("vsprintf", &[s("%d"), FoldArg::Array(vec![])]));
    }

    /// A float at a parameter declared `string` (or a union that takes it as text) does not
    /// fold, whatever the builtin; a string, an int and a float at a `float` position still do
    /// (#1024).
    #[test]
    fn a_float_at_a_string_parameter_does_not_fold() {
        use super::fold_coerces_float_to_string as refuses;
        let f = FoldArg::Float;
        assert!(refuses("strlen", &[f(1.5)]));
        assert!(refuses("strlen", &[f(f64::INFINITY)]));
        assert!(refuses("str_pad", &[f(1.5), FoldArg::Int(5), s("*")]));
        assert!(refuses("str_repeat", &[f(1.5), FoldArg::Int(2)]));
        assert!(!refuses("strlen", &[s("1.5")]));
        assert!(!refuses("strlen", &[FoldArg::Int(1)]));
        assert!(!refuses("abs", &[f(1.5)]));
        assert!(!refuses("round", &[f(1.5), FoldArg::Int(1)]));
        assert!(!refuses("str_repeat", &[s("a"), FoldArg::Int(2)]));
        assert!(!refuses("no_such_builtin", &[f(1.5)]));
    }

    /// The gate is in the seam: `strlen(1.5)` never reaches the engine, `strlen('1.5')` and
    /// `strlen(1)` do.
    #[test]
    fn strlen_of_a_float_is_not_asked() {
        let mut folder = EngineFolder::with_engine(Counting::default());
        folder.fold("strlen", &[ArgValue::Float(1.5)], false);
        assert!(folder.engine.folds.is_empty());
        folder.fold("strlen", &[ArgValue::Str("1.5".into())], false);
        folder.fold("strlen", &[ArgValue::Int(1)], false);
        assert_eq!(folder.engine.folds, ["strlen", "strlen"]);
    }

    /// A literal pattern that reads the locale tables does not fold, whatever the subject is;
    /// one that shows none does, and so does `preg_quote`, which compiles nothing.
    #[test]
    fn a_pattern_that_reads_the_tables_does_not_fold() {
        for name in ["preg_match", "preg_match_all", "preg_split", "preg_replace", "preg_grep"] {
            // `/[/` and `/a\y/` are patterns the reader declines (PCRE2 refuses them, or the reader
            // does not know them): the fold widens rather than claim the C tables.
            for pattern in [
                r"/\s/", r"/(\w)/", "/a/i", "/[[:alpha:]]/", "/^a\u{a0}b$/x", "/^i$/iu", "/[[:<:]]a/",
                "/[/", r"/a\y/", "/\\w #[\n/x",
            ] {
                assert!(fold_reads_ambient_setting(name, &[s(pattern), s("x")]), "{name} {pattern}");
            }
            for pattern in [r"/\d+/", "/a/", r"/\s/u", r"/(*UCP)\w/", "/[a-z]+:/", "/^.[/u"] {
                assert!(!fold_reads_ambient_setting(name, &[s(pattern), s("x")]), "{name} {pattern}");
            }
            assert!(!fold_reads_ambient_setting(name, &[FoldArg::Int(5)]), "{name}: no string");
            assert!(!fold_reads_ambient_setting(name, &[]), "{name}: no argument");
        }
        assert!(!fold_reads_ambient_setting("preg_quote", &[s(r"\w.")]));
        assert!(asked("preg_quote", &["\\w."]));
        assert!(!asked("preg_match", &[r"/\s/", " "]));
        assert!(!asked("preg_split", &["/a/i", "xAy"]));
        assert!(asked("preg_match", &[r"/\d/", "1"]));
        assert!(asked("preg_split", &["/,/", "a,b"]));
    }

    /// Through the seam: the engine is not asked for a call that keeps the read,
    /// and is asked for one that does not, so the gate buys silence only where
    /// the answer would have been under an assumed locale.
    #[test]
    fn the_engine_is_asked_only_when_the_format_is_locale_free() {
        assert!(asked("sprintf", &["%d-%s", "1", "a"]));
        assert!(asked("sprintf", &["%.2F", "1.5"]));
        assert!(!asked("sprintf", &["%.2f", "1.5"]));
        assert!(!asked("sprintf", &["%g", "1.5"]));
        assert!(!asked("sprintf", &["%f"]));
        assert!(asked("strtoupper", &["%f"]));
    }

    fn list(entries: Vec<FoldArg>) -> FoldArg {
        FoldArg::Array(entries.into_iter().map(|entry| (None, entry)).collect())
    }

    /// A float renderer does not fold a float the renderer writes: `strval(1.5)` and
    /// `json_encode([1.5])` are left to the row, and a value free of floats folds as before
    /// (ADR-0101 §3.15). `implode` renders one level and the walkers every level, so a float
    /// inside a nested array is a read for `json_encode` and none for `implode`.
    #[test]
    fn a_renderer_does_not_fold_a_float_it_writes() {
        let float = || FoldArg::Float(1.5);
        assert!(fold_reads_ambient_setting("strval", &[float()]));
        assert!(fold_reads_ambient_setting("json_encode", &[float()]));
        assert!(fold_reads_ambient_setting("json_encode", &[list(vec![list(vec![float()])])]));
        let mixed = list(vec![FoldArg::Int(1), float()]);
        assert!(fold_reads_ambient_setting("implode", &[s(","), mixed]));
        assert!(fold_reads_ambient_setting("join", &[list(vec![float()])]));
        assert!(fold_reads_ambient_setting("implode", &[float(), list(vec![s("a")])]));
        assert!(!fold_reads_ambient_setting("implode", &[s(","), list(vec![list(vec![float()])])]));
        assert!(!fold_reads_ambient_setting("strval", &[list(vec![float()])]));
        for name in ["strval", "json_encode", "implode"] {
            let ints = [s("a"), list(vec![FoldArg::Int(1)])];
            assert!(!fold_reads_ambient_setting(name, &ints), "{name}");
        }
        assert!(!fold_reads_ambient_setting("json_encode", &[list(vec![s("a"), FoldArg::Int(1)])]));
        // `-INF` is `-IN` at `precision=3`: a non-finite float is a read, except where the
        // renderer writes it whole or refuses it.
        assert!(fold_reads_ambient_setting("strval", &[FoldArg::Float(f64::NAN)]));
        assert!(fold_reads_ambient_setting("serialize", &[FoldArg::Float(f64::NEG_INFINITY)]));
        let inf = list(vec![FoldArg::Float(f64::INFINITY)]);
        assert!(fold_reads_ambient_setting("implode", &[s(","), inf]));
        assert!(!fold_reads_ambient_setting("json_encode", &[FoldArg::Float(f64::INFINITY)]));
        let nan = list(vec![FoldArg::Float(f64::NAN)]);
        assert!(!fold_reads_ambient_setting("json_encode", &[nan]));
        assert!(!fold_reads_ambient_setting("round", &[float()]));
        assert!(!fold_reads_ambient_setting("floatval", &[float()]));
    }

    /// `JSON_NUMERIC_CHECK` turns a numeric string into a float, so the call is left to the row.
    #[test]
    fn json_encode_with_the_numeric_check_does_not_fold() {
        let numeric = [list(vec![s("1.5")]), FoldArg::Int(32)];
        assert!(fold_reads_ambient_setting("json_encode", &numeric));
        let pretty = [list(vec![s("1.5")]), FoldArg::Int(128)];
        assert!(!fold_reads_ambient_setting("json_encode", &pretty));
    }

    /// Through the seam: the engine is not asked for `strval(1.5)` and is asked for `strval(1)`.
    #[test]
    fn the_engine_is_asked_for_a_renderer_only_when_it_writes_no_float() {
        let asked_with = |name: &str, args: &[ArgValue]| {
            let mut folder = EngineFolder::with_engine(Counting::default());
            folder.fold(name, args, true);
            !folder.engine.folds.is_empty()
        };
        assert!(!asked_with("strval", &[ArgValue::Float(1.5)]));
        assert!(asked_with("strval", &[ArgValue::Int(1)]));
        assert!(asked_with("strval", &[ArgValue::Str("1.5".into())]));
        assert!(asked_with("floatval", &[ArgValue::Float(1.5)]));
    }
}
