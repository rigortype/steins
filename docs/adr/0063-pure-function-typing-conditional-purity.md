# Pure-function typing: semantic effect propagation first, conditional-purity contracts second

Owner directive (2026-07-29): organize and advance pure-function typing.
Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode). Context sources: the effect system (ADR-0018
labels, ADR-0055 class-level purity and the `Impure = ⊤` meet rule), and the
maintainer-side study note
[20260703-effect-system-design.md](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260703-effect-system-design.md),
whose upstream conclusion governs the import: metadata-only purity flags are
a *lie* (rejected twice in phpstan-src, #5580/#5912); the endorsed direction
is **declarative conditional purity** (`@pure-unless-callable-is-impure`,
phpdoc-parser #253 syntax MERGED; sister `@pure-unless-parameter-passed`,
phpdoc-parser #259 open).

## 1. The two problems, in Steins terms

1. **Higher-order purity is polymorphic**: `array_map`'s effect is its
   callback's effect. PHPStan needs an annotation because its analysis is
   modular; Steins descends into visible callbacks (closure wave,
   invocation-shape callbacks) by call-site value propagation — the
   polymorphism is *observable*, not declarable.
2. **By-ref out-params poison purity**: `preg_match($p, $s, $matches)` into
   a local is pure in every useful sense; a flat impure-call verdict is the
   crying-wolf pattern 狼少年撲滅 exists to kill.

## 2. Decisions

1. **Semantic first.** The effect of a higher-order builtin call is
   `builtin's own color ⊔ (join of its immediately-invoked callback
   arguments' envelopes)`. A **callback-position catalog** (which argument
   positions of which builtins are immediately invoked — array_map/filter/
   reduce/walk, usort family, preg_replace_callback, …) drives the join
   through the existing via-provenance fixpoint. No annotation is consulted
   when the callback body is visible. This is the differentiator: where
   PHPStan's endorsed fix is a contract, Steins' first answer is inference.
2. **Contracts for opaque callables.** Where the callback is an opaque
   `callable` parameter, the *declared* conditional purity carries:
   recognize `@pure-unless-callable-is-impure` (upstream-merged syntax) and
   lower it to "this function's envelope = join of the flagged callable
   params' envelopes". Recognize the by-ref sister the same way when
   spelled. BC-free proving ground (ADR-0016): we honor the spelling before
   PHPStan ships its own consumer.
3. **`mutate.local` color.** New effect label for by-ref writes that land
   in caller-local bindings (`preg_match` `$matches`, `str_replace` count).
   Colored-builtin rows for out-param builtins become conditional: the
   label attaches only when the by-ref argument is actually passed, and it
   is **tolerated by Pure envelopes** (a pure function may scribble on its
   own locals; the same call writing a property/static is the existing
   `mutate.instance`/`global` world and stays forbidden). This encodes
   upstream #11884's wish — "conditional on the argument, not a per-function
   lie" — as a color, not a flag.
4. **`pure-callable` / `pure-closure` are enforceable spellings.** Lower to
   `CallableTy` + a pure-envelope obligation on the bound argument: a
   closure argument judged against `pure-callable` must have inferred
   envelope ⊑ Pure (with the `mutate.local` tolerance above; `static
   closure`'s binding constraint is a separate mechanical check). This
   closes the conformance rows (`fallback_pure_callable`, `pure_closure`,
   `static_closure`, `static_pure_closure`) with real enforcement, not
   curation.

## 3. Declined imports

- `hasSideEffects=false` metadata blanket for higher-order builtins — the
  rejected-upstream lie; our catalog rows stay conditional.
- PHPStan's dead `ImpurePointIdentifier` color taxonomy as-is — Steins'
  hierarchical dot-path labels (ADR-0018) already subsume it; no second
  vocabulary.

## 4. Slices

| Slice | Content | Instrument |
| --- | --- | --- |
| P1 | Callback-position catalog + higher-order effect join (semantic leg) | effect fixtures; corpus effect counts |
| P2 | `mutate.local` label + conditional out-param rows + Pure tolerance | preg_match/str_replace fixtures |
| P3 | `pure-callable`/`pure-closure`/`static closure` enforcement | conformance T-rows flip to enforced |
| P4 | `@pure-unless-callable-is-impure` (+ by-ref sister) lowering | nsrt; conformance |

P3 overlaps the conformance C-phase (it *is* four of its rows); sequence P3
inside whichever phase opens first.
