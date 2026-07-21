# PHP;STEINS

A PHP type checker / static analyzer in Rust — officially a parody of PHPStan
and a proving ground for BC-breaking proofs-of-concept (ADR-0016) — re-importing
Rigor's value-precise analysis model, designed from the start for LSP and automated
refactoring, with a declarative effect system as a differentiator.

## Language

### Type sources

**Native declaration**:
A type written in PHP syntax that the engine enforces at runtime (parameter,
return, property types). The highest-trust type source — a checked contract
equivalent to a static language's declaration, not a comment.
_Avoid_: type hint (deprecated PHP terminology), annotation

**PHPDoc type**:
A type expressed in a docblock (`@param`, `@return`, `@var`); advisory only —
the runtime never checks it. Steins encourages only simple one-dimensional
phpdoc types (`list<Foo>`, `non-empty-array<int, bool>`).
_Avoid_: annotation (collides with PHP attributes)

### Analysis model

**Call-site value propagation**:
The core analysis model (ADR-0001): actual argument types and values flow from
each call site into the callee body, flow-sensitively — shapes and literals
cross function boundaries by inference, not annotation.
_Avoid_: modular analysis (the rejected PHPStan model)

**Authoritative envelope**:
The role a declared type (native or phpdoc) plays in inference: an upper bound
the analyzer trusts and refines within — call-site precision may tighten
inside it, never widen beyond it.

### Syntax layer

**Syntax tree contract**:
The trait Steins owns for its lossless, error-tolerant CST (ADR-0003). All
analysis and rewriting go through it; parser backends live behind it.

**Mago**:
The existing Rust PHP toolchain (linter/formatter) whose parser is the
**adopted** backend behind the syntax tree contract (spike-verified,
ADR-0003; pinned fork). Not the contract owner.
_Avoid_: the parser (it is a backend behind our contract)

**Span+splice editing**:
The rewriting model the syntax contract guarantees: text edits are computed
from accurate node spans and spliced into the retained source bytes —
unchanged regions stay byte-identical by construction. Chosen because Mago's
tree is data-lossless but not uniformly traversable.
_Avoid_: format-preserving printing (that names the harder, rejected
tree-rendering approach)

### Execution & coverage

**PHP sidecar**:
The resident helper process — the project's own PHP (version, extensions,
autoload) running a Steins request loop — that executes real PHP calls for
literal folding. Default-on, lazily spawned, never used for syntax (ADR-0004).
_Avoid_: the PHP process, the worker; optional (it is the default)

**Folding**:
Evaluating an expression to a value-precise type at analysis time by executing
the real PHP function in the sidecar, gated by a purity allowlist.
_Avoid_: constant propagation (that is the static notion; folding executes PHP)

**Sound subset**:
The diagnostic set emitted WITHOUT the sidecar — sound (zero-FP holds) but
incomplete (findings requiring PHP execution widen away). What `--no-php`
produces.
_Avoid_: degraded mode (name the guarantee)

**Coverage posture**:
Which diagnostic set a run operated at — full fidelity (sidecar) or sound
subset — always surfaced so incompleteness is never silent.
_Avoid_: mode, level

### Extension

**Plugin**:
A fact producer for a target library or DSL — returns facts, synthetic
declarations, effect-catalog entries, and diagnostics through a core-owned
contract; not part of the inference engine. Written in PHP, hosted in the
sidecar; may boot the real framework and ask it (ADR-0012).
_Avoid_: extension (collides with PHP extension modules), addon

### Rewriting

**Fix-it**:
An autofix attached to a diagnostic as a first-class payload — the exit that
accompanies a finding (ADR-0010).
_Avoid_: quick fix (LSP protocol term; fine in protocol code only)

**Transform**:
A standalone semantic rewrite whose preconditions are spelled in types and
effects (loop→map requires purity; deletion requires empty effects). Driven
conversationally by an AI agent via dry-run → diff → approve → apply
(consult-rector's conceptual heir).
_Avoid_: rule (Rector's vocabulary; a Transform carries preconditions, not
just a pattern), codemod (generic)

### Effects

**Effect**:
What an expression does beyond computing its value (throw, output, IO, global
state, nondeterminism, …), inferred and propagated exactly like types — the
second dimension of analysis (ADR-0005).
_Avoid_: side effect (reserve for informal prose), impure point (PHPStan's
mechanism, not ours)

**Effect envelope**:
A declared upper bound on a function's effects. Its presence opts the function
into always-on contract checking; inference exceeding it is a finding. Absent
annotation, no check. Spelled with a dedicated Steins annotation (form under
decision); `@throws` is NOT the effect syntax — it stays Throwable-only, an
analogy for the declarative style at most.
_Avoid_: effect signature (implies exhaustive description; it is a bound)

**Effect catalog**:
The curated effect signatures of builtin/extension functions — together with
language constructs, the *only* origins of effects (origin closure).
Uncatalogued functions widen to unknown-effect.
_Avoid_: function metadata (PHPStan's artifact)

**Envelope carrier interface**:
An interface whose method declarations carry effect envelopes, making
DI-mediated effects checkable: call sites typed against the interface assume
the envelope; implementations must stay within it (Liskov for effects).
PSR-20's `ClockInterface` is the canonical ecosystem example.
_Avoid_: effect interface (too vague)

**Budget**:
A named inference cutoff (per-package and global) that caps propagation cost.
A budget cutoff names itself in output — `maybe` is reported as `maybe`,
silence is never manufactured (the Certainty discipline).
_Avoid_: timeout (budgets are structural, not wall-clock)

### Diagnostics

**Proof layer**:
The always-on diagnostic class: only findings proven to break on a live path,
held to the zero-false-positive bar (ADR-0002).
_Avoid_: errors (names a severity, not the class)

**Policy profile**:
A named, opt-in rule set for works-but-violates findings (coercion strictness,
annotation restraint, effect declarations). Replaces PHPStan's numeric levels.
_Avoid_: level, strictness level

**Zero-false-positive bar**:
The proof-layer discipline imported from Rigor: "the program works" outranks
the worst-case static reading; gated against a corpus of real PHP codebases.

**Crying-wolf prohibition**:
The paramount product principle (shared with Rigor): a noisy default gets the
tool discarded in the first week. Every default is quiet; noise boundaries
move only via explicit config knobs.
_Avoid_: strictness trade-off (this is not a trade-off; it is a constraint)

**Baseline**:
The acknowledged pre-existing findings a project starts from; only new
findings surface. The adoption path that replaces gradual level-raising.

**Annotation restraint** (provisional name):
The design stance that complex structural types (`array{foo: int}` shapes,
scattered `@var`) should not be hand-written: Steins infers them, and steers
code toward runtime-enforced native declarations instead. A core
differentiator from PHPStan.
