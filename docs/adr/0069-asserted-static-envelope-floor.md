# A builtin's declared return type gets an Asserted floor, imported by name and firewalled by grade

Without a live engine, a builtin call with variable operands types as
`unknown`: every rung of the return ladder is engine-gated, and ADR-0061's
ladder deliberately ends at `no sidecar → nothing`. This ADR raises that
floor with a static declared-envelope table whose data lineage is PHPStan's
functionMap — itself inherited from Phan — under an owner decision
(2026-07-31, issue #73) that consciously narrows the wholesale-import
refusals of ADR-0056 §6 and ADR-0064 §3. Status: PENDING ratification.

## 1. Context: the designed hole, and why it is now worth filling

`str_repeat($str, $times)` is `string` with the sidecar and `unknown`
without it. The gap is not an accident: ADR-0056 grounded builtin return
knowledge in what the engine that will run the code says at analysis time,
and refused the alternative — a hand-maintained signature map — as the rot
ADR-0014 warns about. The 13 curated rows cannot serve offline either; they
are refinements, structurally in need of a reflected envelope to subsume.

Two things changed. Issue #64 made the engine-present path reach the
browser, so the engine-absent population shrank to `--no-php`, the pre-boot
playground, and spawn failure past the respawn cap — exactly the places
where a *less precise but honest* answer beats silence. And the survey for
issue #73 established that the refusals' rationale is specific: bulk rows
rot **silently** when they carry authority they cannot re-earn. A floor
that never carries authority is outside the rationale's reach.

## 2. Decision: import into the Asserted lane, never the Verified one

The refusal stands, narrowed: no functionMap row enters the Verified lane,
ever. The import lands as a new bottom rung of the return ladder with the
grade of tool-shipped phpdoc:

- **Asserted, not Verified.** The floor seeds the dump surface and
  contracts-tier reasoning. It is never a proof-layer premise; the zero-FP
  default surface cannot cite it by construction.
- **Any engine answer wins — per name, not per run.** The rung sits
  strictly below `builtin_return_fact` and fires exactly where the folder
  yielded `None` for the asked name. `--no-php` is only the total case;
  with a live engine the floor still speaks where the engine is silent —
  a name whose extension is not loaded on the analyzing PHP
  (`mysqli_fetch_field` without mysqli), or a name with no declared
  return type in reflection. An absence-family finding may stand beside
  such a floor fact, and the pair is complementary, not contradictory:
  the call fails on the analyzing PHP, and if the runtime has it, this
  is its declared shape. Where the engine *answers*, the floor never
  overrides it — the consuming engine may not be the pinned one, and a
  static row must not outvote the real thing.
- **Return envelopes only.** Declared types, lowered through the same
  `lower_str` → `envelope_fact` seam the reflected envelope uses — one
  lowering, two provenances. No param types, no arity, no curated-grade
  refinement enters by this road.
- **The absence family never consumes it.** Existence is a boot-surface
  fact; a table answering `function_exists` is a false-absence FP factory
  (the php-wasm spike's missing-mbstring lesson, in static form).

This is the ADR-0067 shape applied to return types: a declared lane beside
the proven one, legible as such, never laundered into proof.

## 3. Decision: rot is answered by machinery, not diligence

ADR-0014's concern — a signature map that silently drifts from the engine —
is met structurally, the way `hierarchy.toml` already meets it:

- The source is **pinned**: one phpstan-src commit, mined into a committed
  TOML under `docs/research/`, regenerated only alongside `PINNED_PHP`
  bumps.
- Every candidate row is **cross-checked at generation time against the
  pinned engine's reflection** via the real sidecar. A row where
  functionMap and php 8.5 disagree is excluded and listed in the generated
  file with the disagreement verbatim. The per-row evidence bar of
  ADR-0056 is thereby automated, not waived.
- **Version discipline is A11-shaped.** The functionMap delta files are the
  change oracle: a function whose signature moved across the supported
  minors declines when the project's declared PhpTarget straddles the
  change; an unknown target admits, because the row is Asserted anyway and
  its consumers tolerate that grade.

## 4. Decision: the lineage is named where license law puts it

PHPStan's `resources/functionMap.php` opens by naming its own inheritance:
copied from Phan's `src/Phan/Language/Internal/FunctionSignatureMap.php`,
Copyright (c) 2015 Rasmus Lerdorf, Copyright (c) 2015 Andrew Morrison, MIT.
phpstan-src itself is MIT, Copyright (c) Ondřej Mirtes and contributors.

Steins reproduces the chain in a root `NOTICE` file — Steins ← phpstan-src
← Phan — with both MIT permission notices, and the generated table carries
a provenance header naming the pinned commit. `THIRD-PARTY-LICENSES.md`
is untouched: it is generated from the cargo dependency graph and this
data never enters that graph. The playground attribution precedent
(php-wasm, PHP License 3.01) already established that non-cargo lineage
lives beside the thing it licenses.

## 5. Consequences

- `--no-php` and the pre-boot browser gain declared types for builtin
  calls with variable operands; their notices say the types come from the
  catalog's declarations, unverified.
- Where the engine answers a name, behavior is unchanged. Where it is
  silent — engine absent, extension unloaded, no declared type — the
  floor now speaks, Asserted.
- The first slice is deliberately narrower than the source. It takes
  plain functions only (method rows like `Phar::getSignature` are
  skipped) and only rows whose type lowers to an envelope (base / `?T`);
  the rows where functionMap genuinely exceeds reflection — shaped
  arrays, `T|false` unions — are dropped at generation and counted, and
  await a contracts-grade Asserted slice that seeds through the full
  `lower_str` lowering rather than `envelope_fact`.
- A new failure mode exists and is accepted: a floor row can be wrong for
  the user's actual runtime (a patched PHP, an exotic build). It can
  mislead a dump or a contracts-tier fact; it cannot mint a proof-layer
  finding. That asymmetry is the entire design.
- ADR-0056 §6 and ADR-0064 §3 carry narrowed-by notes pointing here; their
  refusals remain in force for the Verified lane.

## Amendment (2026-07-31): the ladder is PHPStan's extension stack, graded

Owner review surfaced the correspondence this floor completes. PHPStan's
dynamic return extensions receive a constant **or a union of constants**,
call the real function per member, and compose the results; an extension
that cannot meet its condition returns `null` and PHPStan falls back to
the next provider, ultimately the functionMap signature. Steins' return
ladder is the same stack with grades made explicit:

| PHPStan | Steins | grade |
|---|---|---|
| extension, constant args, calls the real thing | fold lane (sidecar) | Verified |
| argument-dependent extension | DR3 dispatch + shape transfers (ADR-0061/0062/0064) | reflection-checked |
| extension returns `null` → next provider | rung returns `None` → next rung | — |
| functionMap fallback | this ADR's Asserted floor | Asserted |

Two consequences are recorded rather than implied. First, a future
extension-porting layer is coverage growth of the DR3 rung under the
ADR-0064 taxonomy, not a new mechanism (issue #75); a ported extension
whose essence is a value question routes through the fold lane, never a
Rust reimplementation. Second, the one condition Steins cannot yet meet
is the **union of constants**: the fold gate admits a single constant
tuple only. Member-wise engine calls over a bounded product, composed to
a union and declining on any widened member, are issue #74 — they ride
the existing fold memo, the width gate, and the #64 replay loop without
new wire machinery.
