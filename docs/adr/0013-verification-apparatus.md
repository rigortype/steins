# Verification apparatus: FP-gate corpus, counterexample constructibility, differential as instrument

Zero-FP is a measured discipline, not a slogan. Four instruments, standing
from the start:

1. **FP-gate corpus** — real composer-world projects (framework-free first,
   per ADR-0012: Composer, PHPUnit, Guzzle, standalone Symfony components,
   monolog, …). One proof-layer diagnostic pointing at working code is a
   release blocker; triage is human, resolution is a fix or a rule
   retraction.
2. **Counterexample constructibility** — Steins has no reference
   implementation to diff against (unlike rigor-rs), but PHP has a real
   engine: proof-layer claims must in principle be reproducible by execution
   in the sidecar, and diagnostics are emitted in a form from which a
   reproduction can be constructed. Claim classes that execution cannot
   reproduce (unreachability/dead code) are individually admitted via an
   audited allowlist with their own static-proof review bar.
3. **PHPStan differential = instrument, not gate** — diagnostic-set agreement
   is a non-goal (ADR-0002), but "PHPStan silent, Steins fires" is a
   high-yield FP screening feed on the CI dashboard.
4. **Private-corpus injection point** — corpus definitions load from outside
   the repo, so non-public codebases can serve as additional FP gates under
   the same discipline.
