# The dump surface: a debug lane for requested introspection — dumpType family port, var_dump default-on

Owner-requested, in v0.1.0 scope (issue #17, decision log 2026-07-24):
port the `PHPStan\dumpType()` family, and make `var_dump()` report the
engine's inferred facts by default in `check`. A dump is **requested
introspection** — the user asks "what do you know here?" and the engine
answers. It is neither a proof (nothing breaks) nor a contract claim
(nothing is declared) nor mechanics (nothing rots if it is absent); it
is a position query (ADR-0048) whose answer is produced during the
batch walk and rendered by the same honesty machinery that renders
`annotate` margins and docblock promotions (ADR-0052 §4 / N1). This ADR
places the lane in the layer taxonomy, splits the exit posture between
the explicit and the incidental trigger, fixes the trigger semantics,
and binds the rendering to the one fact source.

1. **A fourth layer: `debug` — this ADR is the extension ADR-0050 §1
   requires.** The layer set {proof, contract, mechanics} is extensible
   by ADR only; `debug` joins it with the semantic identity "an
   answered question": the finding-shaped report exists *because a call
   site requested it*, and its content is a fact rendering, not a
   claim about the program. `Layer` (steins-infer `suppress.rs`) gains
   a `Debug` variant; every exhaustive match over `Layer` — profile
   surface computation, exit-level defaulting, the fp-gate partition,
   SARIF/JSON mapping — is thereby *forced* by the compiler to state
   its debug posture, which is exactly why the lane is a layer and not
   a boolean beside one. Ids enter `DIAGNOSTIC_REGISTRY` as
   `(id, Layer::Debug)` through the S1 pattern (ADR-0049): registered
   in the groundwork slice, listed in `REGISTERED_NOT_YET_EMITTED`
   until their emit slice lands, moved to `ALL_EMITTABLE_IDS` by that
   slice — the totality test's disjointness and count assertions bind
   each step, unchanged in shape.

2. **Three ids, mapped onto the three fact carriers (ADR-0052 §1).**
   Family `debug`, kebab-case `family.rule-name` (ADR-0022):

   - **`debug.type`** — explicit `PHPStan\dumpType($e)`: renders the
     walk's best knowledge of `$e` at the call position — the
     four-layer value fact (ADR-0035) where one is bound, else the
     heap's exact class / `Member` bounds for object holders
     (ADR-0036/0052), else the narrowed contract-fact arm list, else
     honest `unknown`. The precedence is the trust order (ADR-0037):
     proven value beats membership beats declared arms.
   - **`debug.phpdoc-type`** — explicit `PHPStan\dumpPhpDocType($e)`:
     renders the **contract-fact arm list** (the declared envelope as
     narrowed by guards, ADR-0052 §1's second carrier) — the
     declared-side view, mirroring PHPStan's native/phpdoc pair
     through Steins's carrier split; `no declared contract` when the
     carrier is empty, never a synthesized type.
   - **`debug.var-dump`** — a call resolving to the global
     `var_dump()`: one `debug.type`-shaped report **per argument
     expression**, same rendering, same fact source. Zero-argument
     `var_dump()` emits nothing in this lane (arity is S5's business,
     not a dump).

   Every rendering carries the stratum honestly: an Asserted-stratum
   fact (ADR-0052 §5) renders with an explicit `(asserted)` marker — a
   dump that prints a docblock claim indistinguishably from a proven
   value would launder trust through the introspection surface, the
   exact laundering the derivation clause exists to prevent.

3. **Exit split: explicit dumps fail, var_dump dumps are structurally
   exit-neutral.** The two triggers differ in *authorship of the
   question*, and the posture follows authorship:

   - **`debug.type` / `debug.phpdoc-type`: level fail, fixed.**
     PHPStan's remove-before-commit posture, adopted — and in Steins it
     is not even a policy choice: `PHPStan\dumpType()` names a function
     that **does not exist at runtime**, so a committed call is a
     guaranteed fatal `Error` on any live path. Failing CI on it is
     the zero-FP identity agreeing with the compat precedent, not a
     lint judgment.
   - **`debug.var-dump`: level warn, fixed — exit-neutral forever.**
     Measured (2026-07-24): the legacy monorepo carries ~63 live
     first-party `var_dump` sites (81 textual, 18 commented out);
     public corpus: Carbon 43 (docs/test examples), phpunit 2,
     monolog/guzzle/symfony-process 1 each, phpstan-src `src/` 0. A
     leftover `var_dump` is legal, working PHP; a default `check` that
     goes red on ~63 pre-existing lines is the crying-wolf prohibition
     inverted on day one (ADR-0002/0007), so exit-neutrality is forced
     by the quiet-default identity, not preferred. Structurally
     enforced: the id is born at warn and ADR-0050 §7's profile
     machinery only *demotes* — no channel exists that could promote
     it to fail. Teams that want "no var_dump in CI" are asking for a
     lint rule, which Steins refuses categorically (ADR-0017).

   ~63 noticeable-not-catastrophic report lines on a monorepo default
   run is the accepted cost of the owner's default-ON decision; the
   named opt-out (point 4) is the relief valve, and the lines
   themselves are answers, each pointing at a deletable call.

4. **Profile and suppression matrix — asked-for answers cannot be
   muted, incidental ones can be declined.**

   - **`debug.type` / `debug.phpdoc-type` are profile-inert** (like
     mechanics, ADR-0050 §1/§5): no profile disables or demotes them.
     Writing the call is unambiguous first-person intent *in the
     code*; a profile that hid the answer would yield
     silence-then-runtime-fatal — the rot shape mechanics-exemption
     exists to prevent.
   - **`debug.var-dump` is profile-disableable** — dump-on-var_dump is
     a default *service*, not anti-rot; a team drowning in legacy
     sites may turn it off in a named profile (`disable =
     ["debug.var-dump"]`). The built-in `default` profile keeps it ON,
     so the named opt-out coexists with the owner's default without
     contradiction: defaults are what a bare run does, profiles are
     deliberate (ADR-0050 §5).
   - **All three ids are exempt from all three suppression channels**
     (ADR-0023): never written by `--set-baseline`, never matched by a
     baseline entry, never matched by `@steins-ignore` or `[[policy]]`
     disable. A suppressed dump is a contradiction — the question is
     in the source; the remedy is deleting the call, one keystroke
     away. An inline ignore naming a debug id therefore reports
     `suppress.unmatched`, the anti-rot channel doing its normal job.
   - `--format json` carries `layer: "debug"` and the level field with
     no new machinery (ADR-0050 §2/§7 additive fields).

5. **Trigger semantics — resolution decides, and the two triggers
   resolve differently on purpose.**

   - **The `PHPStan\` pair is reserved vocabulary, recognized
     unconditionally by resolved FQN.** `\PHPStan\dumpType(...)`,
     `use function PHPStan\dumpType; dumpType(...)`, and any
     resolution path reaching the FQN trigger recognition; the
     `PHPStan\` namespace is the compat vocabulary surface (ADR-0029)
     and Steins treats these two names as reserved in it. A userland
     *definition* of `PHPStan\dumpType` does not stand recognition
     down — definition-sensitive recognition would make the dump
     surface depend on vendor contents and resolution completeness,
     violating determinism for zero legitimate use.
   - **`var_dump` triggers only when the call resolves to the global
     function under PHP's own fallback rule.** Enumerated: (a)
     `\var_dump($e)` — always; (b) unqualified `var_dump($e)` in the
     root namespace — always; (c) unqualified in `namespace Foo;` —
     only if `Foo\var_dump` is provably undefined (the runtime would
     fall back to global); if a same-namespace homonym exists, or its
     existence is Unknown (conditional definition, dam), **no dump** —
     a missing dump is a missed service, never an FP, so silence is
     the free safe side; (d) `Foo\var_dump($e)` qualified, or
     `use function Foo\var_dump;` — never (resolves elsewhere); (e) a
     *method* named `var_dump` (`$o->var_dump()`, `static::var_dump()`)
     — never (different symbol space); (f) first-class callables
     (`var_dump(...)`) and string callables (`array_map('var_dump',…)`)
     — never: no argument expression exists at the site to dump.
     Dynamic calls (`$f($e)`) never trigger. The homonym-existence
     question consumes the same project-index/sidecar existence
     surface as ADR-0049 S1 — a query answer, replay-safe.

6. **Runtime semantics untouched; one deliberate interaction with the
   absence family.** `var_dump`'s effect coloring (output effect,
   ADR-0008 catalog) is unchanged — dump reporting is analysis-side
   only, and the call analyzes exactly as before plus one report. A
   recognized `PHPStan\dumpType` call likewise keeps its conservative
   unresolved-call treatment for effect and invalidation purposes
   (recognition must not launder an undefined call into a benign one —
   it is going to be deleted anyway). One carve-out, recorded here
   because ADR-0049 §3 would otherwise double-report: a recognized
   dump-family FQN is **excluded from `call.undefined-function`**
   (S4) — the fail-level `debug.type` already reds CI at that site
   with a message that says what to do; two findings for one deletable
   line is noise. The S4 emitter consults the same recognizer, so the
   exclusion cannot drift.

7. **Rendering: one fact source, one spelling, annotate parity as a
   pinned test.** The dump is answered **during the batch walk** at
   the call's position — the same `analyze_scope` thread that pushes
   `LineFact` values for `annotate` (ADR-0048 §1's demonstrated
   mid-walk surfacing); no second inference pass, no retained tables
   (replay over retention). Spelling: the fact renders through the N1
   normalizer (`steins_contract::normalize` — `summarize_vals`,
   `dedup_arms`, the precision ladder) plus a **shared plain-text arm
   spelling that moves into steins-contract**, consumed by both the
   dump emitter (steins-infer) and `annotate`; steins-edit keeps only
   its docblock armor (literal-safety escaping, CAP-bounded spelling
   decisions — ADR-0052 §4's "stays" list, unchanged in scope, since
   `*/`-safety is meaningless in terminal output). This is forced by
   the dependency direction — steins-edit depends on steins-infer, so
   the emitter cannot call the renderer where it lives today — and it
   is the ADR-0052 extraction continued one step, not a new layer.
   Binding: **a dump's rendered fact and `annotate`'s margin fact for
   the same expression at the same position are byte-equal** (the
   parity test); `Unknown` renders as honest incompleteness
   (`unknown`, with annotate's `…?` discipline for non-exhaustive
   sets), never a guess and never a `mixed` pretense. Message frame
   wording around the rendered fact ("dumped type: …") is not a
   contract (ADR-0023) and may improve; the rendered fact itself is
   pinned by the parity and fixture tests. Multi-argument calls: one
   report per argument, argument order, for both `var_dump` and (as a
   forgiving generalization) a multi-arg `dumpType`; a zero-argument
   `dumpType()` still reports — fail-level, message says the call
   dumps nothing — because the committed call remains a runtime fatal
   either way.

8. **fp-gate: dumps are not findings and the gate proves it.** The
   gate's partition predicate is `layer(id)` (ADR-0050 §9);
   `Layer::Debug` maps to **excluded from every counter** — not
   red-on-sight, not a tripwire, not `EXPECTED_PROOF_FINDINGS`
   material. Without the exclusion the pinned corpus alone injects ~48
   dump lines (Carbon's 43 foremost) and the legacy monorepo ~63 —
   flooding the exact measurement the gate exists to keep clean. The
   convergence assertion: the gate run with the dump lane emitting is
   **byte-identical in all counted output** to the recorded pre-dump
   run — the exclusion is asserted, not assumed — and a debug-layer id
   appearing in any gate counter is itself a gate failure (the
   partition match arm is exhaustive, so a future fourth-layer id
   cannot silently fall into a counting bucket).

9. **`PHPStan\Testing\assertType` is its own slice — the consumption
   point stays reachable, by construction.** The oracle-B synergy
   (phpstan-src's `tests/PHPStan/Analyser/nsrt/`, ~1,600 files, as the
   narrowing slices' acceptance instrument) is real and wanted, but
   assertType is a different machine: it *compares* a rendered fact
   against an expected **type string in PHPStan's renderer spelling**,
   which imports a spelling-compat contract (PHPStan's exact `array<…>`
   / `non-empty-string` / constant-literal spellings) that the honesty
   renderer deliberately does not promise, plus a harness runner and a
   pass/fail accounting that belong beside the conformance apparatus
   (ADR-0013/0026), not in the check pipeline. Coupling it here would
   force those spelling decisions prematurely and bloat every dump
   slice's gate. What this ADR binds so the door stays open: the
   trigger is a **name-keyed recognizer over resolved FQNs in call
   position**, feeding the walk's fact-at-position answer to a
   per-name consumer (dumpType → report; assertType → comparator
   later) — the assertType slice plugs a comparator into the same
   recognizer and touches no walk internals. Deferred-with-design to
   its own issue.

10. **ADR-0048 compliance, explicit.** §2 replayability: a dump is a
    deterministic function of (scope CST, entry state, query answers,
    fold memo) — the identical inputs that produce the walk's facts;
    the recognizer consults resolution and the existence surface, both
    query answers; re-running the scope's walk reproduces the dump
    byte-for-byte. §3 entry states: dumps contribute **nothing** —
    they read facts, never bind them (a dump call must not perturb the
    env, the heap, or any carrier; the fixture matrix includes a
    dump-is-transparent case: facts before and after the call are
    identical). §4 ordering: dump output ordering is positional within
    a file and presentational across files, like every diagnostic.

11. **Slices — Opus-sized, standard verification protocol (workspace
    tests grow, clippy 0, release build, foreground gate, conformance
    when semantics move), sequenced against the in-flight N2/S2
    pipeline.** The emit point lives in the call-handling arm of the
    scope walk (`crates/steins-infer/src/lib.rs`, `analyze_scope`) —
    the same region the absence emitters land in, so the dump emit
    slices sequence **after the in-flight S2 merge** (rebase hygiene,
    not a semantic dependency) and after N2 (the stratum bit is a
    rendering input for the `(asserted)` marker):

    - **D1 — lane groundwork, zero behavior**: `Layer::Debug` variant;
      three ids registered `(id, Layer::Debug)` and listed in
      `REGISTERED_NOT_YET_EMITTED` (count assertion 9 → 12); every
      exhaustive `Layer` match extended — profile surface (explicit
      pair inert, `debug.var-dump` disableable), suppression
      exemptions, exit-level defaults (fail / fail / warn), fp-gate
      partition arm (excluded + byte-identity assertion), JSON layer
      string. Gate byte-identical. No file-level collision with S-
      or N-slices beyond the registry pair every slice already touches.
    - **D2 — shared spelling + annotate rewire**: the plain-text arm
      spelling moves into steins-contract beside `normalize`;
      `annotate`'s value/exact-class margin rendering rewired to it;
      steins-edit's docblock armor re-layered on top with the existing
      honesty tests asserting **byte-identical docblock output** (the
      N1 discipline repeated). Any deliberate annotate-margin spelling
      change is reviewed against recorded output in the PR.
    - **D3 — the explicit pair emits**: the name-keyed recognizer;
      `debug.type`/`debug.phpdoc-type` move to `ALL_EMITTABLE_IDS`;
      fail-level wiring; the S4 carve-out (point 6) recorded where the
      S4 emitter lives (or as a pinned fixture if S4 has not landed);
      fixture matrix — Singleton/OneOf/Refined/General, exact-class,
      `Member` bounds, contract arms, `unknown`, `(asserted)` marker,
      dump-transparency, zero-arg, multi-arg; the annotate **parity
      test** pins byte-equality at shared positions. Conformance rerun.
    - **D4 — `var_dump` default-on**: the resolution-sensitive trigger
      with all six legs of point 5 as fixtures (incl. the
      namespaced-homonym silence and the Unknown-existence silence);
      per-argument reports; warn-level exit-neutral wiring; profile
      opt-out test; corpus measurement run with the ~63-line monorepo
      surface and the Carbon-43 public surface triaged against the
      point-3 judgment; fp-gate byte-identity re-asserted with the
      lane live.

12. **Refusals** (each one line, each anchored):
    - **A boolean setting instead of a layer** — every `Layer` match
      would silently default the dump posture; the compiler-forced
      statement of point 1 is the design (ADR-0050 §1).
    - **Fail-on-var_dump, or any promote-to-fail channel for it** —
      that is a lint rule; Steins ships no lint (ADR-0017), and the
      quiet default is identity, not preference (ADR-0002/0007).
    - **Suppressing dumps via baseline/ignore/policy** — a muted
      answer to an asked question is rot; the remedy is deleting the
      call (ADR-0023's channels exist for findings, and a dump is not
      one).
    - **A Steins-native alias (`Steins\dumpType`)** — two spellings of
      one introspection call is vocabulary sprawl; the compat spelling
      *is* the vocabulary (ADR-0029); revisit only with a Steins-only
      capability the PHPStan spelling cannot name.
    - **Definition-sensitive recognition for the `PHPStan\` pair** —
      recognition that varies with vendor contents is nondeterminism
      for zero use; the namespace is reserved compat vocabulary
      (ADR-0029, point 5).
    - **Dumping on `print_r`/`var_export`/`dd`/`dump`** — the owner
      asked for `var_dump`; framework dumpers are plugin territory
      (ADR-0012), and each added trigger multiplies the point-5
      resolution matrix.
    - **PHPStan-spelling type strings in dump output** — the honesty
      renderer's spelling is the one spelling (ADR-0052 §4); PHPStan
      spelling-compat is assertType's problem, quarantined there
      (point 9).
    - **A second inference pass or retained type tables to answer
      dumps** — replay over retention is settled (ADR-0048 §1).

13. **Deferred-with-design**: the assertType harness slice (point 9 —
    recognizer consumer + comparator + nsrt runner beside the
    conformance apparatus, own issue); `annotate` optionally marking
    recognized dump sites in the margin (pure presentation, rides D3's
    machinery whenever wanted); a `doctor` count of live dump sites as
    a coverage-posture line (M2, with `doctor` itself, ADR-0050 §11);
    SARIF mapping for the debug layer if code-scanning ingestion ever
    wants dumps at all (default: omitted from SARIF — dumps are for
    humans at terminals, and the M2 SARIF slice inherits the
    exhaustive-match obligation either way).
