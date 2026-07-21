# The four oxidized PHP checkers: Steins vs Mago / Pzoom / Mir

Survey date: 2026-07-22. Steins is the fourth Rust PHP type checker; this note
places ADR 0001–0015 against the three incumbents, from local checkouts of
[Mago](https://github.com/carthage-software/mago) 1.44.0 (~300k+ LOC, 2988
commits), [Pzoom](https://github.com/muglug/pzoom) (~154k LOC, Psalm port by
Psalm's author), and [Mir](https://github.com/jorgsowa/mir) 0.60.0 (~83k LOC),
plus [From Psalm to Pzoom](https://mattbrown.dev/articles/from-psalm-to-pzoom).

## Identity in one line each

- **Mago** — a toolchain (linter 199 rules / formatter / analyzer / guard),
  Clippy+OXC+Hakana lineage; modular analyzer with Psalm-style types.
- **Pzoom** — a deliberate **port of Psalm** ("a port of Psalm"; parity
  CI-gated, Hakana as second reference; "not something I ever intend to
  support — Caveat Emptor").
- **Mir** — Psalm-*inspired* analyzer, salsa-based, Psalm-compatible
  config/baseline/suppressions; "expect false positives and rough edges."
- **Steins** — value-precise zero-FP bug finder + effect system + LSP/refactor
  premise; PHPStan-modeled in ambition, Rigor-modeled in discipline. Not a
  port of anything, and the only one with no oracle to diff against
  (hence ADR-0013's apparatus).

## Axis by axis

| Axis | Mago | Pzoom | Mir | Steins |
|---|---|---|---|---|
| Analysis model | Modular per-symbol; template substitution at call sites | Modular (Psalm's model, faithfully) | Modular per-file; **demand-driven inferred return types**; per-function memoized inference = prototype, free functions only | **Call-site value propagation** (ADR-0001) — alone here |
| FP posture | FP-conscious knobs, linter culture, Note/Help/Warning/Error | Psalm parity = Psalm's FP profile; README notes Mago FPs on ~⅓ of happy-path tests | Explicit "expect false positives"; error levels 1–8 | **Zero-FP bar, no levels** (ADR-0002) — alone here |
| Parser | **Own arena CST: full source + trivia side-table, error-tolerant** (no incremental reparse) | Mago parser, **pinned fork** (`muglug/mago-clone`); parse errors deliberately unsurfaced (mago mis-recovery → FPs) | php-parser-rs family, normalized owned AST | Contract-owned; **Mago spike** (ADR-0003) |
| Executes PHP? | Never (own hand-written stub prelude, 138 ext files, bincode-embedded) | **Never** ("can't execute PHP…"); framework = offline PHP-side StubProvider (boots Laravel, emits stubs pre-run) | Never (phpstorm-stubs vendored + hand-written Rust overrides) | **Live sidecar, default-on** (ADR-0004) — alone here |
| Effects / purity | Psalm-style flags (`PURE`, `MUTATION_FREE`); **`check_throws` + configurable `unchecked_exceptions`** | Purity issues ported; `@throws` parsed, **not enforced** | Purity issues + `MissingThrowsDocblock` | **Inferred effect dimension, lattice, attribute envelopes** (ADR-0005–0008) — alone here |
| Incremental | Hand-rolled: content-hash + AST fingerprint + targeted repopulation (the path ADR-0009 rejects) | None — cache & AST-differ are **explicit "NOT YET IMPLEMENTED" stubs** | **salsa 0.28**, demand-driven queries | salsa-style from first commit (ADR-0009) |
| LSP | **None shipped** ("for watch mode or LSP integration" — aspirational) | **None** | **None** (infra exists, server out-of-tree, unshipped) | Premise, not add-on |
| Refactoring | Lint autofixes + formatter only | None | None | Transform engine + fix-its, effect preconditions (ADR-0010) |
| PHP range | 7.0–8.6 | ~7.0–8.5 (Psalm CallMap deltas) | 7.4–8.5 | **8.1+ only** (ADR-0011) |
| Framework story | 4 compiled-in Rust plugins (psl, flow-php, psr-container, stdlib) | PHP-side stub generation (pzoom-laravel) | Psalm-plugin subprocess bridge + Rust plugins | Sidecar-hosted PHP plugins that boot the real framework (ADR-0012) |

## What the incumbents validate

- **ADR-0003 (Mago spike)** — de-risked: Mago's CST keeps full source bytes +
  complete trivia and powers a production formatter; error tolerance is real.
  Two caveats for the spike: trivia lives in a side-sequence (not interleaved
  green nodes) — check ergonomics for rewriting; and Pzoom both **pins a
  fork** (mago moves fast; a 1.30 parser-API migration hit them) and refuses
  to surface mago parse errors because its error *recovery* mis-parses some
  constructs — our spike must test recovery quality, not just error presence.
- **ADR-0009 (salsa)** — Mir proves salsa works for PHP analysis at 83k LOC
  and ~2k commits. Its pain points preview ours: mutual recursion broken by a
  thread-local guard returning `mixed` to force a fixpoint, and per-function
  memoized inference still prototype-grade. Budget + Certainty discipline
  (name the cutoff) is our planned answer to exactly that class of problem.
- **ADR-0007 (checked/unchecked)** — Mago independently arrived at
  `check_throws` + a configurable `unchecked_exceptions` set. Same shape as
  our Error+LogicException default; ours adds the SPL-hierarchy default and
  proof-layer story.
- **"LSP retrofits fail" (ADR-0009 rationale)** — now 6-for-6: PHPStan,
  Psalm, Rector, Mago, Pzoom, Mir — none ships an LSP. The niche is
  empirically open, and empirically hard.

## Where Steins is deliberately alone

1. **Call-site value propagation.** All three incumbents are modular; Mir's
   half-step (demand-driven return types) is the closest anyone gets, and its
   recursion hack shows the dragons. This is our hardest bet and our
   annotation-restraint differentiator; nobody else can kill `array{...}`
   sprawl because nobody else lets shapes cross boundaries by inference.
2. **Zero-FP as identity.** The incumbents inherit Psalm's/linter FP culture
   (Mir says "expect false positives" out loud). Pzoom's own README
   criticizing Mago's FP rate confirms FP discipline is a live axis of
   competition — no Rust tool occupies the zero-FP position.
3. **Executing real PHP.** Everyone else chose fully-static (it buys them
   wasm playgrounds and zero-dependency distribution). Matt Brown states the
   consequence himself: the Rust speedup "matters less for PHP because modern
   PHP heavily relies on runtime magic that static analysis cannot observe,"
   and Pzoom's plugin system is limited because "there's no scan-time
   execution of PHP scripts allowed." That sentence is the argument *for*
   ADR-0004: the sidecar is how Steins observes what they structurally
   cannot. Pzoom's StubProvider (boot Laravel offline, emit stubs) is a
   one-shot batch cousin of our resident sidecar — same instinct, weaker
   form.
4. **Effects as an inferred dimension** and **refactoring as a product
   surface** — no incumbent has either beyond Psalm-style purity flags and
   lint autofixes.

## What the Pzoom article warns us about

Pzoom is the *easier* road — a faithful port, by the original author, with
Psalm's 6,722-case test suite as oracle and Hakana as a second map — and it
still took ~100 hours, ~$2,000 of tokens, and lands at a self-measured
26.7/100 real-world parity score (99.9% of ported tests pass; parity ≠ test
pass rate). Steins has no oracle, a novel analysis model, and three
subsystems (CST rewriting, salsa engine, sidecar) none of which the port
needed. Consequences we have already drawn, now reinforced:

- ADR-0013's verification apparatus is not optional — it is the substitute
  for the oracle Pzoom had for free.
- ADR-0011/0012's scope cuts (8.1+, frameworks deferred) are survival, not
  taste.
- Brown's adoption humility ("unless developers really need the speedup and
  can maintain it themselves…") is the market's honest signal: a 4th checker
  justifies its existence only by doing what the other three structurally
  cannot — which is precisely the set of positions where Steins is alone.
