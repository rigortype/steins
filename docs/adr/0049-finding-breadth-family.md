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
   claims need no dam at all; only symbol *existence* is dammed.
   *Amended 2026-07-24 (A2): scoped, not revoked — the immunity holds
   only for chains of unconditional top-level declarations with no
   runtime-surface homonym and no alias/decl collision.* The
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
   to point 8 *(amended 2026-07-24, A1: proven means `class_exact`;
   `$this`-derived receivers are membership facts and route to point
   8)*; (b) hierarchy completely enumerated per ADR-0043 §3
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
   ambiguous to a reader.
   *Amended — the warning-handler posture is a pseudo-constant setting*
   (ADR-0037 §2 family, ask-the-real-thing config edition): what a
   warning *does* is decided at bootstrap by the installed error
   handler, so `[runtime] warning-handler = "abort" | "null"` declares
   it. Default **"abort"** — the realistic-application assumption
   (owner-confirmed): handlers convert warnings to exceptions or
   terminate, so a proven `E_WARNING` is a proven runtime break and the
   proof-layer placement above needs no further argument. Under a
   declared `"null"` posture the application tolerates the warning and
   continues: the warning-backed findings leave the proof surface
   (ADR-0050 layer demotion, not deletion), and the documented
   `null`/`""` results become the propagated values
   (deferred-with-design — value-side adoption needs its own triage). Ids: `offset.missing` (key provably absent
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

## Amendment (2026-07-24): audit-hardened ladder legs

Source: the pre-implementation soundness audit
(`docs/notes/20260724-adr0049-0052-soundness-audit.md`; gaps G1–G7,
hardenings M1–M5, cited below by those names; line citations there are
at commit `2be9a14`). Every leg here is **normative** — it joins the
ladder it anchors to and ships under the point-10 discipline (the
silence fixture is written before the id fires). The audit's
confirmed-sound items (C1–C8) change no text; every new closure walk
specified below copies the C1 standard (the landed is-a oracle's
completeness bookkeeping: unresolvable or Ambiguous nodes taint
closure, cycles terminate via the seen-set, case folding matches the
engine).

**A1 (points 4a, 8; G1) — `$this` is membership, not exactness.**
"Receiver class proven" requires an *exactness-bearing* heap fact: the
heap object carries a `class_exact` bit — true for `new` and for
clone-of-exact; true for the `$this` seed only when the enclosing class
is `final` or the descent context carries exactness; false otherwise,
because the runtime object under `$this` may be any descendant, and
plain aliasing (`$u = $this`) or `clone $this` must not launder the
enclosing class into exactness. An inexact receiver **never** satisfies
point 4a: the claim routes to point 8's declared-receiver ladder
(descendant closure, dam included, contract layer) or stays silent. The
membership direction stays sound and usable — `is_a(lower_bound, T) =
Yes` holds for every descendant; only the No side and every
definite-No consumer require the bit. FP prevented (live — the audit's
13th FP class): the template-method shape, `$u = $this;
$u->handle();` in `Handler` reported as `undefined method
Handler::handle()` while the runtime object is a `MailHandler` that
defines it. The carrier fix (the bit plus its four `class_of`
consumers) is already queued as its own slice; the audit's first two
counterexamples join the current fp-gate independent of S2.

**A2 (points 2, 4; G2) — dam-freedom scoped to unconditional,
homonym-free, unambiguous chains.** Point 2's immunity sentence ("PHP
cannot reopen a defined class") is true of the *bound* class; the
ladder's method list comes from the indexed *textual* declaration, and
three legs now guard the identification of the two:
   (i) *Conditionality.* Lowering records `conditional: bool` on class
   declarations (true when nested under anything but a plain
   namespace/program node). A chain containing a conditional
   declaration **re-dams the claim** — dam clear or vouched required —
   because a guarded declaration leaves *which* definition bound to
   runtime load order (the fallback-stub shape: `if
   (!class_exists(…))` beside a dam-site include that defines the real
   class with the "missing" method). Dam-freedom survives only for
   chains of unconditional top-level project declarations — which
   legacy classes overwhelmingly are, so the point-2 core-yield
   prediction stands.
   (ii) *Runtime-surface homonyms.* Any chain FQN that the runtime
   boot surface also knows as a builtin/extension class-like forces
   Unknown: the textual twin (polyfill/stub packages vendored as
   source, ADR-0015) may be dead code shadowed by the loaded extension
   class — a variant with no dam site at all; points 3/5 already carry
   the builtin-shadow guard that point 4 lacked. **Divergence
   from the audit, recorded:** the audit defers this leg to the reflect
   slice and offers "FQN present in the builtin hierarchy table ⇒
   silent" as the pre-reflect approximation. Rejected as the normative
   form: the leg's load-bearing direction is permissive — firing
   requires the name be *absent from the boot surface* — and absence
   from the 352-entry hierarchy table proves nothing (point 1's own
   dictum: the catalog is never an absence oracle). The homonym
   question is an *existence* question, and the S1 sidecar existence
   surface (point 1 oracle (b)) answers it — so the leg consumes that
   surface from S2 on, one cached not-found per chain class, with no
   wait for the reflect slice's full method enumeration. Consequence,
   stated honestly: **without a sidecar, `call.undefined-method` is
   Unknown-silent like points 3/5** — the ADR-0004 sound subset now
   covers the flagship, because the homonym question has no textual
   answer. Table presence remains a valid extra silence (a subset of
   the sidecar's answer); it is never a firing license.
   (iii) *Alias/decl collision.* A literal `class_alias` edge colliding
   with a textual declaration of the same FQN — or two literal alias
   edges for one name — is `Ambiguous`: existence present, identity
   unresolved; method-absence and point-8 closure treat it as any
   Ambiguous node. Stated in S1 beside the alias-edge machinery.

**A3 (points 4, 6, 10; G3) — enums are Unknown until their methods are
lowered, and the reflect slice's gate says so.** Enum lowering today
drops method bodies *and* the trait-use marker; every enum chain
reaches the builtin `UnitEnum`, so point 4f silences by accident. Two
deferrals — enum method lowering (ADR-0043, deferred with the
method-transform stage) and builtin-ancestor enumeration (point 4f,
waiting on the reflect slice) — are individually safe and **jointly
unsound**: the reflect slice would make the enum chain "fully
enumerated" over an empty method list, and every enum method call plus
the engine-provided `cases()`/`from()`/`tryFrom()` (textless in any
source) reports undefined. Normative now: `is_enum` with
enum-methods-not-lowered (today: every enum) ⇒ Unknown for
method-absence (point 4) and arity (point 6), independent of what the
chain enumeration says; and the reflect slice's gate carries the
written precondition *complete enum lowering — method bodies, the
trait-use marker, and the engine-provided statics sourced from the
sidecar surface, never the textual index*. The bomb and the unlock
that arms it are now recorded in the same place.

**A4 (point 8; G4) — the descendant closure's enumeration legs.** The
closure enumerates over *declarations*, not the deduped index — both
halves of an Ambiguous-FQN pair count as potential descendants; parent
matching follows literal `class_alias` edges (`class B extends
LegacyName` beside `class_alias('T', 'LegacyName')` makes B a
descendant of T); and when the union member is an interface, the edge
set includes `implements`, interface-`extends`, and enum-`implements`.
Anonymous classes are invisible to the class index as lowered, so v1:
the presence of any anonymous class whose extends/implements edge
resolves to a union member — or is Unknown against one — is itself a
closure obstacle ⇒ Unknown. FP prevented: `new class extends Report {
public function toPlainText() … }` invisible to a "completely
enumerated" descendant set of `Report`. Recorded refinement: edge-only
lowering of anonymous classes (parent/implements refs plus member
names, no FQN), which shrinks the obstacle to actual edge matches.

**A5 (point 2; G5) — "proven to resolve in-universe" defined
strictly.** Only **absolute** literal include paths and
`__DIR__`-anchored concatenations qualify as provable-in-universe; a
bare relative literal (`include 'config.php';`) is **Unproven — a dam
site**. Runtime resolution of a relative path consults `include_path`
and the CWD before falling back to the including file's directory, so
directory-relative belief is unsound: a same-named in-universe
neighbor makes the dam believe the universe closed while runtime loads
an out-of-universe twin defining symbols the scan never sees.
Recorded opt-in refinement: a `[runtime] include-path` pseudo-constant
(ADR-0037 §2 family) declaring the boot truth re-qualifies relative
literals against the declared path; the exact 8.5 precedence order
(including the `./`-prefix CWD binding) goes through `php -r`
verification before that refinement is worded — this ADR's own
discipline. The audit notes the same under-damming is latent in the
landed transform-side benign-include oracle; the checker dam and the
transform oracle take the corrected rule from one shared judgment.

**A6 (points 3, 5; G6) — the existence oracle records its SAPI.**
"The project's own PHP" is not one function table: the sidecar runs
CLI, production runs FPM/Apache, and per-SAPI ini files load different
extension sets. Legs: (i) the coverage posture records the sidecar's
SAPI, and existence claims are honest to it; (ii) a curated
SAPI-provided symbol set (`fastcgi_finish_request`, the `apache_*`
family, `litespeed_*`, …) is **never Absent** while the serving SAPI
is undeclared — FP prevented: `fastcgi_finish_request()` reported
undefined by a CLI sidecar though defined in every environment that
executes the path; (iii) `[runtime] sapi` (ADR-0037 §2 family)
declares the serving surface and unlocks the curated names against the
declared truth. The same reasoning covers extension classes loaded by
only one SAPI's ini under point 5.

**A7 (point 7; G7) — the read-context whitelist.** The severity table
is per-operation; the ladder is now too. `offset.missing` fires
**only** in: plain rvalue reads (return operands included), argument
positions whose parameter is proven non-by-ref on a *resolved* target,
and `foreach` subjects. Every other context is a silence leg with one
pinned fixture each: `isset`/`empty`/`??`/`array_key_exists`/`unset`,
write positions (assignment targets, autovivifying nested writes,
`&$a[k]`, `list()`/foreach-list targets), by-ref argument positions,
and **any argument of an unresolved or unreflected callee** —
by-ref-ness is unknowable there, and `f($a[0])` into `function
f(&$x)` autovivifies silently at runtime. Compound assignment
(`$a[0]++`, `$a[0] .= …`) stays out of the whitelist pending `php -r`
verification of the read-half warning on 8.5; unverified contexts
default to silence.

**A8 (points 3, 5; M2) — the reference form must be fully resolved.**
PHP's relative form `namespace\foo()` / `new namespace\Bar` has no
lowered ref kind today and would resolve to a doubled prefix
(`Ctx\namespace\foo`) — Absent by construction, an FP-manufacturing
bug the moment points 3/5 fire. Lowering gains a distinct Relative ref
kind, normalized against the enclosing namespace, **before S4**; the
point-3 ladder gains the leg "the reference form is fully resolved",
and until the kind exists, any reference lowering to a raw
`namespace\`-headed name is Unknown-silent.

**A9 (family-global; M3) — monkey-patch extensions void the family.**
If the sidecar's loaded-extension list contains a runtime-redefinition
extension (`uopz`, `runkit7`, `Componere`), every id in point 9 —
including point 8's final-immunity leg, since finality itself stops
binding — is Unknown-silent, and the coverage posture names the
extension. One lookup against the point-1 oracle; these are dev
tooling in practice, so the posture will rarely fire in anger, but
with any of them loaded no absence claim holds.

**A10 (point 7; M5) — one key canonicalization, both sides.** The
read-side key lookup reuses the engine's array-key canonicalization
exactly as landed on the write side (`php_canonical_int_string`
semantics: `"5"` → 5; `"05"`/`"+5"`/overflow stay strings; bool → int;
null → `""`; finite floats truncate) — the same shared helper, never a
parallel comparison, so `$a = [5 => 'x']; $a["5"]` is present, not
missing. String-numeric keys and negative offsets behave per the
landed lowering and the verified table; if negative string offsets
enter scope they get their own verified row first.

**A11 (point 8, ADR-0052 §2/N4; M1) — version-skew demotion for
catalog-backed is-a.** The builtin hierarchy table is mined at a
pinned php-src version; when the sidecar-reported PHP minor differs
from the pin, catalog-backed is-a verdicts consumed for **arm
deletion** (ADR-0052 §2's class arms: the positive-branch final-arm
`No`, and any negative-branch `Yes` riding a catalog edge) and for
point 8's descendant closure **demote to Unknown** — the arm and the
claim survive. A skewed edge set can fake No-under-closure where truth
is Yes, wrongly delete a final arm, and manufacture
`phpdoc.undefined-method` on the wrongly narrowed receiver.
`class.undefined` is already double-locked by its sidecar leg; the
narrowing arms were not, and now are. Recorded refinement: per-minor
table generation selected by sidecar version; blanket demotion is v1.

**Sequencing (point 10, restated with the audit's consequences).**
Before S2: A1's carrier fix, A2's three legs, A3's silence condition,
and ADR-0052's N2 (the checked stratum bit — see that ADR's amendment;
every S-slice consumes env facts). Before S3: A7, A10. Before S4: A5,
A6, A8, plus measurement mode as designed. Before S6/N4: A4, A9, A11.
The reflect slice carries A3's precondition in its own gate. Every
"verify against PHP 8.5" above goes through `php -r` before its leg is
worded — never recall.
