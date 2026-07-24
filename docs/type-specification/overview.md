# Overview

**Status: implemented**, except where marked.

## The core principle: call-site value propagation

Steins analyzes a program by carrying **actual argument values and types from
each call site into the callee body**, flow-sensitively (ADR-0001). This is the
single decision every other one follows from: it is the opposite of the modular
model, where a function is analyzed once against its declared signature and call
sites are checked against that signature alone.

The consequence that matters for users: a literal, a shape, or a proven value
crosses a function boundary **by inference, not by annotation**. Code that has
no docblock at all is still analyzed precisely, as long as its values are
traceable.

```php
function scale(int $n): int { return $n * 2; }
function run(): void { scale("x"); }   // the call site carries "x" into scale()
```

The cost is equally deliberate: propagation is bounded (`MAX_BINDING_DEPTH = 8`
frames of interprocedural binding descent, plus on-stack recursion detection),
and where propagation cannot reach, Steins widens to silence rather than
guessing. A budget cutoff is *silence*, never a manufactured finding.

## The declared type as an authoritative envelope

A declared type — native or PHPDoc — is an **upper bound the analyzer trusts and
refines within**. Call-site precision may tighten inside the envelope; it never
widens beyond it. The trust order is fixed and not configurable: a *proven*
value beats a *declared* type wherever the two disagree (ADR-0037), because the
proven value is what the runtime will actually see.

There is no `treatPhpDocTypesAsCertain`-style toggle, and there never will be
(ADR-0002, ADR-0037). See [trust-stratification.md](trust-stratification.md).

## Two dimensions, not one

Types are the first inferred dimension. **Effects** are the second: what an
expression *does* beyond computing a value — throw, output, filesystem,
network, global state, nondeterminism — inferred and propagated the same way
(ADR-0005). Declarations of effects are envelopes, exactly like types
(ADR-0006), and violating one is a finding.

The throw system is a third, closely related accounting (ADR-0007, ADR-0040):
`@throws` envelopes are checked against provably-escaping checked exceptions.

See [effects.md](effects.md) and [throws.md](throws.md).

## Diagnostic layers

Every diagnostic id carries a **layer** in the registry (ADR-0050 §1). A layer
is a semantic identity — what kind of claim the finding makes — not a severity
grade.

| Layer | Claim | Bar |
| --- | --- | --- |
| `proof` | The program provably breaks on a live path. | Zero false positives. Any FP on corpus code is a release blocker (ADR-0013). |
| `contract` | A proven behavior violates something the code *declares* about itself. The program still works. | True findings legitimately abound in released code; gated as increase tripwires, never on sight. |
| `mechanics` | The analyzer's own hygiene — a finding whose absence would let another channel rot silently. | Red on sight. |
| `debug` | Requested introspection: a report that exists *because a call site asked for it* (`PHPStan\dumpType()`). | Excluded from every gate counter (ADR-0053). Landed in full (D1–D4): the lane, its three ids, the shared rendering, and both emit slices. |

A bare `steins check` surfaces `proof` + `mechanics` only. The contract layer is
reached through a named profile. See [diagnostic-policy.md](diagnostic-policy.md).

## The zero-false-positive bar

The proof layer's discipline, imported from Rigor: *"the program works" outranks
the worst-case static reading*. Concretely:

- A finding fires only on a `Yes` — a proven break. `Maybe` is silence.
- Where analysis cannot decide (unresolvable name, dynamic dispatch, a budget
  cutoff, a missing sidecar), the answer widens to `Maybe`, and `Maybe` does not
  report.
- The bar is verified, not asserted: `cargo xtask fp-gate` runs the proof layer
  over a pinned corpus of real PHP (10 OSS packages plus locally-registered
  projects — a private legacy monorepo and phpstan-src; ~99.3k files at the
  last recorded run) and fails on *any* proof-layer finding that is not a
  triaged, fingerprint-pinned true positive.

The paramount product constraint behind it (the "crying-wolf prohibition"): a
noisy default gets the tool deleted in the first week, so every default is
quiet and noise boundaries move only by explicit opt-in.

## Coverage posture: the sidecar and the sound subset

Steins types literals by **executing the project's own PHP** through a resident
sidecar process (ADR-0004, ADR-0024) — its version, its extensions — so a folded
value is what this code produces on the runtime it actually runs on. Folding is
gated by a purity allowlist (ADR-0008 applied as a hand-picked list; see
[`docs/internal-spec/catalog.md`](../internal-spec/catalog.md)).

Without a sidecar (`--no-php`, or no `php` on `PATH`), the run degrades to the
**sound subset**: still zero-FP, but incomplete — findings that require
executing PHP widen away, and the whole absence family
([dynamism.md](dynamism.md)) goes silent. Incompleteness is never silent about
itself: the run prints a one-line coverage-posture notice.

## What v0.1.0 actually ships

Emitting ids, by layer (the registry is the source of truth —
`steins_infer::DIAGNOSTIC_REGISTRY`):

- **proof** — `type.argument-mismatch`, `type.return-mismatch`,
  `type.property-mismatch`, `call.on-null`, `readonly.reassigned`,
  `call.undefined-method`, `call.undefined-function`, `class.undefined`,
  `call.too-few-arguments`, `call.unknown-named-argument`,
  `offset.missing`, `offset.on-unsupported`.
- **contract** — `phpdoc.param-mismatch`, `phpdoc.return-mismatch`,
  `phpdoc.property-mismatch`, `phpdoc.undefined-method`, `throw.undeclared`,
  `throw.liskov-widened`, `effect.envelope-exceeded`, `effect.liskov-widened`.
- **mechanics** — `suppress.unmatched`, `suppress.unknown-id`,
  `effect.unknown-label` (a typo'd label is apparatus rot, not a contract
  claim).
- **debug** — `debug.type`, `debug.phpdoc-type` (fail-level, profile-inert),
  `debug.var-dump` (warn-level, exit-neutral, disableable) — ADR-0053 D3/D4.

Registered but **not yet emitted** — the registry reserves the id and its layer
so `@steins-ignore` can name it and a profile can select it, but no emitter
produces it (`REGISTERED_NOT_YET_EMITTED`):

| Id | Waiting on |
| --- | --- |
| `call.too-many-arguments` | the sidecar `reflect` slice — the arm fires for *internal* targets only, since userland too-many runs clean and is never a finding |

CLI surface: `check`, `annotate`, `transform`. ADR-0020 declares six commands;
`doctor`, `lsp`, and `mcp` are **designed, not implemented**, and are
deliberately not stubbed (a minimal `doctor` is scoped into v0.1.0 by owner
decision, not landed). Output formats are `text` and `json`; `sarif` and
`github` are designed in ADR-0054 and absent from the binary.

The full gap list is [not-implemented.md](not-implemented.md).

## What Steins deliberately will not do

A specification is also a refusal list. Each of these is anchored in an ADR;
"PHPStan has it" is not a reason.

- **Numeric strictness levels.** Profiles are named intent, not a ladder
  (ADR-0020, ADR-0050).
- **Worst-case `maybe`-reporting.** `maybe` is reported as `maybe` or not at
  all (ADR-0002).
- **Message-regex suppression / `ignoreErrors` sprawl.** Ids, scoped policy, and
  the baseline are the whole surface; message wording is not a contract
  (ADR-0023).
- **Benevolent unions.** The grammar is accepted, the semantics erased;
  failure-arm labels replace the need (ADR-0030 reg. 3, ADR-0042).
- **A call-site template solver.** Where propagation reaches, templates are
  transparent (ADR-0032).
- **Lint and format rules.** Boundary decision (ADR-0017).
- **A PHP-version emulation matrix.** Steins asks the project's real PHP
  (ADR-0004, ADR-0024).
