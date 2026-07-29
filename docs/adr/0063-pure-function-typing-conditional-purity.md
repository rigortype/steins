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

## Amendment (2026-07-30): P2 + P4 outcomes

**The sister tag is merged, not open.** §1 cites phpdoc-parser #259 as still
open. The copy vendored at `harness/phpdoc-oracle` is 2.3.3 (2026-07-08) and
ships `PureUnlessParameterIsPassedTagValueNode` alongside its callable sibling,
registered under `@pure-unless-parameter-passed` and
`@phpstan-pure-unless-parameter-passed`. Both tags are therefore implemented
from the merged grammar (`parseRequiredVariableName` + optional description, no
type, no `@psalm-` alias) — no ADR-0016 lead was needed, and no spelling was
guessed.

**`preg_replace`'s count is argument 4.** The P2 brief listed it at 3 alongside
`str_replace`. The optional `$limit` sits between subject and count, so
`str_replace`/`str_ireplace` are 3 and `preg_replace`/`preg_replace_callback`
are 4. Rows are transcribed from the stubs, not from the brief.

**Conditionality needed a second catalog axis, not a wider label set.**
`effect_labels` answers per *function*; an out-parameter write is a property of
the *call*. `out_params(name) -> Option<&[usize]>` is the new row, resolved at
each call site against two legs: arity (`arg_count > position`) and target (the
argument's lvalue root, classified in `steins-syntax` as `RefTarget`). The
variadic-by-ref family (`sscanf`, `array_multisort`) is deliberately absent —
its positions are open-ended, and an under-approximated target leg would
downgrade an escaping write to `mutate.local`.

**Target distinction: Steins has it.** Property, static-property, by-ref
parameter, superglobal, and aliased-frame targets are each recognized and never
claim `mutate.local`. What Steins declines is naming *which* escape it is: those
all land on the conservative parent `mutate`, because ADR-0055's
`mutate.self`/`.instance`/`.static` inference (slice E2) does not exist. The
frame-locality claim is additionally gated on the frame carrying no ADR-0001
give-up-list construct (`global`, `static`, `$$v`, `extract`, `$a = &$b`,
`use (&$x)`) — a coarse gate, but each member genuinely defeats "this name is a
frame-private binding".

**The tolerance is universal, not `Pure`-only.** §2.3 states it for `Pure`.
Implemented for every envelope: `Pure` is the tightest one, so tolerating a
label there while rejecting it under a wider declaration would be non-monotone.

**P1's own-color leg is now live.** It was inert because the catalog had no
color for `usort`/`array_walk` to contribute. Their by-ref row supplies one, so
`usort($localRows, $pureCmp)` under `Pure` is clean (tolerated) while
`usort($this->rows, …)` is not.

**P4 lowers to a taint discharge, not a purity override.** A tagged callee's
proven findings still propagate (ADR-0037). What the declaration buys is the
*unknown*: a tagged function's body calls its callable parameter dynamically and
is therefore permanently non-exhaustive, and when every flagged condition is
decided at a call site the contract discharges that taint there. An opaque
callable in the flagged slot decides nothing, so the taint stands.

**Declined this slice.** The tag is honored on free functions only —
`EffectOrigin::MethodCall` records neither arguments nor callbacks, so a method
carrying the tag falls back to the plain edge. Widening the effects pass's
notion of a catalogued builtin (so `preg_match`/`sort` resolve as builtins at
all) was scoped to that pass: the same widening would change how the *throws*
pass classifies those names, which is a real gap and a different baseline.
