# Finding breadth: undefined symbols, arity, offset access as absence proofs

Every landed id proves a *presence*: a known value meets a known contract
and breaks it — one witness suffices. The ROADMAP gap-1 family (a checker
silent on `$foo->tyop()` is not adoptable) makes *absence* claims: a
symbol defined nowhere, a required argument never passed, a key no
execution put there. Absence inverts the evidence rule — presence needs
one fact, absence needs closure over every place a witness could hide.
This is ADR-0043's is-a discipline generalized: **definite No only under
complete enumeration**; every ladder below is that sentence applied to a
different universe. Runtime semantics were verified against PHP 8.5.8
(`php -r`), not recalled; the verified tables are normative for the
message register.

1. **Two existence oracles; the catalog is never one.** Existence closure
   draws on (a) the *textual universe*: the project index over project +
   vendor-as-source (ADR-0015), which collects declarations at any
   nesting depth — a `function_exists`-guarded polyfill body is a
   definition site, so a conditionally-defined name is never Absent, and
   duplicate FQNs are `Ambiguous` (both are silence, ADR-0046's
   definition-havoc absorption); and (b) the *runtime boot surface*: the
   sidecar asked on the project's own PHP (ADR-0004 ask-the-real-thing)
   whether a name exists among builtins and loaded extensions —
   `reflect(target)` with a structured not-found result (ADR-0024's
   surface; no new protocol method), which is also what finally closes
   the extension-symbol Unknown-silent gap for this family. The builtin
   catalog is *rejected as an absence oracle*: `foldable`/effect-label
   entries are curated seedings and the 352-entry hierarchy enumerates
   ancestry, not the function namespace — "not in catalog" proves
   nothing. Without a sidecar, the sound subset (ADR-0004) keeps
   `call.undefined-function` and `class.undefined` silent, and the
   coverage posture says so.

2. **The runtime-definition dam** (ADR-0046 applied checker-side).
   Verified: `eval` defines functions and classes at runtime; so does an
   include the analysis cannot see into. Therefore function- and
   class-absence claims are dammed by the universe's dynamism sites —
   the already-lowered `DynamismSite` set: any `eval`, any non-vendor
   include whose path is unproven or proven out-of-universe (the vendor
   presumption of ADR-0046 §2 carries over verbatim), plus non-literal
   `class_alias` — a runtime name mint the reference scan cannot
   resolve (literal `class_alias` arguments instead contribute alias
   edges to the index).
   Code-generating autoloaders need no separate analysis: generating a
   class requires `eval` or an out-of-universe include, so the dam
   already catches them — one mechanism, not two. The dam is
   whole-universe in v1 (checker-side region scoping is exactly
   ADR-0047 §9's deferred promotion of the region map into engine
   config; when that lands, damming adopts nameability scoping
   unchanged). The ADR-0046 **vouch valve extends to the checker**: a
   vouched dynamism site lifts the dam, and the run's claim downgrades
   visibly — findings unlocked by vouches ride the user-assertion
   stratum (ADR-0037) and the report/doctor posture says "conditional on
   N vouched dynamic-code exemptions"; silent promotion to the proven
   stratum is rejected. **The immunity asymmetry is the design's core
   yield**: PHP cannot reopen a defined class — no runtime construct
   adds a method to a class the index resolved — so *method*-absence
   claims need no dam at all; only symbol *existence* is dammed. The
   family's strongest id is therefore the one legacy code needs most.

3. **`call.undefined-function`** — all four legs, or silence: (a) every
   candidate FQN the reference can denote under PHP resolution order
   (`use function` import; `NS\name`; global fallback) is `Absent` —
   not Ambiguous, not builtin-shadowed; (b) the sidecar answers
   not-found for every candidate on the project's own PHP; (c) the dam
   is clear or vouched; (d) the call is a real call (first-class
   callable syntax, `$fn()`, `call_user_func` shapes are out of scope —
   the closure-wave and value lanes own those). Verified consequence
   claimed: fatal `Error: Call to undefined function f()`.

4. **`call.undefined-method`** — the closed-world flagship, dam-free
   (point 2): (a) receiver class **proven** — heap exact class
   (ADR-0036), `new`, or proven propagation; declared-only receivers go
   to point 8; (b) hierarchy completely enumerated per ADR-0043 §3
   (every ancestor edge resolved in-project or in the builtin
   hierarchy); (c) the method absent — case-insensitively, verified —
   from every class in the chain; (d) **no `__call` anywhere in the
   chain** (verified: it swallows any name — no error); (e) **no trait
   use anywhere in the chain**: the is-a oracle rightly ignores traits
   for ancestry, but traits add *methods*, so method-absence forces
   Unknown on any `uses_traits` class until trait flattening exists
   (deferred-with-design, one line: flattening is name-set union at
   class-lowering time, conflict-resolution rules and all, and nothing
   here blocks it); (f) every chain class is project-defined — a chain
   reaching a builtin ancestor waits for the sidecar reflect slice to
   enumerate that class's method surface (M2 gap 7), Unknown-silent
   until then. Static calls `Foo::bar()` share the id and ladder with
   `__callStatic` in place of `__call`; the class is textual, so no
   receiver proof is needed; `self`/`static`/`parent` stay unlowered
   and silent (ADR-0043 §1). Verified consequence claimed: fatal
   `Error: Call to undefined method C::tyop()`. Calling an instance
   method statically (`Error`, verified) is an adjacent deferred id,
   one slice behind this machinery.

5. **`class.undefined`** — only at positions verified to hard-error:
   `new`, static method call, class-const fetch, static property fetch
   (`Error: Class "X" not found`). Explicitly **not** findings, per the
   verified table: `instanceof` an undefined class evaluates `false`
   (feeds branch analysis as a value fact instead); `catch` of an
   undefined class matches nothing and errors nothing; `X::class` has
   been a plain string since 8.0; an undefined class in a *type
   declaration* never errors at the declaration — the call site fails
   as an ordinary `TypeError`, which the existing acceptance lane may
   some day prove, and a declaration-coherence lint here is refused
   (ADR-0030 silences §2). Ladder: index Absent (alias edges included)
   + absent from the builtin hierarchy + sidecar not-found + dam clear
   or vouched. One verified trap is a ladder leg of its own: lowering
   collects classes, interfaces, and enums but **not traits**, and a
   static method call through a *trait* name runs (deprecated, not an
   error) — so the closure set is the class-*like* name set, traits
   included; trait names enter the index (as names, not yet flattened
   members) before this id fires. Abstract-class and interface
   instantiation (both fatal
   `Error`, verified) are adjacent ids reachable trivially once this
   resolution lands — deferred one slice, not designed away. So is
   `new` on a trait name (fatal `Error: Cannot instantiate trait`,
   verified — a distinct claim, hence a distinct message, never folded
   into `class.undefined`).

6. **Arity** — the verified PHP 8.5 table is the spec, and it is
   asymmetric: *too few* to a userland function is always
   `ArgumentCountError` (strict_types-independent); *too many* to a
   userland non-variadic is **silent at runtime** (extra args are
   simply ignored) and therefore **never a finding** — the ADR-0002
   consequence pattern verbatim, whatever PHPStan reports; internal
   functions throw `ArgumentCountError` in *both* directions
   (`strlen("a","b")` verified); an unknown named argument is a fatal
   `Error` on non-variadics while a variadic silently collects it
   (verified: `fv(x: 1)` → `{"x":1}`); a named argument overwriting a
   positional is a fatal `Error` (deferred id, one line). Initial ids:
   `call.too-few-arguments`, `call.too-many-arguments` (internal
   targets only, by semantics), `call.unknown-named-argument`.
   Provability: a uniquely resolved target with ground-truth signature
   — user functions from the index; **methods only under a proven
   exact receiver**, because the declared-receiver variant is not
   merely contract-conditional but *unsound*: verified, an override may
   add optional parameters (`P::m(int $a)` / `Q::m($a = 0, …)`), so
   `$p->m()` on a declared `P` holding a `Q` satisfies the contract
   and runs — a finding there is a false positive, refused outright,
   not deferred. Static calls are textual-exact and qualify. Internal
   targets take their arity from sidecar reflection of the project's
   own PHP (version-dependent signatures are never recalled from
   memory or catalog), so the internal arm ships with the reflect
   slice. Call-site conditions: no argument unpacking (`...$args`
   makes the count unproven; counting proven `Singleton` arrays is
   deferred, one line), named-argument binding fully resolved against
   the target's parameter names. Claimed consequences: the verified
   `ArgumentCountError` / `Error` messages.

7. **Offset access** — verified severity table: reading a missing array
   key is `E_WARNING` ("Undefined array key k") yielding `null`; an
   uninitialized string offset is `E_WARNING` yielding `""`; an offset
   read on `null`/`int`/`float`/`bool` is `E_WARNING` yielding `null`;
   an offset read on a non-`ArrayAccess` object is a fatal `Error`; a
   string-typed key on a string is a fatal `TypeError`. **Decision: a
   proven `E_WARNING` is proof-layer reportable.** The zero-FP identity
   is a bar on *certainty*, not a floor on *severity*: the claim ("this
   read provably emits Undefined array key 0 and yields null") is
   exact, runtime-observable misbehavior; PHP itself promoted these
   notices to warnings in 8.0; and the poisoned `null`/`""` routinely
   chains into a fatal the engine can also prove (the conformance case
   `assertions_assert_non_empty_list` returns the `null` from an
   `: int` function). The message register
   carries the runtime consequence verbatim so severity is never
   ambiguous to a reader. Ids: `offset.missing` (key provably absent
   from a proven container value) and `offset.on-unsupported` (proven
   non-offsetable base: the object case is `Error`-grade, the
   scalar/null cases warning-grade; `call.on-null`'s discipline, same
   family shape). Provability is **value-domain evidence only** in v1:
   a `Singleton` container is the whole value, so key absence is
   definite — including `Singleton([])` produced by an `=== []` guard;
   a `OneOf` fires only when every member lacks the key; `Refined`
   non-empty-list proves offset 0 *present* (silence, never a
   finding); `General` is silent. This is what closes
   `assertions_assert_non_empty_list`: the `=== []` branch fires, the
   declared-`list<int>` read of unknown emptiness stays silent with no
   narrowing machinery required. `ArrayAccess` receivers are silent
   (`offsetGet` is arbitrary code). Writes are silent (writes create
   keys; string-offset writes pad). Declared-shape claims (`array{…}`
   as a sealed key set, ADR-0030 registry 2 / #14939) are the phpdoc
   lane and are deferred-with-design: they need the sealed/unsealed
   marker surfaced through lowering and belong with the contract
   family's tripwire discipline, not this opening.

8. **The declared-receiver lane: `phpdoc.undefined-method`** (contract
   layer — the paired-id precedent of `type.property-mismatch` /
   `phpdoc.property-mismatch`, and the family prefix is what M2 layer
   separation filters on). When the receiver type is a phpdoc contract
   narrowed by branch analysis (the conformance shape: `User|Guest`
   minus `instanceof User` leaves `Guest`), the claim is conditional on
   the contract — but *conditional is not enough*: a subclass of the
   declared class also satisfies the contract and may define the
   method, and `eval` can mint such a subclass. So the ladder adds
   **descendant closure**: every remaining union member is `final`
   (immune — extending a final class is fatal), or its project-wide
   descendant set is completely enumerated *and* the dam is clear;
   plus the point-4 legs (chain closure, no `__call`, no traits,
   project-defined chain). Arity has no such lane at all (point 6's
   unsoundness). This is what closes `assertions_instanceof_narrowing`
   — its `Guest` is `final`, exactly the immune case. The id
   interlocks with the M1 narrowing queue: instanceof-subtraction over
   declared unions is what produces the narrowed receiver, and neither
   side ships without the other.

9. **Register and messages.** New ids, all `family.rule-name`
   (ADR-0022), appended to `DIAGNOSTIC_IDS`, prefix-suppressible
   (`call.*` now spans presence and absence claims — acceptable, prefix
   semantics are deliberately coarse): proof layer —
   `call.undefined-function`, `call.undefined-method`,
   `class.undefined`, `call.too-few-arguments`,
   `call.too-many-arguments`, `call.unknown-named-argument`,
   `offset.missing`, `offset.on-unsupported`; contract layer —
   `phpdoc.undefined-method`. Messages speak PHP's own verified
   phrasing first, closure evidence second, e.g.: `call to undefined
   function App\tyop() — not defined in the project, not on PHP 8.5.8
   (32 extensions)`; `call to undefined method Order::tyop() —
   hierarchy fully enumerated (Order → AbstractOrder), no __call`;
   `too few arguments to format(): 1 passed, 2 required — provable
   ArgumentCountError`; `offset 0 provably missing — $values is [] on
   this path; reads null with "Undefined array key 0"`. Familiar
   spelling is the ADR-0030 governing rule; the evidence clause is the
   Steins differentiator and doubles as the triage handle.

10. **Rollout: staged opening under the gate discipline** (ADR-0043 §5
    precedent, plus the descent guard-blindness lesson inverted for
    absence: a walk that misses one silence condition *manufactures*
    certainty, so every ladder leg ships with a fixture proving silence
    when that leg fails — the silence matrix is written before the id
    fires). Stages, each boundary fp-gate + conformance verified:
    (S1) groundwork, zero behavior: sidecar existence surface,
    checker-side dam aggregation over `DynamismSite`, literal
    `class_alias` edges, trait names into the class-like index, ids
    registered but never emitted;
    (S2) `call.undefined-method` — dam-free, the resolution and is-a
    machinery already exist — with verbatim corpus triage;
    (S3) `offset.missing`/`offset.on-unsupported` on value evidence;
    (S4) `call.undefined-function` + `class.undefined` — the FP-risk
    hotspot: opened in **measurement mode first** (counted, not
    printed), every finding class triaged verbatim (5-sample minimum),
    true positives pinned as `EXPECTED_PROOF_FINDINGS` rows before any
    promotion to firing;
    (S5) arity — userland `too-few`/`unknown-named-argument` first,
    internal arms after the reflect slice;
    (S6) `phpdoc.undefined-method` with the contract families' tripwire
    management, sequenced with the narrowing queue. Expected corpus
    impact is **unknown until measured, and saying otherwise would be
    an impression, not a number** (the M1 exit criterion demands the
    number); the falsifiable prediction to judge the design by: the
    pinned OSS packages are largely dynamism-free, so S4 fires there,
    while the legacy monorepo stays dammed for S4 until vouches or
    ADR-0047 region scoping — if S2 also yields nothing there, the
    design's core-yield claim (point 2) was wrong and gets revisited.
    Replayability (ADR-0048): every oracle here is a query answer
    (index, hierarchy, sidecar, dam state as a whole-universe input) —
    no mid-walk cross-scope coupling, no ordering dependence, and the
    dam is recomputed per run like any other fact of the universe.

11. **What stays out, anchored**: maybe-undefined method on a union
    that merely *includes* a lacking class — worst-case
    maybe-reporting, refused (ADR-0002, ROADMAP won't-build);
    possibly-missing offset on `General` maps/lists — same anchor;
    too-many arguments to userland — runs clean, never a finding
    (point 6; at most a future policy profile); undefined classes
    referenced only in phpdoc — the contract is unverifiable, so the
    contract stays closed and silent (`contract_touches_class`), a
    coverage-posture surface for `doctor`, never a finding
    (declaration-coherence refusal, ADR-0030 silences §2); the
    undefined-*property* family — verified warning-grade like offsets,
    but dynamic properties, `#[\AllowDynamicProperties]`, `__get`, and
    `stdClass` make its closure ladder a design of its own,
    deferred-with-design; `readonly`/`unset` offset-existence
    interactions with the heap — deferred until the object-shape work
    needs them.
