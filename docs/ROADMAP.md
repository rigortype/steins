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

## Current state (verified against the tree, 2026-07-24)

Engine:

- Call-site value propagation with the four-layer value domain
  (ADR-0001/0035), branch analysis (ADR-0031), closures (ADR-0033),
  object state/heap (ADR-0036), throw accounting (ADR-0040), effects
  (ADR-0005/0018), object/method world complete (ADR-0043: trinary is-a
  over a 352-entry generated builtin hierarchy, native + phpdoc object
  acceptance, enums, `::class`).
- salsa (ADR-0009) memoizes `parse`, `function_index`, and a monolithic
  `project_index`; the check pass itself runs *outside* the query graph
  (ADR-0028, folding impurity) — nothing of inference is memoized
  across runs. Acceptable for batch CLI; the LSP prerequisite is
  ADR-0048 §5.
- Diagnostic surface (all landed ids): `type.argument-mismatch`,
  `type.return-mismatch`, `type.property-mismatch`, `call.on-null`,
  `readonly.reassigned`, `phpdoc.param-mismatch`,
  `phpdoc.return-mismatch`, `phpdoc.property-mismatch`,
  `throw.undeclared`, `throw.liskov-widened`, `effect.envelope-exceeded`,
  `effect.unknown-label`, `effect.liskov-widened`, plus
  `suppress.unmatched`/`suppress.unknown-id`.

Verification apparatus (ADR-0013):

- fp-gate: runtime layer zero-FP over 90,709 corpus files (10 pinned
  OSS packages + a private legacy monorepo injected via
  `corpus.local.toml`). `phpdoc.*` (488) and `throw.*` (44,164) are
  increase-tripwires in measurement mode; triaged true positives are
  fingerprint-pinned (`EXPECTED_PROOF_FINDINGS`).
- php-typing-conformance: 85/98. Remaining fails: one intended
  (#14939 list semantics), the registered intentional silences
  (ADR-0030), and the unimplemented queue below.
- ~780 workspace tests; zero conformance regressions ever.

CLI (ADR-0020, partially landed):

- Landed: `check` (`--format text|json`, `--no-php` sound subset,
  `--vendor-diagnostics`, baseline set/match/stale per ADR-0022),
  `annotate` (margin facts, `…?` non-exhaustiveness), `transform`
  (`phpdoc-to-native`, `phpdoc-honesty`; dry-run default, `--apply`
  gated on zero-new-diagnostics; vouch valve + partition regions read
  from `steins.toml`). Inline `@steins-ignore` with anti-rot.
- NOT landed (declared in ADR-0020/0023, absent from the binary):
  `sarif`/`github` output, `doctor`, `lsp`, `mcp`, `--profile` /
  policy profiles, `check --fix` fix-its, `[paths.sets]` / `[[policy]]`
  scoped policy (steins.toml is parsed only for `[transform.*]`).
- `check` does not separate layers: `phpdoc.*` and `throw.*` findings
  print beside proof-layer findings. The measurement-mode distinction
  exists only in the xtask gate. On the legacy monorepo this means
  ~44k `throw.undeclared` findings in a default run — the single
  largest adoption blocker in the current binary.

Transforms (ADR-0034/0041): promotion + honesty landed through method
scope (ADR-0043 stage 5) with the full refusal taxonomy, eval/include
obstacles (ADR-0046), `no-observed-callers`, and the vouch valve.
Whole-universe closing measurement: 23,148 / 509 candidates enumerated,
0 transformed — dynamic dispatch is the sound floor; partitioning
(ADR-0047) is the recorded precision axis (slice A landed, B in flight,
C–E queued).

## The distance to PHPStan-practical

What a team adopting the checker needs, versus what exists. Each gap
names its milestone.

1. **Finding breadth.** No undefined-function/method/class/property,
   argument-count, or offset-access ids exist. A checker that is silent
   on `$foo->tyop()` is not adoptable regardless of its precision
   elsewhere. Zero-FP variants are reachable (definite-No only under
   the closed-world conditions ADR-0043 established). → M1
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
6. **CI surface.** `sarif`/`github` formats with auto-detection;
   `doctor` for coverage posture, sidecar health, catalog audit. → M2
7. **Vendor and extension maturity.** Vendor trees are analyzed as
   source (works, budgeted per ADR-0015). Classes from unloaded PHP
   extensions (`ext-redis`, …) are Unknown-silent; the sidecar's
   `reflect()` (ADR-0024) against the project's own PHP is the designed
   answer and is unused today. → M2/M4
8. **Per-PHP-version posture.** Steins analyzes against the project's
   real PHP (sidecar `env()`, ask-the-real-thing) — a documented
   posture, not a version-emulation matrix (see Won't build). → M2
   (documentation), library-range checking deferred.
9. **Ecosystem packs** (ADR-0044/0045: PSL, Serde, Valinor, PSR) —
   designed, not implemented; the mapper-boundary types they recover
   are exactly where legacy modernization needs truth. → M4
10. **Performance and incrementality.** ~200s full batch over 90,709
    files on dev hardware is CI-viable; there is no warm story and no
    cross-run persistence. Not checker-release-blocking; LSP-blocking.
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
  (ADR-0030) — zero absent-machinery fails. (From 85/98; the ceiling
  is set by intentional entries, expected ≥ 93/98.)
- fp-gate green over the full corpus; every tripwire movement triaged
  verbatim (5-sample minimum per class).
- Issues #1–5 closed.
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
  user.
- Tagged v0.1.0; a third party can install and reproduce the adoption
  drill from docs alone.

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

Goal: the warm path exists and is measured; ADR-0048 §5 prerequisites
land.

Work: perf harness under xtask (cold/warm, per-corpus); per-declaration
entry-state summaries as memoized salsa queries; `project_index`
sharded per-symbol (the ADR-0009 recorded plan); fold results into the
graph as recorded inputs (ADR-0028's revisit trigger).

Exit criteria:

- Cold full-run within 10% of the pre-decomposition batch time (the
  decomposition must not tax CI).
- Warm re-check after a single-file edit on the ~30k-file first-party
  scale: ≤ 2s p95 (provisional — the harness sets the final number
  from measured baselines, and the target is recorded in the harness,
  not here).

### M6 — LSP preview

Goal: `steins lsp` with diagnostics, hover, and the flagship:
type-directed member completion at the cursor.

Work: `steins-lsp` crate per ADR-0048 §6; span-keyed facts (LineFact
generalization); position queries by scope replay from memoized entry
states (ADR-0048 §1); completion = facts-at-position → type → members
(the second half — project index + trinary is-a + `TypeMember` — has
existed since ADR-0043); sidecar crash transparency in-session
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
global-ordering dependence (ADR-0048 §2–4). Everything else about LSP
is M5/M6 work.

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
- **G3 — core relicense** AGPL → Apache-2.0/MPL-2.0, settled before
  the first external contribution (ADR-0025). Blocks M3 (or a DCO/CLA
  is the recorded fallback if contributions arrive first).
- **G4 — conformance-repo checker adapter.** SteinsChecker + `--tool`
  filter exist uncommitted in the user's php-typing-conformance
  working tree; committing is theirs. Affects M1 measurement
  convenience only.
- **G5 — this roadmap's order.** M4-before-M5 (knowledge before
  incrementality) is the recommendation, on the grounds that packs
  move checker usefulness while decomposition moves only latency;
  reversible on the user's call.
