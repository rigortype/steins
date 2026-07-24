# Diagnostic layers and profiles: registry-carried layers, proof-only default, named surfaces

A default `steins check` today prints contract-layer findings (`phpdoc.*`,
`throw.*`) beside proof-layer findings with no separation — on the legacy
monorepo that is ~44k `throw.undeclared` lines in a default run, the
single largest adoption blocker in the binary. The layer distinction
exists, but only inside the fp-gate (xtask's measurement-mode
partitioning); the user-facing binary has never heard of it. The identity
is the zero-FP banner: **what a bare `check` prints must be exactly the
set held to the proven-runtime-break bar** — contract findings are true,
but they are debt reporting, and debt reporting is opt-in (the crying-wolf
prohibition, ADR-0002/0007). This ADR makes the layer a first-class
registry attribute and the printed surface a named, deliberate choice.

1. **Three layers, and layer is semantic identity — not severity.** A
   diagnostic's layer names *what kind of claim it makes*, mirroring
   ADR-0030's two acceptance relations:

   - **proof** — runtime survivability: the program provably breaks on a
     live path. Held to the zero-FP bar; gates red on sight (ADR-0013).
     Today: `type.argument-mismatch`, `type.return-mismatch`,
     `type.property-mismatch`, `call.on-null`, `readonly.reassigned`.
   - **contract** — declared-contract acceptance: a proven behavior
     violates something the code *declares* about itself; the program
     works. True findings legitimately abound in released code. Today:
     `phpdoc.param-mismatch`, `phpdoc.return-mismatch`,
     `phpdoc.property-mismatch`, `throw.undeclared`,
     `throw.liskov-widened`, `effect.envelope-exceeded`,
     `effect.liskov-widened`.
   - **mechanics** — the apparatus's own hygiene: findings whose absence
     would silently rot another channel. Today: `suppress.unmatched`,
     `suppress.unknown-id`, `effect.unknown-label` (a typo'd label in a
     written envelope silently disables its checking — exactly the rot
     this layer exists to surface). Mechanics ids print in **every**
     profile and, like the `suppress.*` pair already, are exempt from the
     suppression channels; opt-in mechanics ids (a future
     `suppress.missing-reason`, ADR-0023) are born disabled and enabled
     via scoped policy, not via profiles.

   ADR-0002's proof/policy split maps cleanly: proof = proof; the policy
   umbrella today contains exactly the contract layer. The value set
   {proof, contract, mechanics} is extensible **by ADR only** — the
   future `boundary.*` family (ADR-0042) registers its layer when it
   lands, and the profile machinery below keys on layers generically, so
   no decision here forecloses it.

2. **The id registry carries the attribute.** `DIAGNOSTIC_IDS`
   (steins-infer's ADR-0022 registry) becomes a table of `(id, layer)`
   entries with a `layer(id)` lookup; a workspace test asserts totality
   (every emitted id is registered *with* a layer — registering an id
   without one does not compile). The invariant is binding on every
   registration channel: **a new id MUST declare its layer**, and the
   plugin registration channel (ADR-0022/0039) requires it at
   registration time — a plugin registering a proof-layer id thereby
   accepts the zero-FP bar and fp-gate applicability. The attribute is
   **per-id, not per-family**: families are layer-homogeneous today as a
   fact, but ADR-0022 already promises that a rule moving between layers
   is not a BC break, so prefix spellings (`throw.*`) remain a config
   convenience, never the semantic carrier. `--format json` gains a
   `layer` field per finding (additive); the M2 `sarif` format maps
   proof→`error` and warn-level contract→`warning` naturally.

3. **Default surface: proof + mechanics.** A bare `steins check` prints
   proof-layer findings (the banner set) plus mechanics (anti-rot must
   always bite). Contract-layer families are reached through profiles,
   intentionally. One family is carved out: `throw.undeclared`'s default
   posture is **user gate G1** — this ADR supplies the mechanism such
   that every G1 outcome is a one-line change to the built-in default
   profile's definition, and it deliberately does not decide the gate.

4. **G1 as configuration, not architecture.** The built-in `default`
   profile is data (an id/layer selection), so the three G1 outcomes are
   three definitions of it:

   - **Keep-on**: `default` = proof + mechanics + `throw.*`. Preserves
     the "a written `@throws` is a deliberate opt-in" principle
     (ADR-0040 §5: unannotated code is never envelope-checked). Cost:
     ~44k findings in the monorepo's default run; the printed set stops
     being the zero-FP runtime-break set, which muddies the banner; a
     first-touch baseline absorbs it but a 44k-entry baseline is its own
     deterrent.
   - **Demote**: `throw.*` lives only in the `contracts` profile.
     Default runs are pure proof; envelope checking is deliberate. Cost:
     a user who writes `@throws` expecting checking gets silence —
     mitigated by a `doctor` notice ("N written envelopes are unchecked
     under the active profile"), deferred with `doctor` itself (M2).
   - **Split**: `throw.undeclared` stays in `default` for a high-signal
     subset and demotes the rest to `contracts`. The check already fires
     only on declarations carrying a locally-written `@throws`
     (verified: `throw_diagnostics` reads the declaration's own
     docblock), and findings are reported at the *origin* throw site —
     so one annotated controller over pervasive base exceptions yields
     hundreds of origin findings (ADR-0040/0007; whence the 44k). The
     crisp split axes are therefore (i) **direct-vs-propagated**: default
     reports only escapes thrown in the annotated declaration's own
     body, propagated escapes demote; or (ii) **origin locality**:
     escapes originating outside the annotated declaration's file/region
     demote. Mechanism: the emitter records a registry-declared **facet**
     on the finding (`origin = direct|propagated`), and profile entries
     may select on facets — the only facet v1 defines, added only if G1
     chooses split.

   The trade-off summary exists to inform the gate; the gate's outcome
   is recorded in this ADR's amendment history when the user decides.

5. **Profiles: named surfaces, both flag and config.** A profile is a
   named selection over layers and ids. Built-ins in v1: `default`
   (point 3/G1) and `contracts` (= default surface + the whole contract
   layer). `strict` and `boundary` are **reserved names**,
   deferred-with-design to ADR-0042's boundary-profile landing — v1
   ships two, not a ladder pretending to be names. Selection:
   `steins check --profile contracts` or `[check] profile = "contracts"`
   in steins.toml; the flag wins (invocation intent beats repo default).
   User-defined profiles compose in config, never on the command line
   (config carries intent, ADR-0023; ad-hoc `--enable id,id` flags are
   rejected — an unnamed surface is unreviewable in CI history):

   ```toml
   [check]
   profile = "migration"

   [profile.migration]
   extends = "contracts"
   warn    = ["throw.*"]      # surfaced and reported, exit-neutral (point 7)

   [[policy]]
   enable  = ["phpdoc.*"]
   in      = "@domain"
   reason  = "the domain layer keeps honest docblocks"
   ```

   `extends` names a built-in or user profile; `enable`/`disable` take
   ADR-0022 prefix id-arrays; cycles and unknown names are config
   errors. Mechanics ids ignore profile `disable` (point 1).

6. **Composition order with scoped policy.** The pipeline is: vendor
   filter (ADR-0015) → **profile surface** → **`[[policy]]` scoped
   enable/disable** (ADR-0023, which gains `enable` alongside `disable`
   for exactly the per-path contract-checking case above) → inline
   ignores → baseline. Surface first: a finding outside the active
   surface does not exist and never consumes — nor rots — a later
   channel. Scoped policy refines the surface structurally within paths;
   later `[[policy]]` entries win on overlap (declaration order is the
   precedence, no specificity metric to reverse-engineer). A
   consequence accepted deliberately: a `[[policy]]` disable makes
   previously-baselined findings in its scope go stale — correct
   anti-rot, prompting a baseline refresh, not a bug.

7. **Exit codes: surfaced means fail, warn is the opt-out.** Every
   surfaced finding carries a level, `fail` by default in every layer —
   if a profile put it on the surface, it was asked for, and CI must see
   it (a surfaced-but-harmless default would be the crying-wolf
   prohibition inverted: asked-for findings ignored). A profile's
   `warn = [...]` demotes matching ids to report-without-fail. Exit
   semantics: `0` when nothing fail-level is displayed (warn-only runs
   exit 0 — that is what warn means), `1` when any fail-level finding is
   displayed, `2` for usage/config errors — unchanged from today except
   the level distinction. Mechanics ids default to fail like everything
   else (an unmatched ignore rots unless CI bites). `--format json`
   gains a `level` field.

8. **Baseline: capture-surface header, dormant is not stale.** Entries
   stay `{"id","path","hash"}` with ids verbatim (ADR-0022), so a
   baseline entry can only ever suppress a finding of its own id — the
   cross-layer silent-suppression hazard is structurally absent at the
   entry level. Two additions close the surface-level holes: (a) the
   JSONL header records the **capture surface** (`"profile"` name and
   the resolved id set) written by `--set-baseline` under the active
   profile; (b) staleness is computed **only over ids inside the current
   run's surface** — an entry whose id is outside it is *dormant*: kept,
   not reported stale, not pruned. A run whose active surface exceeds
   the captured one prints a one-line notice ("contract layer active but
   baseline captured without it — those findings are unbaselined") so a
   `--profile contracts` run over a default-captured baseline drowns
   loudly, not silently.

9. **The fp-gate converges on the registry, keeping its semantics.**
   Measurement mode is a *gate* concept (increase tripwires,
   `EXPECTED_PROOF_FINDINGS`) and stays in xtask; what changes is only
   the partitioning predicate: `is_phpdoc`/`is_throw` string-prefix
   checks are replaced by `layer(id)` — proof gates red on sight,
   contract gates as per-package increase tripwires, mechanics gates red
   on sight (a mechanics finding on corpus code means apparatus rot).
   One recorded delta: `effect.envelope-exceeded`/`effect.liskov-widened`
   move from red-on-sight to contract tripwires, matching their
   declared-contract semantics — vacuous on the corpus today (no
   ADR-0006 envelope annotations exist in the wild) and correct the day
   they are not. Gate output is otherwise byte-identical; the
   convergence commit asserts it against a recorded run.

10. **Refusals** (each one line, each anchored):
    - **Numeric severity levels** — profiles are named intent, not a
      ladder; PHPStan's 0–9 makes "what will be reported" opaque
      (ADR-0002/0020, roadmap Won't-build).
    - **A severity dimension beyond layer + fail/warn** — the identity
      is binary proof with layered surfaces; "hint/info/notice" grades
      re-import the ladder through the side door.
    - **Trust toggles** (`treatPhpDocTypesAsCertain`-style) — the trust
      order is fixed (ADR-0037); profiles select *surfaces*, never
      inference behavior.
    - **Message-regex selection in profiles or policy** — wording is not
      a contract (ADR-0023).
    - **Per-finding config entries** — the three suppression channels
      are the whole surface (ADR-0023); profiles must not become
      `ignoreErrors` sprawl with a new name.
    - **Ad-hoc CLI id lists** (`--enable`/`--disable` flags) — unnamed
      surfaces are unreviewable; name it in config or use a built-in.

11. **Deferred-with-design**: `strict`/`boundary` profile contents and
    the `failure.*`-label consumption rules (ADR-0042); a
    declaration-coherence family as a contract-layer profile entry
    (ADR-0030 silences §2 — "at most a policy profile, never core");
    generalizing facet selectors beyond the single G1-split facet;
    `doctor` reporting written-but-unchecked envelopes and the active
    surface (M2); per-layer SARIF level overrides if code-scanning
    ingestion demands them.

## Amendment (2026-07-24): G1 decided — demote, with named-stage opt-up

The owner resolved gate G1, and generalized it into a standing
principle recorded here because this ADR is where surfaces live:

**The lenient-default principle (owner, 2026-07-24): defaults are
lenient; strictness is opt-in, expressed as named stages a project
declares in config.** A project's appetite for debt reporting is not
uniform — it tracks the project's modernization stage — so the tool
never guesses it; the repo declares it, reviewably.

1. **G1 outcome: demote.** The built-in `default` profile is
   proof + mechanics (point 3's shape, now unconditional). `throw.*`
   lives in the `contracts` profile. The "wrote `@throws`, got
   silence" gap is doctor's written-but-unchecked-envelopes notice
   (point 11), unchanged.
2. **Named stages, never a ladder.** The graduated-strictness wish is
   served by *named* profiles a project selects in `[check] profile`
   and ratchets through with the baseline round-trip (§8) — the
   numeric-level refusal (point 10) stands untouched: stages have
   names and definitions, not numbers.
3. **A possible middle stage is measurement-gated.** Whether a
   `throw.undeclared` direct-origin subset deserves a named built-in
   stage (the point-4 split, needing the origin facet) is decided by
   the pending direct-vs-propagated measurement over the legacy
   corpus, not by taste. Until then v1 ships `default` and
   `contracts` exactly as point 5 states.
4. **Two carve-outs, restated as binding.** (a) Mechanics ids print in
   every profile (anti-rot is not a strictness preference). (b) The
   `[runtime]` pseudo-constants (`zend-assertions`,
   `warning-handler`, `include-path`, `sapi`) are declarations of
   boot truth (ADR-0037 §2, ask-the-real-thing), NOT strictness
   knobs — the lenient-default principle governs *reporting
   surfaces* and never reopens the trust-toggle refusal (point 10).
5. **Middle stage decided by measurement — v1 ships THREE built-ins**
   (amending point 5's "two, not a ladder": still not a ladder, now
   three names). The direct-vs-propagated measurement
   (`docs/notes/20260724-g1-throw-origin-measurement.md`: monorepo
   43,963 = 158 direct across 134 declarations + 43,805 propagated,
   99.5%) justified the middle stage, so `throws-direct` (= default +
   `throw.undeclared` where `origin = direct`) ships beside `default`
   and `contracts`, and point 4's origin facet is productionized from
   the measurement's classification rule (origin-file identity plus
   own-body origin-offset membership). Facet selection in USER
   profiles stays deferred-with-design (point 11): v1 reaches the
   facet only through the built-in — a facet-shaped token in a user
   profile's id arrays is an unknown-pattern config error, no TOML
   syntax invented ahead of its design.
6. **The conformance-harness consequence, recorded.** The
   php-typing-conformance adapter invokes a bare `check`, which now
   reads the default surface: contract-layer expectations are hidden
   (measured: 81/98 default-surface vs 87/98 contracts-surface, zero
   proof-layer movement; one intended-divergence case,
   `regressions_reversed_literal_list_param`, now PASSES under
   default — the demote silencing an over-report). Capability
   measurement should read the full surface; the adapter-side
   `--profile contracts` (or equivalent) is the conformance repo
   owner's call (gate G4's boundary), recorded here so the 81-vs-87
   split is never mistaken for a checker regression.
