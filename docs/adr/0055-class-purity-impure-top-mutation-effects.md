# Class-level purity defaults, the `Impure` top envelope, and the mutation label family

The effect system (ADR-0005/0006/0018) checks envelopes on the
declarations that opt in, and the heap (ADR-0036) tracks property
writes precisely — by allocation identity, with `$this` as a
pre-escaped id — yet the two have never been connected: the `mutate`
label is registered in the taxonomy but **no inference source and no
catalog entry colors it** (verified against `steins-catalog`'s seed
table: `shuffle` carries only `nondet.random`; `sort`/`array_push` are
uncatalogued; the effects spec's origin table has no property-write
row). The consequence is a hole: a `#[\Steins\Pure]` method that
writes `$this->p` passes silently, and "Pure" does not mean what it
says. This ADR closes that hole and lands the three owner-decided
postures around it: **no mandatory annotation, ever** (the ADR-0006
opt-in identity and the lenient-default principle, ADR-0050
amendment); **implicit self-mutation must not pass as `Pure`**; and
**`#[\Steins\Impure]` exists as the deliberate anti-enumeration escape
hatch, designed as the top envelope (⊤) — checked-but-unbounded, never
"unchecked"**. Post-v0.1.0: this ADR binds future slices; nothing here
lands in the release.

## Part I — the mutation label family

1. **`mutate` becomes a family parent; the restructure is BC-free by
   measurement.** The family:

   - **`mutate.arg`** — mutation of a caller-owned value through a
     by-ref parameter or by-ref binding: the future coloring for
     `sort`/`usort`/`array_push`/`array_splice`, `shuffle` (which
     gains it *beside* `nondet.random` — one call, two colors), and
     the by-ref callback position of `array_walk` (whose invocation
     shape already records the by-ref discipline for folding; the
     effect color joins it).
   - **`mutate.self`** — a property write whose base object is
     `$this`-aliased, decided by the heap's **allocation identity,
     not syntax** (ADR-0036): `$that = $this; $that->p = 1;` is
     `mutate.self`; a write through a variable holding a different
     allocation id is not, even when the class matches.
   - **`mutate.instance`** — a property write through a
     non-`$this`-aliased receiver. The split from `mutate.self` is
     semantically real, not stylistic: PHP visibility is
     **class-scoped, not instance-scoped** — `$other->privateProp = x`
     is legal inside the declaring class — so "mutates its own
     object" and "mutates some object" are distinct, observable
     contracts, and a method carrying only `mutate.self` genuinely
     cannot have touched another instance.
   - **`mutate.static`** — a static property write (`self::$x`,
     `static::$x`, `Foo::$x`). See point 4 for its N5 relationship.

   Prefix subsumption (ADR-0018) does the compatibility work: a
   declared `#[\Steins\Effect('mutate')]` admits all four children,
   so any existing coarse declaration keeps meaning "may mutate,
   unspecified how." On the catalog side the restructure moves
   nothing: today's registry entry is a root with zero carriers, so
   no builtin recolors and no inferred set changes shape behind
   anyone's envelope. (Bodies will *gain* labels — that is Part II's
   deliberate tightening, not a relabeling.)

2. **Inference is always-on; findings remain envelope-gated.**
   Property writes already exist in the walk — the heap records them
   per allocation id today. The slices add only the labeling: each
   write contributes `mutate.self` or `mutate.instance` to the
   containing declaration's inferred effect set, which propagates to
   the fixpoint like every other label and prints in `annotate`
   margins. **No finding fires without an envelope** — binding
   constraint, restating ADR-0006: unannotated declarations stay
   unchecked, and the lenient-default principle governs the surface
   (the two effect contract findings stay behind `--profile
   contracts`). Inference-always/checking-opt-in is the same split
   the throw dimension already lives by.

3. **`#[\Steins\Effect('mutate.self')]` is the observational-purity
   tier**, the `&mut self` of the taxonomy: the method may mutate its
   own object and do nothing else — no I/O, no globals, no output, no
   foreign-instance writes. What it buys, concretely:

   - **Testability**: construct, call, assert on state — no mocking,
     no world. The project's stated end goal is structurally
     separating effectful code from testable code (handbook 04); this
     label names the largest testable class of methods that `Pure`
     excludes: setters, builders, accumulators, `withX`-style
     mutators.
   - **The modernization mission**: effect-splitting a 2007-era god
     class means separating the data half from the gateway half. The
     data half's honest envelope is exactly `mutate.self` (a DTO or
     builder is not pure — it mutates itself — but it is *only* that);
     the margin count of mutate.self-only methods is the measurable
     inventory of extractable state, and the annotate margins over the
     legacy monorepo become the instrument (point 15).

4. **`mutate.static` registers now; its fact channel stays N5's.** Two
   different machines are involved and only one is deferred. The
   effect *origin* is structural — a static property write is
   detectable in the walk exactly as `echo` is (ADR-0005 origin
   closure; the origin scan is structural, not reachability-aware) —
   so the label needs no heap channel and can land with the family.
   The static-property *fact* channel (narrowing facts on `self::$x`,
   the every-call-invalidated store) is ADR-0052 N5, owner-deferred
   out of v0.1.0; nothing here reopens or waits on it, and when N5
   lands it consumes this label's origin sites unchanged. Dynamic
   forms (`$class::$x`) contribute no label and taint exhaustiveness,
   per the standing dynamic-origin rule. Static writes are also the
   global-state family (ADR-0036's classification): `mutate.static`
   deliberately lives under `mutate`, not `global` — `global.*` names
   the interpreter-global surfaces (superglobals, ini, env), while a
   static property is class-owned state; a policy that wants both
   names both.

## Part II — `#[\Steins\Pure]` strictness

5. **The empty envelope excludes `mutate.self` automatically — no new
   rule.** Once the labels are inferred, the existing
   `effect.envelope-exceeded` machinery does everything: a
   Pure-enveloped method with a proven `$this->p = …` write exceeds
   the empty set, pinpointed at the write. `Pure` finally means strict
   purity — no self-mutation — which is the owner's constraint
   verbatim.

6. **The present hole is classified as a design gap, now closed — not
   an accepted limitation.** ADR-0005's origin-closure argument
   ("primitive effects arise only from language constructs and
   catalogued functions") is correct, but the enumeration of language
   constructs (spec: `echo`/`print`/`<?=`, `exit`/`die`) simply never
   listed property assignment, and no ADR recorded that silence as
   accepted. The `mutate` label existed with no inference source; the
   heap landed (ADR-0036) without connecting to it. Nothing was ever
   *unsound* — silence is the safe side of proven-only checking, and
   the zero-FP identity was never at risk — but a `Pure` annotation
   was checkable against less than its name claimed. This ADR is the
   closing record.

7. **The BC consequence is a deliberate tightening, recorded as
   such.** Existing `#[Pure]`-annotated code that self-mutates — and
   equally an `#[Effect('io')]` method that also writes `$this` —
   will newly fire `effect.envelope-exceeded` once the labels infer.
   Owner intent, not collateral (constraint 2: such a method "cannot
   honestly carry the Pure name"). Three softeners, all structural:
   the finding is contract-layer, so a bare `check` shows nothing new
   (the default surface is proof + mechanics, ADR-0050); only
   annotated declarations are affected at all (the opt-in identity —
   whoever wrote `Pure` asked for exactly this check); and the
   migration is mechanical — `steins annotate` prints the newly
   inferred labels in the margin, and the fix is one of
   `#[Effect('mutate.self')]` (the honest middle tier), a wider
   `#[Effect(...)]`, or `#[Impure]` (point 8). The corpus carries no
   envelope annotations today (ADR-0050 §9), so the gate's contract
   tripwires stay vacuous through the landing.

## Part III — `#[\Steins\Impure]`, the top envelope

8. **`Impure` is ⊤: checked-but-unbounded, never "unchecked".** Its
   envelope is the universal label set — it admits every label,
   present and future, known and unknown, so `effect.envelope-exceeded`
   is vacuously unfireable against it. It is nonetheless a *checked*
   declaration, and the difference from unannotated is real at every
   consumer:

   - **Liskov**: an `Impure` abstraction admits any implementation
     (⊤ bounds nothing). Under a narrower abstraction, the
     implementation's operative bound is the **meet** of its own
     declaration and the abstraction's envelope — so a `Pure`
     interface method implemented by an `Impure` method is checked
     against ∅ (⊤ ∧ ∅), and any proven effect fires
     `effect.liskov-widened` with the bound attributed to the
     abstraction. `Impure` buys no slack under a promise someone else
     made; the proven-only rule is untouched (a pure-bodied
     `Impure`-declared implementation stays silent — declaring ⊤ is
     not proof of anything, and declared-vs-declared coherence
     checking remains the refused declaration-coherence family,
     ADR-0050 §11 / ADR-0030 §2: at most a policy profile, never
     core).
   - **Class defaults**: under a class-level `Pure`/`Effect` default
     (Part IV), a per-method `Impure` is *the* opt-out spelling.
   - **Accounting**: an `Impure` declaration counts as annotated —
     doctor's written-but-unchecked-envelope notice, coverage counts,
     and the annotate margin all distinguish "nobody has looked"
     (unannotated) from "looked; effects deliberately unbounded"
     (`Impure`). That distinction is the attribute's whole reason to
     exist: it answers "I don't want to spell out this method's
     effects" without becoming a silence.
   - **Folding**: ⊤ never licenses folding (the license is the empty
     set — point 13).
   - **Caller-side**: for resolved calls, propagation consumes the
     *inferred* body as always (an envelope is a checked bound, never
     an inference source). At DI seams where the envelope is what call
     sites assume (ADR-0005 interface-mediated effects), an `Impure`
     envelope widens the caller to the `…?` exhaustiveness taint —
     honest, and operationally the same as undeclared *for the
     caller*; the declaration-side meaning (accounting, opt-out,
     meet-rule participation) is where ⊤ differs.

9. **Refusal, anchored: `Impure` as an unchecked opt-out.** The
   alternative — `Impure` disables effect checking on the node — is
   refused on the named-silences argument: Steins has exactly three
   suppression channels, each visible, reasoned, and rot-checked
   (ADR-0023); an attribute that silences a checker is a fourth,
   in-source, unbudgeted silence with no `suppress.unmatched`
   analogue. It would also punch a Liskov hole (a `Pure` abstraction
   implemented by an "unchecked" method passes silently — exactly the
   widening the lane exists to catch), and it would make `Impure`
   equivalent to deleting the attribute, i.e. meaningless. ⊤ keeps
   every consumer sound and makes the declaration say something true.

10. **One new mechanics finding: `effect.contradictory-envelope`.** A
    bounded envelope (`Pure` or `Effect(...)`) co-declared with
    `Impure` on one node has no coherent single reading: the two
    attributes express opposite intents (a bounded promise; a
    deliberate refusal to bound), and either silent resolution
    overrides one author's written declaration — the silent-rot shape
    the mechanics layer exists for (ADR-0050 §1, the
    `effect.unknown-label` precedent: a declaration that no longer
    means what it says is red on sight, undisableable, in every
    profile). While the contradiction stands, checking uses the
    tighter bound (fail-safe toward more checking; mechanics findings
    never suppress other checking). The existing `Pure` + `Effect(...)`
    quiet rule (spec: `Pure` wins, no diagnostic) is *retained*, and
    the asymmetry is the argument: `Pure` ⊂ `Effect(...)` is a
    redundancy inside one intent — bounding — with a coherent
    tightest-wins reading; bounded + ⊤ is not. The same id fires on a
    class-level `Pure` + `Impure` pair (point 12). No other new ids:
    a method exceeding its class-inherited default fires the ordinary
    `effect.envelope-exceeded`, with the message noting the envelope's
    class-level origin.

11. **Attributes-package impact: the first versioning event.**
    `Steins\Impure` is a new final, argumentless attribute class in
    `rigortype/steins-attributes` (MIT vocabulary, ADR-0025) —
    additive, therefore a minor version bump, and the package's first
    release event since the seed set. Core recognizes it exactly as
    `Pure`/`Effect`: fully-qualified, `use`-imported bare, and
    aliased spellings, case-folded. Version skew is graceful in both
    directions: a core predating the class sees an unrecognized
    attribute (= no envelope = unchecked — the lenient side), and a
    source file using `#[\Steins\Impure]` against an older installed
    package is inert at runtime (PHP autoloads attribute classes only
    on reflective instantiation); the core release notes state the
    minimum package version that makes IDEs and other tools resolve
    the class. Class-level *placement* of `Pure`/`Effect`/`Impure`
    (Part IV) requires the attribute classes' `#[Attribute]` targets
    to include `TARGET_CLASS` — part of the same package release.

## Part IV — class-level defaults

12. **A class-level envelope is the default for methods the class
    declares; the class attribute IS the opt-in.** `#[Pure]`,
    `#[Effect(...)]`, and `#[Impure]` are all admitted on class-likes
    (classes, abstract classes, interfaces, enums, traits — each
    qualified below), with one meaning: every method **declared by**
    that class-like receives the class envelope as its default;
    a per-method attribute overrides the default entirely (no
    merging — the nearer declaration wins, the standing
    tightest-is-explicit reading). This preserves the ADR-0006
    identity exactly: no annotation anywhere → nothing checked; one
    class-level attribute → the class's own methods opt in, one line
    for the DTO/value-object case instead of per-method spam
    (annotation restraint applied to the envelope dimension).
    `#[Effect(...)]` at class level is admitted deliberately, not
    just tolerated: `#[Effect('mutate.self')] class OrderDraft` is
    the observational-purity class shape (point 3) and the single
    most likely spelling the effect-splitting workflow produces.

13. **Boundary decisions, each argued:**

    - **Inherited methods keep their declaring class's posture.** A
      method inherited-not-redeclared carries the envelope (or
      absence) of the class that declares its body; the subclass's
      class-level attribute reaches only code the subclass owns.
      Enveloping other people's bodies from a distance would turn a
      one-line subclass annotation into findings inside a parent the
      author may not control — contract findings must sit on the
      declaration that made the promise.
    - **Trait-provided bodies are out — decided, not deferred.** A
      trait method is declared by the trait, so a using class's
      default never reaches it: the same "methods the class declares"
      rule, applied honestly, and it needs no trait flattening
      (which does not exist — ADR-0049 leaves trait-using regions
      Unknown for other oracles). Symmetrically, a class-level
      attribute **on a trait** applies to the methods the trait
      declares, wherever used. This is settled now rather than
      parked on the flattening deferral; flattening, when it lands,
      changes which bodies resolve, not who owns the envelope.
    - **Constructors: creation is not mutation.** A constructor's own
      writes to the `$this` it is initializing contribute **no**
      `mutate.self` — before construction completes there is no
      observably-mutated object, the same insight the language itself
      encodes (readonly properties are writable exactly there,
      ADR-0036's constructor-established discipline). So `#[Pure]
      class Money` with an ordinary initializing constructor is clean
      — precisely the value-object story. Everything else in a
      constructor colors normally: `echo`, I/O, writes to *other*
      objects (`mutate.instance`), static writes. Constructors
      otherwise receive the class default like any declared method,
      and a per-method attribute on the constructor overrides as
      usual.
    - **Magic methods are methods.** `__set`/`__get`/`__call` and
      friends receive the class default; a `__set` that writes under
      a `Pure` class default fires — that is what declaring the class
      pure means. No carve-out.
    - **Interfaces: admitted, and it is the PSR-pack shape.** A
      class-level `Pure` (or `Effect(...)`) on an interface is
      nothing but distribution of the already-designed per-method
      interface envelope (ADR-0005 envelope carriers): implementors
      are bound through the **existing** Liskov lane, callers typed
      against the interface assume the envelope. `#[Pure] interface
      Repository` is powerful, but the power is opt-in (someone wrote
      it on the abstraction they own), the findings are
      contract-layer (profile-gated, lenient default), and
      implementations may always be purer — an upper bound cannot
      trap a correct implementor. The alternative — excluding
      interfaces — would make the annotation a silent no-op, a
      written-declaration-does-nothing trap worse than the power.
      ADR-0045's synthetic PSR envelopes become expressible as
      exactly this shape.
    - **Class-level `Impure`: admitted, for a reason beyond
      symmetry.** Its checking semantics are near-inert (⊤ defaults
      fire nothing; overrides are admitted vacuously) — the
      meaningless-noise objection is real and conceded. It is
      admitted anyway because its value is declaration-side, at the
      granularity the modernization workflow actually works in: the
      effect-splitting of a god class produces
      `#[Effect('mutate.self')] class OrderData` **and** `#[Impure]
      class OrderGateway`, and the second annotation is the
      deliberate boundary marker — "looked; this is the effectful
      half" — feeding doctor's annotated-coverage accounting (point
      8) and any future no-unannotated-classes policy profile as the
      named escape hatch. Refusing it would leave the workflow's
      natural closing move unspellable while the per-method spelling
      exists.

14. **Folding: unchanged.** The fold license stays the empty effect
    set. A `mutate.self`-bearing method is not foldable — mutation is
    an effect, observational purity is not purity, and no part of
    this ADR widens the license. The constructor creation exemption
    (point 13) affects labeling, not folding: folding permission
    remains the hand-picked allowlist until the computed-purity slice
    that is explicitly not this ADR.

## Part V — layers, slices, measurement

15. **Layer placement is unchanged; the families hold.**
    `effect.envelope-exceeded` and `effect.liskov-widened` stay
    contract-layer (reached via `contracts` or a named profile);
    `effect.unknown-label` stays mechanics; the one new id
    (`effect.contradictory-envelope`, point 10) registers mechanics
    with its layer declared at registration per ADR-0050 §2. Nothing
    reaches the default surface that does not reach it today.

16. **Slice plan** (post-v0.1.0; each slice under the standard
    verification protocol — workspace tests, clippy `-Dwarnings`,
    fp-gate with byte-identical accounting where behavior is inert,
    conformance rerun with zero unexplained flips):

    - **E1 — vocabulary**: registry gains
      `mutate.arg`/`mutate.self`/`mutate.instance`/`mutate.static`
      (subsumption tests extended); `Steins\Impure` recognized (FQN /
      `use` / alias, the ⊤ envelope form in the envelope type);
      `effect.contradictory-envelope` registered and fired on the
      bounded+⊤ node pair; `rigortype/steins-attributes` minor
      release (Impure class + `TARGET_CLASS` on all three).
      Corpus-inert by construction (no inference change).
    - **E2 — inference**: `mutate.self`/`mutate.instance` labeled
      from the heap walk (allocation-identity decision, `$this`-alias
      tracking, constructor creation exemption); `mutate.static`
      structural origin; margins render the new labels; the automatic
      `Pure`-excludes-self-mutation consequence goes live. Gate
      expectation: effect contract tripwires stay vacuous (no
      envelopes exist in the wild); the slice ships the **margin
      measurement** — annotate over the legacy monorepo, counting (a)
      label prevalence, (b) mutate.self-only methods (the
      observational-purity inventory), recorded as a note like the G1
      measurement.
    - **E3 — class defaults**: class-level recognition on all
      class-likes; precedence (method over class), inherited/trait
      ownership rules, constructor/magic postures, interface
      distribution through the existing Liskov lane; doctor's
      envelope accounting counts class-level declarations;
      class-level contradiction wired to the E1 mechanics id.
    - **E4 — catalog coloring**: `mutate.arg` on the by-ref builtin
      set (`sort` family, `array_push`/`array_splice`, `shuffle`
      gaining it beside `nondet.random`, `array_walk`'s by-ref
      callback position). Pure catalog data; tripwire posture
      unchanged.

    E1 → E2 strictly ordered; E3 and E4 are order-free after E2.
    No slice waits on N5, and N5 waits on none of them (point 4).

17. **The corpus expectation, stated so the gate reads correctly.**
    Every finding this ADR enables is contract- or mechanics-layer.
    The corpus carries no envelope annotations, so the effect
    tripwires remain vacuous through all four slices — a gate that
    stays green here is *expected*, not evidence. The measurement
    instrument is `annotate`'s margins on the legacy monorepo (E2),
    the same posture as the G1 direct-vs-propagated measurement:
    inference lands first, envelopes arrive with adoption, and the
    margin counts are what the effect-splitting transform's business
    case is later argued from.

## Refusals (each anchored)

- **Mandatory effect annotation in any form** — unannotated stays
  unchecked; the ADR-0006 opt-in identity and the lenient-default
  principle (ADR-0050 amendment) are untouchable.
- **`Impure` as an unchecked opt-out** — the named-silences argument,
  point 9; three suppression channels are the whole surface
  (ADR-0023).
- **Declared-vs-declared envelope coherence as a core finding** — an
  `Impure`-declared, pure-bodied implementation under a `Pure`
  abstraction is silent; proven-only checking stands (ADR-0050 §11's
  declaration-coherence family remains at-most-a-policy-profile).
- **A `mutate` carve-out from folding relaxation** ("observational
  purity is almost pure, fold it") — the fold license is the empty
  set, full stop (point 14; ADR-0008's trustworthiness argument).
- **Syntax-keyed self-mutation** (`$this->` textually) — the heap's
  allocation identity is the decider; a syntax rule would both miss
  aliases and misfire on rebound variables (ADR-0036 exists precisely
  so this is not guessed).
- **`mutate.static` under `global.*`** — class-owned state is not the
  interpreter-global surface; a policy wanting both names both
  (point 4).

## Deferred-with-design

- **The static-property fact channel** — ADR-0052 N5, unchanged by
  this ADR; `mutate.static`'s origin sites are its future input.
- **`fopen`-style argument discrimination for `mutate.arg`** (e.g.
  `array_walk` callbacks that provably never write the by-ref
  param) — coarse coloring first, refinement on demand.
- **A no-unannotated-classes policy profile** consuming class-level
  envelopes and the `Impure` escape hatch — expressible once scoped
  policy (#15) lands; not designed here.
- **Margin rendering of ⊤** (`effects: ⊤` vs the inferred set on
  `Impure` declarations) — presentation, decided in E2's annotate
  work, recorded here as open.

## Open questions

- Should `annotate` (or a future `--fix` lane) *suggest*
  `#[Effect('mutate.self')]` verbatim on envelope-exceeded
  self-mutation findings, making the point-7 migration one keystroke?
- Does the constructor creation exemption need an escape-hatch edge
  when `$this` escapes mid-constructor and is written afterward
  (currently: still exempt, simplicity over a sliver of precision) —
  revisit with a triaged case in hand.
- Whether PSR packs (ADR-0045) should ship any class-level `Pure`
  interface envelopes in their first release, or per-method only.

## Amendment (2026-07-24): superglobal access as a structural effect origin

Owner-approved addition, from an empirical finding verified against
the release binary: superglobal reads are effect-invisible today —
`#[Pure] function f() { return $_GET['q'] ?? null; }` passes with
zero findings even on the contracts surface, while `getenv('PATH')`
correctly carries `global.read`. The inconsistency is one of
**mechanism**, not taxonomy: effect origins are catalogued calls plus
an enumerated list of language constructs, and variable-access
origins simply do not exist (an `effects_gaps.md`-unrecorded gap —
the same unenumerated-origin shape as point 6's property-write gap,
classified the same way: a design gap now closed, soundness never at
risk, silence being the safe side).

A1. **Superglobal access is a structural origin** — the same pattern
    point 4 established for `mutate.static`: decided by syntax
    position, no heap channel, reachability-blind like every origin.
    The coloring:

    - **Reads** of `$_GET`, `$_POST`, `$_SERVER`, `$_COOKIE`,
      `$_ENV`, `$_REQUEST`, `$_FILES`, `$argv`, `$argc` →
      `global.read`.
    - **Writes** to any superglobal (including `$_SESSION`) →
      `global.write`.
    - **`$_SESSION` reads → `global.read`**, decided. Session state
      is global mutable state — cross-request persistence changes
      nothing about the intra-request semantics an effect label
      describes, and `global.read` is the coherent floor: a function
      reading `$_SESSION` is world-coupled in exactly the way a
      `$_GET` reader is. The *bootstrap* is already handled — the
      catalog's `session_start` composite coloring
      (`io.fs.write` + `output.header` + `global.write`) stands
      unchanged; this row covers the subsequent array accesses that
      the composite cannot see.
    - **`$GLOBALS`** access joins the family (it is itself a
      superglobal): reads → `global.read`, writes → `global.write`.
    - **`global $x;` imports**: reads of an imported global →
      `global.read`, writes → `global.write` — the import statement
      is the syntactic marker; the same world-coupling through a
      different door.
    - Dynamic forms (`${'_GET'}`, variable-variables) contribute no
      label and taint exhaustiveness, per the standing dynamic-origin
      rule.

    The taxonomy needs **no new labels**: `global.read` and
    `global.write` exist and already carry `getenv`/`ini_set`. A
    finer request-coupling child (a hypothetical
    `global.read.request`) is **refused as premature**: no consumer
    exists (no policy or transform targets request-coupling today),
    and prefix subsumption makes the refinement additive and BC-free
    whenever one does — a declared `global.read` would admit it
    unchanged. This is the `fopen`-stays-at-`io.fs` precedent:
    coarse until discrimination is demanded. Revisit trigger: a
    policy profile wanting "no request coupling below the controller
    layer," argued with a case in hand.

A2. **Catalog coloring for the input-reading builtins**
    (owner-requested, the two named rows binding): `filter_input()`
    and `filter_input_array()` read `INPUT_GET`/`INPUT_POST`/
    `INPUT_SERVER`/`INPUT_ENV`/`INPUT_COOKIE` without touching the
    superglobals syntactically — both color `global.read`. Two
    adjacent candidates are colored with them rather than left open,
    on the identical-shape argument: `getopt()` reads
    `$argv`/invocation state, and `apache_request_headers()` (with
    its `getallheaders` alias) reads SAPI-provided request state —
    each is a function-shaped door to the same world-coupling A1
    catches at the variable-shaped door, and leaving them uncolored
    would recreate this amendment's inconsistency one row over. All
    are plain catalog data at the no-arg-analysis upper bound
    (`filter_input(INPUT_ENV, …)` vs `INPUT_GET` is not
    discriminated — same posture as `fopen`'s mode string).

A3. **The Pure consequence, restated.** With these origins, a
    Pure-claiming request-reading function finally fires
    `effect.envelope-exceeded` (contract layer; default surface
    unmoved, same three softeners as point 7): the owner's
    "implicit self-mutation must not pass as `Pure`" constraint,
    generalized — **implicit world-coupling must not pass as
    `Pure`**. The migration is the same margin-driven mechanic, and
    the honest envelope for a request reader is
    `#[Effect('global.read')]`.

A4. **Slice placement.** No new slice: A2's catalog rows fold into
    **E1** (vocabulary/catalog, corpus-inert until inference), A1's
    structural origins fold into **E2** (structural origins, beside
    `mutate.static`), and E2's monorepo margin measurement (point 16)
    now also counts superglobal-origin prevalence — on a 2007-era
    codebase the `global.read` margin is expected to be the loudest
    single instrument of the effect-splitting inventory.
