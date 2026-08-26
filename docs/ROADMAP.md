# Roadmap: to checking real projects

What must land, in what order, for a team to adopt Steins on a real
codebase the way they adopt PHPStan — and what deliberately never lands.
ADRs are the canon; this document sequences them and holds the exit
criteria. On conflict, the ADR wins and this file is stale.

**Release order (binding):** the checker releases first. LSP and
Rector-style editing are design-core from day one — no decision below may
paint them out, and ADR-0048 binds current inference work to keep
position queries reachable — but they release after the checker is
genuinely usable. The specifically protected LSP capability:
type-directed member completion at a cursor position.

## Current state (verified against the tree, 2026-08-26)

Engine:

- Call-site value propagation with the four-layer value domain
  (ADR-0001/0035), branch analysis (ADR-0031), closures (ADR-0033),
  object state/heap (ADR-0036), throw accounting (ADR-0040), effects
  (ADR-0005/0018), object/method world complete (ADR-0043: trinary is-a
  over a 352-entry generated builtin hierarchy, native + phpdoc object
  acceptance, enums, `::class`).
- **Frozen generations (ADR-0092) have landed**, superseding ADR-0009's
  mechanism (its principles stand) and replacing ADR-0048 §5's
  prerequisite list. A run captures its sources behind a seal, builds
  per-Composer-package artifacts — symbol shards, declared contracts,
  per-file trace IR, per-declaration own-rows and walk blocks — and
  publishes them atomically; the next run reuses every package and every
  file the content fingerprints say is unmoved, walks only the files an
  edit could reach, and answers the rest from persisted diagnostics. Fold
  results persist as one generation-level table keyed by engine identity
  (ADR-0092 §4, over ADR-0066's replay seam). salsa still memoizes `parse`
  within a run; the check pass still runs outside the query graph
  (ADR-0028), which is now a placement rather than a limitation.
- Diagnostic surface, through v0.1.6. Pre-existing: `type.argument-mismatch`,
  `type.return-mismatch`, `type.property-mismatch`, `call.on-null`,
  `readonly.reassigned`, `phpdoc.param-mismatch`,
  `phpdoc.return-mismatch`, `phpdoc.property-mismatch`,
  `phpdoc.undefined-method`, `throw.undeclared`, `throw.liskov-widened`,
  `effect.envelope-exceeded`, `effect.unknown-label`,
  `effect.liskov-widened`, `call.undefined-function`,
  `call.undefined-method`, `call.too-few-arguments`,
  `call.unknown-named-argument`, `class.undefined`, `offset.missing`,
  `offset.maybe-missing`, `offset.undeclared`, `offset.on-unsupported`,
  the `debug.*` family, plus
  `suppress.unmatched`/`suppress.unknown-id`.
- v0.1.4 added thirty-nine ids, closing gap 1 below: the absence family
  (`property.undefined`, `class-const.undefined`, `constant.undefined`,
  `variable.undefined`), the inaccessibility family
  (`call.inaccessible-method`, `property.inaccessible`,
  `class-const.inaccessible`), the declaration fatals
  (`class.abstract-unimplemented`, `class.extends-final`, the five
  `override.*`), the value-domain checks (`call.on-non-object`,
  `property.on-non-object`, `foreach.non-iterable`,
  `type.invalid-operand`, `string.non-stringable`,
  `string.array-conversion`), reachability (`type.return-missing`,
  `type.return-maybe-missing`), mechanics
  (`syntax.unparsable`, `array.duplicate-key`, the five `phpdoc.*`
  rot ids, `closure.unused-use`), `preg.invalid-pattern`,
  `call.printf-too-few-arguments`, and the six contract-layer
  `untyped.*`, plus the two possibly-grade legs ADR-0081 registered
  (`variable.maybe-undefined`, `property.maybe-undefined`) — ahead of
  emission alongside `call.too-many-arguments`, which waits for the
  reflect slice.
- v0.1.5 added one id, `effect.interop-unknown-label` (ADR-0082, the
  typo check for labels written in PHPStan's purity tags; contract
  layer, rides with `effect.envelope-exceeded`). The two ADR-0081
  possibly-grade legs registered ahead of emission since v0.1.4,
  `variable.maybe-undefined` and `property.maybe-undefined`, now fire
  under `strict` (#267).
- v0.1.6 added four ids. `type.maybe-argument-mismatch` and
  `phpdoc.maybe-argument-mismatch` (#391, ADR-0081 §8): a builtin's
  `T|false` handed straight to a native `T` — the possibly grade on
  the argument side, `strict` floor, prefix split by premise grade.
  `phpdoc.maybe-undefined` (#396, ADR-0087 §4): a read of a top-level
  `@var T|unset $x` the declaration says may not be bound — contract
  layer, `contracts` floor, since its premise is the docblock rather
  than reachability. `phpdoc.never-param-reachable` (#428, ADR-0088
  §4): a `@param never` sentinel reached with a non-empty declared
  domain, carved out of `phpdoc.param-mismatch`, which no longer
  judges a never-declared parameter at all.
  `call.too-many-arguments` is now the only id registered ahead of
  emission, still waiting on the reflect slice.

Verification apparatus (ADR-0013):

- fp-gate: runtime layer zero-FP over ~85k corpus files (10 pinned
  OSS packages + phpstan-src and a private legacy monorepo injected
  via `corpus.local.toml`; those two are unpinned live checkouts, so
  the total drifts with them). `phpdoc.*` (680), `throw.*` (44,333)
  and `effect.*` (4,442) are increase-tripwires in measurement mode,
  and so are the **possibly-grade** proof ids (`Layer::Proof` +
  `Floor::Strict`) since ADR-0081 §8 scoped the strict-zero bar to
  the definite ids; triaged true positives among the definite ids are
  fingerprint-pinned (`EXPECTED_PROOF_FINDINGS`).
- php-typing-conformance: **216/230, re-measured 2026-08-26** against the
  sibling repo at `f8ed38b`. The verdicts are identical to the ones that
  repo already records — zero newly failing, zero newly passing — so the
  twenty-two-PR ADR-0092 series moved no conformance row, which is an
  independent witness to its behaviour preservation alongside the fp-gate
  and the warm ≡ cold oracle. Of the 14 fails, three are registered
  refusals (ADR-0030 entries 1–2) and one is the `resource`-domain
  deferral (entry 4); the other ten arrived with cases the suite added
  after the 2026-08-09 measurement and cluster in narrowing
  (`regressions_*_narrowing`, `regressions_string_narrowing_assert_if_true`),
  properties, and `phpdoc_advanced_member_tag_undefined_type`. One row
  moved *toward* Steins since that measurement:
  `assertions_this_out_self_out` now passes, closing the generics leg M1's
  exit criteria named. The suite lives in the sibling repo, so its
  denominator moves without notice here; re-measure and date this line
  rather than trusting it.
- ~5,210 workspace tests; zero conformance regressions ever.

CLI (ADR-0020, partially landed):

- Landed: `check` (`--format text|json|github|sarif`, `--profile`,
  `--no-php` sound subset, `--no-cache` to skip the generation store
  (ADR-0092; on by default since 2026-08-26), `--no-tolerated-effects` audit switch per
  ADR-0084, `--vendor-diagnostics`, `--fix`, baseline
  set/match/stale per ADR-0022), `annotate` (margin facts, `…?`
  non-exhaustiveness), `transform` (`phpdoc-to-native`,
  `phpdoc-honesty`, `throws-envelope`, `effects-envelope`,
  `loop-to-array-map`; dry-run default, `--apply`
  gated on zero-new-diagnostics; `--asserted-subjects` opt-in; vouch
  valve + partition regions read from `steins.toml`), `effect-diff`,
  `doctor` (ADR-0054 C3 minimal scope, plus `--format json` and the
  Catalog/Registry/SAPI posture sections, ADR-0054 C4 partial), and
  `mcp` (stdio MCP server, four tools). Inline `@steins-ignore` with
  anti-rot.
- NOT landed (declared in ADR-0020/0023, absent from the binary):
  `lsp`, `doctor`'s remaining ADR-0054 C4 audits (the dump-site count
  and `contract_touches_class`'s project-wide count), `[paths.sets]` /
  `[[policy]]` scoped policy, and every `check --fix` family beyond the
  `debug.*` dump-removal one.
- `check` separates layers (ADR-0050): the cumulative ladder
  `default ⊂ contracts ⊂ strict` is live behind `--profile`, so
  `phpdoc.*` and `throw.*` no longer print in a default run and the
  ~44k `throw.undeclared` findings the legacy monorepo produced are
  opt-in. The mechanics layer (ADR-0078) is the exception by design:
  on by default, disable-proof and undemotable.

Transforms (ADR-0034/0041): promotion + honesty landed through method
scope (ADR-0043 stage 5) with the full refusal taxonomy, eval/include
obstacles (ADR-0046), `no-observed-callers`, and the vouch valve.
ADR-0010's flagship loop→`array_map` landed under ADR-0076 — the first
transform preconditioned on a *proven* effect judgment (and a proven
throw set, stricter than `Pure`), with a differential fixture that runs
both spellings under the real PHP.
Whole-universe closing measurement: 23,148 / 509 candidates enumerated,
0 transformed — dynamic dispatch is the sound floor; partitioning
(ADR-0047) is the recorded precision axis (slice A landed, B in flight,
C–E queued).

## The distance to PHPStan-practical

What a team adopting the checker needs, versus what exists. Each gap
names its milestone.

1. **Finding breadth. CLOSED in v0.1.4.** The
   undefined-function/method/class/property, argument-count and
   offset-access ids all exist, each shipped as a zero-FP variant
   (definite-No only under the closed-world conditions ADR-0043
   established) and corpus-triaged before its id shipped. See the
   diagnostic surface above for the full list. The rest of M1's exit
   criteria are unaffected by this and still bind. → M1
2. **Narrowing and assertions.** Real code is guard-heavy. The deferred
   list — `@phpstan-assert-if-true/-if-false`, `assert()`,
   short-circuit refinement, loops beyond write-sets, static props,
   property chains — costs true positives (never FPs: unknowns widen to
   silence), which costs adoption credibility. → M1
3. **Generics carry, callable signatures, template scope transfer**
   (ADR-0030 queue; ADR-0032; issues #1–4, FP #5). Collections and
   callbacks are where application code lives. → M1
4. **Layer separation and noise control.** `phpdoc.*`/`throw.*` must
   stop printing in a default `check`. The zero-FP banner applies to
   the proof layer; contract-layer findings are true but are *debt
   reporting*, and debt reporting is opt-in (crying-wolf prohibition).
   `throw.undeclared`'s default posture is a USER decision (gate G1).
   → M2
5. **Config UX.** ADR-0023's `[paths.sets]` and `[[policy]]` scoped
   policy are designed, not implemented. Zero-config must stay true;
   config carries intent only. → M2
6. **CI surface.** `sarif`/`github` formats with auto-detection have
   landed (ADR-0054 C1/C2); what remains is `doctor`'s richer coverage
   posture, sidecar health and catalog audit. → M2
7. **Vendor and extension maturity.** Vendor trees are analyzed as
   source (works, budgeted per ADR-0015). The sidecar's `reflect()`
   (ADR-0024) now resolves extension classes against the project's own
   PHP (#269, gap 7's first slice): a class an installed extension
   provides is no longer unknown, while a class the runtime cannot
   reflect stays Unknown-silent and reflected declarations never
   premise an absence-family finding. What remains is the rest of the
   maturity story — the vendor budget cap and the wider reflect
   surface. → M4
8. **Per-PHP-version posture.** Steins analyzes against the project's
   real PHP (sidecar `env()`, ask-the-real-thing) — a documented
   posture, not a version-emulation matrix (see Won't build). → M2
   (documentation), library-range checking deferred.
9. **Ecosystem packs** (ADR-0044/0045: PSL, Serde, Valinor, PSR) —
   designed, not implemented; the mapper-boundary types they recover
   are exactly where legacy modernization needs truth. → M4
10. **Performance and incrementality. Largely CLOSED at M5.** The warm
    path exists, persists across runs and is measured: on the ten pinned
    corpus packages (6,670 files) a cold run is 7.70s and a rebuild that
    walks nothing is 1.41s; on nikic/PHP-Parser with the engine on, cold
    1.05s against 0.17s after a leaf-file edit (2 files walked, 339
    replayed). `cargo xtask perf` carries the numbers and the warm ≡ cold
    oracle, and `--paranoid` grades every would-be skip against a fresh
    walk. What remains is the last exit criterion: at the ~30k-file scale
    a zero-walk rebuild still straight-lines to roughly 6s, over the ≤2s
    target, in phases that scale with the universe rather than the edit.
    → M5
11. **Adoption path.** Docs, install/distribution, licensing, public
    repo — USER gates G2/G3. → M3
12. **Position queries** (LSP): constrained now by ADR-0048, built at
    M6. **Editing/MCP**: M7 plus the standing background track.

## Milestones

Ordering rule: M1→M2→M3 are strictly sequential (the checker release
path). M4/M5 may interleave after M2. M6 follows M5. M7 follows the
checker release. The background track (below) runs throughout but
yields to milestone work on contention.

### M1 — Semantic core completion

Goal: the checker finds what a PHPStan user expects it to find, at the
zero-FP bar.

Work: narrowing/assertions (gap 2, in ADR-0030 queue order); generic
type-argument carry (ADR-0032 stage 1); callable signatures; template
scope transfer (#1–4) and the #5 shadow FP; new finding ids for
undefined symbols / arity / offset access under closed-world
conditions, each corpus-triaged before its id ships.

Exit criteria:

- Every php-typing-conformance fail is a registered divergence
  (ADR-0030) — zero absent-machinery fails. **Not met today**, and the
  distance grew because the suite did. At the 2026-08-26 measurement
  Steins passes 216 of 230 automated cases. Four fails are accounted
  for: three registered standing refusals — `phpdoc_advanced_vendor_prefixed_param_phan`
  (entry 1, tool-tag scope) and the two declaration-coherence cases
  `phpdoc_advanced_param_typehint_nullable_mismatch` and its
  `…_array_nullable_mismatch` variant (entry 2, a refusal PHPStan
  shares) — plus `native_types_resource_argument`, the `resource`
  value domain deferred as entry 4. The generics leg this criterion
  used to name **closed**: `assertions_this_out_self_out` passes, after
  template bounds read as upper-bound contracts (#293), offset-read
  breadth (#288), type arguments off inheritance edges (#294) and carry
  through a variable binding (#295) took the family's other cases.
  The remaining ten fails arrived with cases the suite added after
  2026-08-09 and are not yet triaged into capabilities; they cluster in
  narrowing (`regressions_class_string_negative_narrowing`,
  `regressions_object_property_discriminant_narrowing`,
  `regressions_string_narrowing_assert_if_true`,
  `assertions_array_key_exists_key_narrowing` — the M1 gap-2 territory),
  in properties (`properties_uninitialized_read`,
  `properties_promoted_property_hook_body`), and in
  `phpdoc_advanced_member_tag_undefined_type`. Triaging them into named
  capabilities, the way the previous batch was, is the next step on this
  criterion. No fail is an unregistered intentional divergence, and none
  is a defect — every one is a silence, not a wrong answer.
- fp-gate green over the full corpus; every tripwire movement triaged
  verbatim (5-sample minimum per class).
- Issue #5 (the template-shadow FP) closed. Issues #1–4 (the template
  tracer) are **out of v0.1.0 scope** (owner decision, 2026-07-24):
  ADR-0051's design stands, its implementation moves to the post-release
  background track and is promoted only if dogfooding demands it.
- New-id true-positive yield measured on the legacy monorepo and
  reported (a number, not an impression).

### M2 — Adoption surface

Goal: a stranger can run `steins check` on their project and get a
quiet, true, CI-ready result.

Work: layer separation — `check` defaults to the proof layer;
contract-layer families (`phpdoc.*`, `throw.*` per G1) move behind
policy profiles (`--profile`, ADR-0020); scoped policy + `[paths.sets]`
(ADR-0023); `sarif` and `github` formats with CI auto-detection;
`doctor`; extension-class reflection via the sidecar (gap 7, first
slice); the per-version posture documented.

Exit criteria:

- Default `check` on the legacy monorepo prints proof-layer findings
  only; profiles reach the contract layer intentionally.
- Adoption drill on ≥ 2 held-out well-known OSS projects (never used
  for tuning): zero false positives, a documented true-positive list a
  maintainer would plausibly accept, baseline round-trip
  (`--set-baseline` → edit → only new findings), GitHub Actions run
  annotating a PR via the `github` format, `sarif` accepted by code
  scanning.
- G1 decided by the user and implemented accordingly.

### M3 — Checker release 0.1

Goal: public, installable, documented. Blocked on USER gates, by
design.

Work: adoption guide (quickstart, baseline workflow, suppression
channels, coverage posture), install path (release binaries;
`cargo install` at minimum), issue intake conventions
(docs/agents/issue-tracker.md), versioning policy.

Exit criteria:

- Gates G2 (public repos, ADR-0025) and G3 (license) resolved by the
  user. **Both resolved**: G2 with the repo going public, G3 on
  2026-07-25 with the relicense to Apache-2.0.
- Tagged v0.1.0; a third party can install and reproduce the adoption
  drill from docs alone. **v0.1.0 tagged 2026-07-25** — five prebuilt
  targets, a Homebrew tap, and `cargo install --git`, with the install
  path written up in the quickstart and the handbook. The
  third-party-reproduction half is not self-certifiable and stays open
  until someone outside the project actually does it.

### M4 — Ecosystem knowledge

Goal: the checker understands the runtime-enforced boundaries real
projects are built on.

Work: packs in dependency-verified order Valinor → Serde → PSR → PSL
(ADR-0044/0045; preconditions: named-arg proven-value reading,
int-range rendering); composer.lock-driven pack activation; extension
reflection hardening (gap 7 completion).

Exit criteria: each pack's fixture suite green; fp-gate unmoved;
mapper-boundary true-positive/coverage delta measured on the corpus
and the monorepo.

### M5 — Incrementality and scale

Goal: the warm path exists, is measured, and survives the process;
ADR-0092's frozen generations land. (This milestone was rewritten
2026-08-25 when ADR-0092 superseded the salsa-decomposition plan; the
prior text is in history.)

Work — **landed 2026-08-25/26**, tracked on issue #493: the perf harness
with the warm ≡ cold oracle and the `--paranoid` verifier; generation
identity and candidate-then-publish (§2); the Composer-package
partition, reverse-closure invalidation and per-generation global merges
(§3); per-package artifacts — symbol shards, contracts, per-file trace
IR, per-declaration own-rows and walk blocks (§2, §4); the fold table as
one generation-level recorded input (§4); the warm lifecycle and walk
skipping (§5); a compact binary payload codec; artifact sharing between
generations, and a cache's durability posture. The cache is now **how `steins check`
runs** (#525, owner decision 2026-08-26): on by default, `--no-cache` to
opt out, silent — a default run's output is byte-identical to what it was
before the series — and the fp-gate now analyzes every corpus project
twice through the orchestrator, cold then warm, requiring the two finding
sets to match. The store is **bounded at one generation** (#529): a
publish sweeps what it superseded and every open collects what a crash
left unreachable, so a day of editing costs one cached analysis rather
than one per edit. **Still open**: parallelism re-scoped by measurement
from the generation build to `check_units`' per-file loop (#490); and the
MCP server resident over published generations (#491), which still
re-analyzes from scratch per call.

Exit criteria:

- Cold full-run within 10% of the pre-persistence batch time (the
  persistence layer must not tax CI). **Met** — cold reads no artifact,
  and the measured cold path is unmoved.
- Warm re-check after a single-file edit on the ~30k-file first-party
  scale: ≤ 2s p95 (provisional — the harness sets the final number
  from measured baselines, and the target is recorded in the harness,
  not here). **Not met.** At 6,670 files a zero-walk rebuild is 1.41s,
  which straight-lines to roughly 6s at 30k. Everything the edit reaches
  is now proportional to the edit; what is left scales with the universe
  (capture, and the phases #516/#519/#521 have been working down).
  Judging this honestly needs a real first-party tree at that scale — a
  synthetic multiple of the corpus measures a different shape, and once
  measured one wrong (issue #523's retraction).
- Warm ≡ cold: a warm generation's findings are byte-identical to a
  cold build of the same tree, pinned as a differential gate in the
  harness (ADR-0092 §5). **Met and pinned.** The `--paranoid` verifier
  additionally grades every file the affected set *would* have skipped
  against a fresh walk of it; tens of thousands of such grades across
  five corpus targets and every seeded edit shape have come back
  byte-identical. Its limit is worth stating: it proves the answer, not
  the reasoning, so a missed dependency whose findings happen to agree
  passes — which is how two closure holes reached #515 unnoticed.
- An unchanged vendor tree costs no vendor re-analysis on a warm run.
  **Met** — an unmoved package neither parses nor walks, and #520 shares
  its artifact rather than rewriting it.

### M6 — LSP preview

Goal: `steins lsp` with diagnostics, hover, and the flagship:
type-directed member completion at the cursor.

Work: `steins-lsp` crate per ADR-0048 §6; span-keyed facts (LineFact
generalization); position queries by scope replay from the generation's
persisted entry states (ADR-0048 §1, ADR-0092 §5); the dirty-buffer
contract — request-only re-analysis of the enclosing declaration
against the frozen generation, header anchoring, cross-file staleness
refused by name (ADR-0092 §6); completion = facts-at-position → type →
members (the second half — project index + trinary is-a + `TypeMember`
— has existed since ADR-0043); sidecar crash transparency in-session
(ADR-0024's stateless methods).

Exit criteria: completion correctness fixture matrix (receiver forms ×
visibility × hierarchy states, Unknown renders as honest incompleteness
not guesses); warm completion p95 ≤ 150ms on the monorepo-scale
project (provisional, harness-recorded); a session survives a sidecar
kill without a wrong or lost diagnostic.

### M7 — Editing and MCP release

Goal: the agent-driven refactoring loop (ADR-0010) ships.

Work: `steins mcp` exposing check/annotate/transform with the
dry-run → diff → approve → apply loop over EditPlan as currency
(ADR-0034); fold- and dataflow-backed transform proofs (lifting v1's
literal-only `argument-not-proven` dominance, ADR-0041 §1); next
transforms per ADR-0034: DTO promotion (array-shape sprawl → class),
stringly → enum; partitioning C–E closed with the ADR-0047 §8
prediction judged against measurement.

Exit criteria: an agent completes a promotion campaign on a partition
of the legacy monorepo end-to-end through MCP with the completeness
oracle accounting every candidate; the 3,000–4,000 unlock prediction
(ADR-0047 §8) evaluated and the result recorded, whichever way it
falls.

## Background track: transforms and partitioning

Transform machinery is landed and its remaining slices (partitioning
B–E, oracle refinements) are small and well-briefed (issues). Rules:

- The track never blocks an M1–M3 exit criterion; it absorbs effort
  when milestone work is blocked on user gates or review.
- Anything touching the sweep surface coordinates with checker work
  sharing it (the issue-#6 precedent).
- New transform *kinds* wait for M7; slices of already-designed
  machinery may land anytime under the standing verification protocol.

## LSP: the position-query decision

ADR-0048 (accepted alongside this roadmap) decides **replay over
retention**: position facts are answered by re-walking the enclosing
scope from a memoized per-declaration entry state — not by retaining
position-indexed fact tables (memory at 30k-file scale, plus
invalidation would need replay anyway), not by per-query whole-project
re-inference (minutes-scale). What binds *today* is deliberately
minimal: scope-walk replayability, canonical entry states, no
global-ordering dependence (ADR-0048 §2–4). ADR-0092 decides how the
entry states persist and what invalidates them — frozen per-package
generations, not a finer query graph. Everything else about LSP is
M5/M6 work.

## Won't build

A roadmap is also a refusal list. Each entry is anchored; "PHPStan has
it" is not a reason.

- **Benevolent unions** — compensation for worst-case FPs a proof
  layer doesn't emit; grammar accepted, semantics erased; failure-arm
  labels replace the need (ADR-0030 reg. 3, ADR-0042).
- **Narrow-LHS `accepts` strictness** — worst-case reasoning on
  declared types; the single overlap relation stays (ADR-0030 reg. 4).
- **Declaration-coherence lints** — "native wider than phpdoc" style
  findings; code is type-safe, a proof layer speaks on proven breaks.
  At most a future policy profile, never core (ADR-0030 silences §2).
- **Worst-case maybe-reporting as errors** — `maybe` is reported as
  `maybe` or not at all; no `treatPhpDocTypesAsCertain`-style toggles —
  trust order is fixed (ADR-0002, ADR-0009, ADR-0037).
- **Numeric strictness levels** — policy profiles are named intent, not
  a ladder (ADR-0020/0023).
- **`ignoreErrors` sprawl / message-regex suppression** — IDs + scoped
  policy + baseline are the whole surface; message wording is not a
  contract (ADR-0023).
- **A call-site template solver** — where propagation reaches,
  templates are transparent; accepted cost: thin library-author
  generic-signature lints (ADR-0032).
- **A TypeCombinator/TypeUtils layer** — combination happens in the
  value lattice; a type-side normalizer is extracted from the rendering
  boundary when narrowing/subtraction demands it (ADR-0030 amendment).
- **Lint/format rules, Rector integration, migration rulesets** —
  boundary decisions (ADR-0017, ADR-0010).
- **Tool-specific tags beyond `@phpstan-*`/`@psalm-*`** (ADR-0029).
- **`init` command / config generators** — zero-config is the banner;
  adoption is conversational (skill-driven) when it needs help at all
  (ADR-0020).
- **PHP-version emulation matrix** — Steins asks the project's real PHP
  (ADR-0004/0024). Library-mode range checking is deferred, not
  refused; emulating versions the project doesn't run is refused.

## User decision gates

These are decisions the roadmap *waits on*; nothing here pre-decides
them.

- **G1 — `throw.undeclared` default posture.** ON today and printed in
  default runs; the monorepo carries ~44k such findings. Options:
  keep-on, demote to a policy profile, split (on for
  envelope-carrying code only). Blocks the M2 exit.
- **G2 — public repo creation** (`rigortype/steins-attributes`, and
  the core repo's visibility; ADR-0025). Blocks M3.
- **G3 — core relicense — RESOLVED (2026-07-25): Apache-2.0.** Settled
  before the first external contribution, so no consent was required and
  the DCO/CLA fallback was never needed (ADR-0025 amendment). No longer
  blocks M3.
- **G4 — conformance-repo checker adapter.** SteinsChecker + `--tool`
  filter exist uncommitted in the user's php-typing-conformance
  working tree; committing is theirs. Affects M1 measurement
  convenience only.
- **G5 — this roadmap's order — OVERTAKEN BY EVENTS (2026-08-26).**
  The recommendation was M4 before M5, on the grounds that packs move
  checker usefulness while decomposition moves only latency. M5 ran
  first anyway: ADR-0092 landed as one twenty-two-PR series and most of
  the milestone is done. The reasoning behind the recommendation was
  not wrong and still applies to what is left — M4 is now the larger
  open milestone, and the remaining M5 items (#490, #491, #525) are
  bounded.
