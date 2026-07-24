# CI surface: four renderings of one surface, and the doctor posture report

ADR-0020 declared `sarif`/`github` output and `doctor` from the start;
issues #14/#16 make them M2 work. The commitments are scattered —
ADR-0050 §2's sarif mapping note, ADR-0053 §13's deferred SARIF
carriage of the debug layer, the G1 amendment's
written-but-unchecked-envelope notice, ADR-0004's coverage-posture
promise, ADR-0049 A6/A9/A11's posture surfaces, ADR-0050 §8's baseline
capture header — and this ADR consolidates them into one implementable
design. Two identities govern everything below. **A format is a
serialization of the displayed surface, never a second surface**: the
pipeline (vendor → profile → policy → inline ignores → baseline,
ADR-0050 §6) decides what exists; a format decides only how it is
spelled. **Doctor asks what the world is; check asks what is wrong**:
doctor reads configuration, environment, and index-level facts and
never runs a diagnostic emitter — it is the posture mirror ADR-0004
promised, not a gate (ADR-0013's apparatus is repo-side verification;
doctor is user-side adoption readiness).

## Part I — `sarif` and `github` (issue #14)

1. **Format invariance is the binding invariant.** For a fixed
   invocation, all four formats (`text|json|sarif|github`) render the
   same displayed finding multiset and produce the same exit code
   (ADR-0050 §7 unchanged: 1 iff any fail-level finding displays, 0
   otherwise, 2 for usage/config errors). Nothing format-specific
   reopens a suppression channel (a baselined finding does not
   reappear as a SARIF "suppressed result" — point 7) and nothing
   format-specific drops a displayed finding (no annotation cap —
   point 5). A workspace test asserts the invariance: identical
   `(id, path, line, column)` multiset and identical exit across the
   four formats over a fixture project. This is the same soundness
   posture as the fp-gate's partition discipline (ADR-0050 §9): the
   measured set has one definition, carried everywhere.

2. **SARIF 2.1.0, minimal committed schema.** One `run`, `version:
   "2.1.0"`, the standard `$schema` URI. Committed shape:

   - `tool.driver`: `name: "steins"`, `semanticVersion` from
     `CARGO_PKG_VERSION`, `informationUri` the repo URL.
   - `tool.driver.rules`: one `reportingDescriptor` **per id present
     in the displayed results**, deduped and sorted — not the full
     registry, not the surface's capture set. The full surface set
     already has exactly one carrier (the baseline capture header,
     ADR-0050 §8); duplicating it into every SARIF invites divergence,
     and code-scanning ingestion needs only the referenced rules. Each
     rule: `id` = the ADR-0022 `family.rule-name` verbatim,
     `shortDescription.text` = the id (the registry carries no prose
     descriptions today; enrichment plus `helpUri` is deferred to a
     docs site), `properties.layer` = the registry layer string
     (`"proof"|"contract"|"mechanics"|"debug"` — ADR-0050 §2's
     promise), `defaultConfiguration.level` per the point-3 mapping.
   - `results[]`: `ruleId`, `ruleIndex`, `level` (point 3),
     `message.text` = the finding message verbatim (message wording is
     not a contract, ADR-0023 — the id is), one
     `locations[0].physicalLocation` with
     `artifactLocation.uri` = the diagnostic path with forward
     slashes and `region.startLine`/`startColumn` = the same 1-based
     numbers `text`/`json` print (`columnKind` is left at the SARIF
     default; if ingestion ever renders a divergence it gets a
     recorded fix, not a preemptive guess). Registry-declared facets
     ride `properties` (`"origin": "direct"`), mirroring `json`.
   - `partialFingerprints`: `"steinsFindingHash/v1"` = the ADR-0022
     baseline hash of the flagged line's neighborhood. The hash exists
     precisely so identity survives unrelated edits; handing it to
     code scanning gives alert-tracking the same stability the
     baseline already has — one identity function, two consumers,
     zero new machinery.
   - `run.automationDetails.id` = `"steins/{profile}"` — parallel
     uploads under different profiles (a `default` gate beside a
     `contracts` debt dashboard) must not clobber each other's alert
     categories.
   - `run.properties`: `profile`, `vendorSuppressed`, `suppressed`,
     `baselined` — the same accounting envelope `json` carries;
     counts only, never entries (point 7).

   Path contract, stated honestly: paths pass through as given
   (relative stays relative, absolute stays absolute). GitHub upload
   wants repo-root-relative paths, so the documented CI idiom is
   "invoke from the repo root with relative paths"; Steins does not
   guess a repo root it was not shown. Output goes to stdout like
   every format; no `--output` flag in v1 (redirection is the shell's
   job).

3. **The level mapping keys on ADR-0050 §7's level, with one
   debug-layer carve-out.** Layer is semantic identity, not severity
   (ADR-0050 §1), so layer never carries the SARIF level; the exit
   level does, because SARIF `level` and the exit code answer the
   same question ("does CI act on this?") and must not disagree:

   | layer | level (§7) | SARIF `level` | github command |
   |---|---|---|---|
   | proof / contract / mechanics | fail | `error` | `::error` |
   | proof / contract / mechanics | warn | `warning` | `::warning` |
   | debug — `debug.type` / `debug.phpdoc-type` | fail (fixed) | `error` | `::error` |
   | debug — `debug.var-dump` | warn (fixed) | `note` | `::notice` |

   This coincides with ADR-0050 §2's recorded note (proof fails by
   default → `error`; warn-demoted contract → `warning`) while
   generalizing it to the rule that produced it. The debug decision,
   argued rather than defaulted (ADR-0053 §13 left it open with an
   "omitted" lean):

   - **The explicit pair is carried, as `error` — omission is
     refuted.** A committed `PHPStan\dumpType()` reds CI (fail-level,
     fixed, ADR-0053 §3). Under format invariance the exit is 1 in
     every format; a SARIF/github rendering that omitted the finding
     would show a red run with zero annotations explaining it — the
     format actively hiding the cause of its own failure, the
     "surfaced means CI must see it" clause of ADR-0050 §7 violated
     by the serializer. Invisible reds are forbidden; the lean's
     rationale ("dumps are for humans at terminals") cannot override
     an exit code the run already produced.
   - **`debug.var-dump` is carried, as `note`/`::notice` — not
     omitted, not `warning`.** Omission would break invariance for no
     gain (the displayed line exists; hiding it per-format is a
     format-keyed suppression channel, refused). But `warning`
     misstates it to the ingestion surface: a contract warn-demotion
     is still a *claim* softly surfaced; a dump is an *answer* to a
     question the code asked (ADR-0053 §1) — SARIF's third level
     exists for exactly this register, and GitHub's `::notice`
     mirrors it. Teams drowning in legacy `var_dump` annotations have
     the designed relief valve — profile-disable `debug.var-dump`
     (ADR-0053 §4) — in the surface, where suppression lives, never
     in the format. The mapping's `Layer` match is exhaustive
     (ADR-0053 §1's compiler-forced posture): a future layer cannot
     silently fall into a level.

4. **`github` format: workflow commands plus the plain accounting.**
   One command per displayed finding, in the standard sorted order:

   ```
   ::error file={path},line={line},col={column},title={id}::{message}
   ```

   (`::warning` / `::notice` per point 3's table; `title` carries the
   id so the annotation is triageable without opening logs.) Escaping
   is GitHub's documented two-register scheme, committed verbatim as
   unit-test fixtures: in the message (data) `%`→`%25`, `\r`→`%0D`,
   `\n`→`%0A`; in properties (`file`, `title`) additionally
   `:`→`%3A`, `,`→`%2C`. After the commands, the same plain
   accounting lines `text` prints (vendor/suppressed/baselined/stale
   counts, the §8 drowns-loudly notice) — plain lines are inert in a
   workflow log and the accounting must not become format-dependent.

5. **Truncation posture: emit everything; the renderer's cap is the
   renderer's.** GitHub renders a bounded number of annotations per
   step/job; Steins does **not** pre-truncate to fit. A steins-side
   cap would be a silent, format-keyed suppression channel (point 1
   refuses it), and a run displaying hundreds of findings is drowning
   by definition — the exit code is 1, the log carries every line,
   and the adoption answer is the baseline round-trip (ADR-0022), not
   a prettier drowning. Ordering is the standard path/line sort, so
   what does render is deterministic; error/warning/notice caps are
   per-type on GitHub's side, so fail-level findings never compete
   with warn-level for slots.

6. **CI auto-detection: detect the consumer, never the context — and
   it only ever changes the spelling.** When `--format` is not given
   and `GITHUB_ACTIONS=true` is in the environment, the format is
   `github`; an explicit `--format` always wins; everything else
   about the run — surface, profile, pipeline, exit code — is
   untouched by detection (format invariance makes this checkable,
   and a test asserts it). This passes the lenient-default /
   explicitness tension cleanly: the lenient-default principle
   governs *reporting surfaces* (ADR-0050 amendment), and detection
   cannot touch a surface — it selects among spellings of one fixed
   set, the same class of adaptation as terminal-color detection.
   Only `GITHUB_ACTIONS` detects in v1: detection exists to pick a
   rendering the environment can *consume*, and a generic `CI=true`
   names no rendering — `text` is already correct there. `sarif` is
   never auto-selected: it is a file artifact chosen deliberately for
   an upload step, not a log rendering.

7. **Suppression stays closed.** Formats render the post-pipeline
   surface, full stop. SARIF's `suppressions` machinery is
   deliberately unused: re-emitting baselined or inline-ignored
   findings as suppressed results would make the format a second
   suppression UI beside the three channels ADR-0023 fixes as the
   whole surface, and would leak the baseline's contents into every
   upload. Suppressed findings appear as *counts* in
   `run.properties` / the accounting lines, exactly as `json`/`text`
   do today, and no further. Symmetrically (point 1), no format drops
   displayed findings; there is no `--exit-zero` flag for
   sarif-upload workflows — "surfaced means fail" (ADR-0050 §7) is
   identity, and `continue-on-error` on the CI side is the idiom for
   upload-anyway steps.

## Part II — `doctor` (issue #16)

8. **Identity: index-bound posture, never a check.** Doctor reads
   config, the environment (via the sidecar's `env()`/`reflect()`,
   ADR-0024), the project index and lowering-level facts (dam sites,
   docblock envelopes, declared references), and the baseline file.
   It never runs the diagnostic emitters, and its exit never depends
   on what `check` would find — a doctor that re-ran the checker
   would be a second gate wearing a stethoscope. Its runtime is
   index-bound, not inference-bound. Output is a human-readable
   sectioned report; machine-readable output is deferred-with-design
   (the section structure below is deliberately JSON-mappable; it
   ships when a consumer exists, per issue #16's non-goal).

9. **The report's sections — each line anchored to a standing
   commitment, none invented for doctor.**

   1. **Runtime** (ADR-0004/0024): which `php` resolved and how; the
      sidecar `env()` round-trip (version, SAPI, ini of note) and a
      `reflect()` probe; the loaded-extension list. Two amendment
      surfaces live here: the **SAPI notice** (A6 — existence claims
      are honest to the sidecar's SAPI; when `[runtime] sapi` is
      undeclared, say so and name the curated never-Absent set's
      standing), and the **monkey-patch warning** (A9 — `uopz` /
      `runkit7` / `Componere` loaded ⇒ the entire absence family is
      Unknown-silent; doctor names the extension loudly, because a
      silently voided family is exactly the incompleteness ADR-0004
      forbids leaving unsaid).
   2. **Coverage posture** (ADR-0004's promise made concrete): the
      sound-subset statement when no sidecar is reachable — naming
      the silenced claims by id (`call.undefined-function`,
      `class.undefined`, and per A2(ii) `call.undefined-method`);
      the **dam-site count** (the S1 dam fact,
      `crates/steins-infer/src/dam.rs` — "N dammed sites", broken
      down by eval / unproven include / non-literal `class_alias`)
      with the vouch count and the ADR-0046/0049 downgrade sentence
      ("absence claims conditional on N vouched dynamic-code
      exemptions"); the vendor posture (vendor findings suppressed by
      default, ADR-0015); the `contract_touches_class` count
      (ADR-0049 §11 — N phpdoc contracts reference unresolvable
      classes and stay closed/silent, a posture line and never a
      finding); the live dump-site count (ADR-0053 §13's deferred
      line — lands once the D3/D4 recognizer exists to consult).
   3. **Config** (ADR-0023): `steins.toml` parse status; the profile
      table's whole-table validation verbatim through
      `ProfileConfigs::resolve` (a broken-but-unselected profile is
      reported, same as check would); `@name` path-set and
      `[[policy]]` reference resolution once issue #15's config
      surface lands; the `[runtime]` pseudo-constants
      (`zend-assertions`, `warning-handler`, `sapi`,
      `include-path`) each shown as declared-or-defaulted, with the
      default named (`warning-handler` defaulting to `"abort"` is a
      posture fact worth a line — ADR-0049 §7 amendment).
   4. **Active surface** (ADR-0050): the resolved profile and its
      provenance (flag / `[check] profile` / built-in default), the
      layer summary, and the **written-but-unchecked-envelope
      notice** the G1 amendment assigned here: N declarations carry a
      written `@throws` (or effect envelope) whose checking id is off
      the active surface — under `default`, "N written envelopes are
      unchecked under the active profile (`contracts` checks them)";
      under `throws-direct`, "checked for direct origins only". This
      is the designed answer to "wrote `@throws`, got silence".
   5. **Baseline health** (ADR-0050 §8): entry count; the capture
      header's profile and id set versus the active surface; the
      dormant count (entries whose id is outside the active surface —
      kept, not stale); and the drowns-loudly condition previewed
      ("the active surface exceeds the capture surface by N ids —
      those findings are unbaselined"). A pre-§8 header (no capture
      surface) is reported as such, not failed.
   6. **Catalog** (ADR-0014/0021, A11): the builtin catalog's pinned
      php-src minor versus the sidecar-reported runtime minor; on
      skew, the A11 consequence stated ("catalog-backed is-a demoted
      to Unknown for arm deletion and descendant closure"); the
      hierarchy/foldable entry counts as freshness context.
   7. **Registry totality** (mechanics self-check): every emittable
      id registered with a layer, the
      `REGISTERED_NOT_YET_EMITTED` / emittable partition consistent.
      Redundant with the workspace test today — and exactly the
      check that stops being redundant the day plugin registration
      (ADR-0022/0039) puts ids into the registry at runtime.

10. **Exit semantics: environment degrades at 0, configuration
    contradicts at 1 — the crying-wolf lens applied.** Exit codes:

    - **0** — report produced, including *degraded* postures: no
      reachable PHP (the sound subset is a legitimate mode, ADR-0004
      — failing it by default would red every sidecar-less laptop and
      CI box, the crying-wolf prohibition verbatim), undeclared
      `[runtime] sapi`, catalog skew, monkey-patch extensions,
      dormant baseline entries. Degradation is *surfaced loudly,
      exit-neutrally* — the same posture ADR-0004 chose for check's
      startup notice.
    - **1** — hard contradiction: the config asserts something the
      world refutes. `steins.toml` unparseable; profile resolution
      error (reserved/unknown/cycle/unknown-id); a path-set or
      `[[policy]]` reference to nothing; an unparseable baseline
      file. The discriminator is principled, not a severity taste:
      these are exactly the conditions under which `check` itself
      exits 2 or silently diverges from declared intent — doctor
      failing on them is early detection of an already-failing run,
      never new noise.
    - **2** — doctor's own usage errors.

    Issue #16's "nonzero = at least one problem found" is thereby
    narrowed deliberately: a *problem with the configuration* fails;
    a *fact about the environment* reports. Teams that want
    environment facts to fail (e.g. "CI must have the sidecar")
    get it the lenient-default way — strictness as a named opt-in
    declaration, `[doctor] require = ["sidecar"]`-shaped config
    turning a posture line into an exit-1 line — deferred-with-design
    (point 13), not a default.

11. **What doctor is NOT** (each one line, each anchored):
    - **Not a linter** — doctor reports zero code findings; findings
      are check's (ADR-0017's boundary, again).
    - **Not `init`** — ADR-0020's refusal stands; doctor writes no
      config, generates no baseline, and its suggestions are
      sentences, not files (Skill-driven initialization remains the
      planned adoption path).
    - **Not the gate** — ADR-0013's apparatus verifies the analyzer
      against corpora repo-side; doctor verifies a *project's
      posture* user-side; conflating them would put a measurement
      instrument in the product.
    - **No auto-fixing** — fix-its are diagnostic payloads
      (ADR-0010); doctor has no diagnostics to attach them to.
    - **Not a second check** — no emitters run, and exit never
      depends on would-be findings (point 8).

## Slices

12. **Four Opus-sized slices, standard verification protocol**
    (workspace tests grow, clippy 0, release build, foreground
    fp-gate byte-identical where output is claimed unchanged,
    conformance rerun as a no-op — no findings-generation logic moves
    anywhere in this ADR). File-collision posture, stated for the
    concurrent D3/D4 (ADR-0053) and later S-slices: the formats live
    in steins-cli's output path (`main.rs`'s `Format` handling and
    print functions), which D3/D4 do not touch beyond the registry
    everyone touches; doctor is a new subcommand file plus one
    dispatch arm. Rebase hygiene applies to `main.rs`'s arg-parse and
    dispatch regions only.

    - **C1 — renderer seam + `github`**: extract the render boundary
      (displayed findings + surface + accounting → output) so formats
      are siblings, not branches of `run_check`; add
      `Format::Github` with the point-4 command emission and both
      escaping registers as fixture tests; auto-detection
      (`GITHUB_ACTIONS`, explicit-wins, detection-changes-nothing
      tests); the point-1 format-invariance test; recorded-output
      regression pinning `text`/`json` byte-identical.
    - **C2 — `sarif`**: `Format::Sarif` and the builder
      (rules-from-displayed, point-3 mapping with its exhaustive
      `Layer` match, facet properties, `automationDetails`,
      `run.properties` accounting); `partialFingerprints` reusing the
      baseline hash helper (extracted from `baseline.rs`, the one
      shared-file touch); serde snapshot fixtures for the committed
      schema shape. The external acceptance ("accepted by code
      scanning upload") is the M2 exit-criterion drill, not a unit
      test.
    - **C3 — doctor skeleton**: the subcommand (`doctor.rs`, one
      `main.rs` dispatch arm); sections Runtime, Config, Baseline
      health (all consuming existing surfaces: `env()`/`reflect()`,
      `load_profiles`/`load_runtime`, `baseline::parse_header` +
      `Surface::surface_ids`); the point-10 exit semantics with
      fixtures for each discriminator side.
    - **C4 — doctor posture sections**: Coverage posture (dam counts,
      vouches, sound-subset statement, `contract_touches_class`),
      Active surface with the unchecked-envelope scan, Catalog skew
      (A11), SAPI/monkey-patch lines (A6/A9), Registry totality. The
      dump-site count line lands here only if the D3/D4 recognizer
      has landed; otherwise it lands with D4, recorded in both
      places so neither slice waits on the other.

    C1→C2 and C3→C4 are ordered pairs; the pairs are independent of
    each other and of D3/D4/S-slices.

## Refusals

13. Each one line, each anchored:
    - **Per-layer SARIF level override config** — stays deferred
      exactly as ADR-0050 §11 left it ("if code-scanning ingestion
      demands them"); a severity knob re-imports the numeric ladder
      through the side door (ADR-0050 §10).
    - **SARIF `suppressions` for baselined/ignored findings** — the
      three channels are the whole suppression surface (ADR-0023);
      a format that re-surfaces suppressed findings is a fourth.
    - **Steins-side annotation truncation** — a cap is a silent
      format-keyed suppression channel (point 1); drowning is
      answered by the baseline, not by hiding.
    - **Generic `CI=true` detection or a per-CI format zoo** —
      detection selects a consumable rendering, and only
      `GITHUB_ACTIONS` names one today (point 6).
    - **`--exit-zero` / format-dependent exit codes** — "surfaced
      means fail" is identity (ADR-0050 §7); upload-anyway is the CI
      workflow's `continue-on-error`, not a Steins flag.
    - **Debug-layer omission from SARIF/github** — an invisible
      CI-red is a serializer lying about its own exit (point 3);
      ADR-0053's terminal-first rationale is honored by level
      (`note`), not by absence.
    - **Doctor as init/linter/gate/fixer/second-check** — the five
      anchors of point 11.
    - **Doctor exit-fail on environment facts by default** —
      crying-wolf (ADR-0002/0007): degradation reports, config
      contradiction fails; strictness is a named opt-in
      (lenient-default principle).

## Deferred-with-design

14. Ready when wanted, no redesign required:
    - **Rule metadata enrichment** — `shortDescription` prose,
      `fullDescription`, `helpUri` per id when a docs site exists;
      the rules array's shape already carries them.
    - **`[doctor] require = [...]`** — named posture-to-failure
      assertions (`"sidecar"`, `"catalog-pin-match"`, …) under the
      lenient-default principle: config carries the intent, doctor
      enforces it, exit 1 on violation.
    - **`doctor --format json`** — the point-9 section structure is
      the schema; ships when a consumer exists.
    - **Per-minor catalog tables** (A11's recorded refinement)
      surfacing in the Catalog section as "pin matched per-minor".
    - **SARIF for `transform`** — transform's report is a diff/plan,
      not findings; if code scanning ever wants it, it needs its own
      mapping ADR, not a widening of this one.
