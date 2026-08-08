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

**Order-witnessed / order-declared** (ADR-0062):
The provenance split for arrays. An order-witnessed array's insertion order
was *observed* by the analysis (a literal, tracked writes, call-site
propagation) — order-dependent results are sound on it. An order-declared
array is known only through a declared shape (a key set) — reading its field
declaration order as runtime order is unsound and never done.
_Avoid_: "declared order" as a truth source (it is spelling, not order)

**Shape fact** (ADR-0062):
The one canonical abstract array fact: fields (key, presence, value) + tail
(sealed, or unsealed with key/value bounds) + denotational `isList` trinary +
non-emptiness + key covers. `array`, `array<K, V>`, `list<T>` and `array{…}`
are all degenerate cases of this single form; the contract lane's distinct
spellings lower into it at the fact boundary. Always a *single* shape — a
union of shapes lives as contract arms (where discrimination happens), never
inside one fact.
_Avoid_: mirroring the contract lane's array split into the fact domain

**KeyCover** (ADR-0062 A-G8):
A disjunctive presence fact on one array: "at least one of these keys is
there", recorded from `isset(…) || isset(…)`-style guards and consumed by
the coalesce right-arm discharge. Two flavors with different strength:
an Isset-cover promises a *non-null* member, a KeyExists-cover only a
*present* one — the flavor decides what `??` may conclude.
_Avoid_: union-of-shapes expansion (the cover is the compact form)

**Surface floor** (ADR-0062 A-G10):
The single registry attribute that places a diagnostic id on the profile
ladder (`default ⊂ contracts ⊂ strict`): the lowest surface at which the id
reports. Evidence classification (layer) and surface selection (floor) stay
separate axes.
_Avoid_: per-profile id lists, a "strict layer"

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

**Separate-process backend**:
An external tool (formatter, linter) the project already uses, which Steins
detects and orchestrates as its own process — e.g. the post-edit hook that
styles transform-generated code. Never linked, never re-emitted as Steins
output (ADR-0017).
_Avoid_: integration, bundled tool

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

**Effect label**:
The canonical identity of an effect: a hierarchical dot-path string
(`io.net.http`, `nondet.time`, `email.send`), checked by prefix subsumption
(a declared `io` admits an inferred `io.net.http`). Semantic labels
(`email.send`) layer above transport labels and may co-occur on one
declaration. Class constants are completion sugar; the string is the canon.
_Avoid_: effect kind, Kind (collides with type theory's kind), color
(internal slang only)

**Label registry**:
The set of known effect labels — core taxonomy plus plugin-registered
ecosystem/private labels. An unregistered label is a diagnostic.
_Avoid_: label catalog (the effect catalog maps *functions* to labels; the
registry lists the labels themselves)

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

**Value domain**:
The four-layer representation of what the analyzer knows about a value:
Singleton (one concrete value) → OneOf (finite set) → Refined (base type +
predicate bitset / int interval) → General (bare type). Widening is layer
descent with computed predicate summaries (ADR-0035).
_Avoid_: type lattice (names the ordering, not the representation), carrier
(Rigor's term; ours is the layered enum)

**Refined** (layer):
Base type plus refinement — predicate bitset for strings, `IntRange` for
ints. Produced by guard survival, consumed by contract acceptance; rendered
in PHPStan vocabulary (`non-empty-string`, `int<1, max>`).
_Avoid_: accessory type (PHPStan's representation, deliberately not ours)

**Liskov (substitutability)**:
The standing rule that any envelope on an abstraction binds every
implementation/override (purer, narrower-throw, wider-in, narrower-out
only — ADR-0033). Always written out as "Liskov" / リスコフ.
_Avoid_: LSP (reserved exclusively for the Language Server Protocol)

**Divergence registry**:
The tracked ledger of intentional departures from PHPStan's semantics
(ADR-0030) — each entry records the rationale and, where applicable, the
upstream proposal it feeds. Imported from rigor-rs.
_Avoid_: incompatibility list (entries are deliberate and justified, not
defects)

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

**Member-kind family**:
The id-family axis for the finding-breadth port wave: the first segment names
what kind of member or construct the finding is about (`property.*`,
`constant.*`, `variable.*`, `class-const.*`, `override.*`, `string.*`,
`untyped.*`), joining the older premise axis (`type.*` = Verified native /
`phpdoc.*` = Asserted docblock) and syntactic axis (`call.*`, `class.*`,
`offset.*`).
_Avoid_: PHPStan identifier mirroring (`property.notFound` — camelCase ids
are not Steins vocabulary)

**Dischargeable obstacle**:
A silence leg (`__call`, `__get`, `@method`/`@mixin` tags, dams) read as
*default calibration*, recorded at a granularity (per tag, with its subject)
that lets the plugin lane later declare what the magic actually provides and
re-enable the absence proof member-by-member. Obstacles are dischargeable,
never terminal.
_Avoid_: permanent omission, class-level opaque flag

**maybe- sibling**:
The possibly-grade twin of a definite finding id, spelled with a `maybe-`
prefix on the rule name and floored at `strict`
(`offset.missing` / `offset.maybe-missing` is the precedent pair). Every
"Maybe ⇒ silence" describes the default floor; the sibling names the
strict-floor end state, registered ahead of emission when its definite leg
ships.
_Avoid_: scoping the possibly-leg out of existence

**Warning-handler gate**:
The `[runtime] warning-handler = "abort" | "null"` pseudo-constant
(ADR-0049 §7): under the default `"abort"` a proven `E_WARNING` is a proven
runtime break and warning-grade ids sit on the proof layer; under a declared
`"null"` posture they demote off the proof surface. Gate boundaries and id
boundaries must coincide — one id never straddles the gate.

**Final-keyword posture**:
The `[runtime] final-keyword = "enforced" | "stripped"` pseudo-constant
(issue #234), the warning-handler gate's sibling on the ADR-0037 §2 shelf:
under the default `"enforced"` a `final` class admits no subtype, so an
intersection carrying a final arm is uninhabited; under a declared
`"stripped"` the analyzed runtime has rewritten the keyword away
(`dg/bypass-finals`), so `FinalClass&MockObject` is a type the test suite
genuinely holds. It withdraws an emptiness proof and never adds a claim.
_Avoid_: reading it as a `final`-diagnostics switch, or letting it reach
`readonly`

**Annotation restraint** (provisional name):
The design stance that complex structural types (`array{foo: int}` shapes,
scattered `@var`) should not be hand-written: Steins infers them, and steers
code toward runtime-enforced native declarations instead. A core
differentiator from PHPStan.
