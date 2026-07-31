# Dynamic return typing: the PHPStan extension-point import lands in five named seams

Owner directive (2026-07-29): organize and advance the port of PHPStan's
dynamic-typing machinery. Status: PENDING ratification (post-hoc mode).
Scope: the behaviors PHPStan ships as `Dynamic{Function,Method,StaticMethod}ReturnTypeExtension`
and `{Function,Method,StaticMethod}TypeSpecifyingExtension` — per-call
computed return types and per-guard narrowing — for builtins and the
stdlib. Framework-level dynamics (Larastan-class magic) are explicitly the
plugin surface (ADR-0012) and out of scope here.

## 1. Decision: one behavior, one seam

Every imported extension behavior is classified into exactly one of five
existing Steins seams — no new extension mechanism is built:

| Seam | Steins machinery | What imports here |
| --- | --- | --- |
| (i) Fold | sidecar folding (ADR-0004/0028, foldable allowlist) | literal-argument calls the real PHP can answer (`strtoupper('a')`, `range(1,3)`) — already live; grows by allowlist row |
| (ii) Symbolic transfer | argument-dependent returns (ADR-0061 A11/A12 machinery) | shape/arg-structure-dependent returns computable without execution: `explode` non-empty-list, `json_decode` by flags, `range` bounds, array positional projections (= ADR-0062 S7), `str_split`, `array_chunk`, … |
| (iii) Curated rows | builtin return facts under the THREE-legged admission gate (ADR-0056, extended by ADR-0061) | envelope refinements no rule computes but probes verify — the existing 11-row discipline, verbatim probes required, refused-row table maintained |
| (iv) Plugin | sidecar plugins (ADR-0012) | userland/framework dynamic returns; deferred post-v0.1.x |
| (v) Guard vocabulary | narrowing operators (ADR-0052; ADR-0062 S4 pattern) | TypeSpecifyingExtension imports: `is_numeric`/`ctype_*` → StrPreds, `array_is_list` (S4), `count()` (S3/S7), `in_array`, `str_starts_with`, … |

The classification question for any candidate is mechanical: *can the real
PHP answer it on literals* (i) — *can a total rule compute it from argument
structure* (ii) — *is it only measurable, not derivable* (iii) — *does it
require framework boot* (iv) — *is it a narrowing, not a return* (v).

## 2. Priority is measured, not curated

The import queue is ordered by two instruments, not taste: the conformance
table's failing/unenforced rows (which name specific spellings), and
`cargo xtask freq` corpus frequency for builtin call sites. A candidate
absent from both waits. Each (ii)/(iii) addition opens in measurement mode
per the standing protocol; (iii) additions must pass the full admission
gate — a curated row a later PHP minor could falsify is the documented trap
(ADR-0056 §2) and stays refused.

## 3. Declined imports

- A runtime-pluggable DynamicReturnTypeExtension interface in Steins core —
  the five seams cover the behaviors; a sixth open-ended hook would be a
  second extension mechanism competing with ADR-0012 plugins.
- Bulk functionMap transcription — rows enter through (ii)/(iii) with their
  gates, never by mass import (the ADR-0056 subset-hole lesson). *Narrowed
  by ADR-0069: mass import is admitted into the Asserted floor only, never
  into these Verified-lane seams.*

## 4. Slices

| Slice | Content |
| --- | --- |
| DR1 | Candidate census: conformance rows × freq sweep → classified queue (design artifact, no code) |
| DR2 | Seam (v) batch: the guard-vocabulary imports the census ranks first |
| DR3 | Seam (ii) batch: top symbolic transfers (excluding S7's array set, already scheduled) |
| DR4 | Seam (iii) batch: probe-verified curated rows from the census remainder |

DR1 is a half-day design task and gates the rest; DR2–DR4 are ordinary
protocol slices.

## 5. DR1 census outcomes (2026-07-29, amendment)

The census (scratchpad dr_census/DR-CENSUS.md) confirmed the taxonomy with
two recorded exceptions rather than forced fits:

- **By-ref out-param facts fit no seam.** `preg_match`'s real value is the
  `$matches` shape, an out-parameter fact channel — that belongs to
  ADR-0063 P2's `mutate.local` world plus a future out-param fact lane,
  not to any of the five seams. Recorded here so no DR slice force-fits it.
- **`array_replace_recursive`** (surprisingly frequent) exceeds the
  current shape algebra's reach (recursive N-array merging); deferred with
  its frequency noted, not silently dropped.
- Queue headline: the `is_*`/`ctype_*` type-narrowing guard family is
  entirely unported (seam v) and `assert` inherits every addition for
  free; `sprintf` (the #1 builtin by frequency) is already fully served by
  the fold seam — no work fits.
