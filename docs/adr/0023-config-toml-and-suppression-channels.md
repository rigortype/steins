# Config: steins.toml carries intent; suppression splits into three channels

**Format**: TOML (`steins.toml`, repo root, visible). Rust-native parsing
means configuration is readable even in `--no-php` sound-subset runs — a
PHP-file config (Rector style) would structurally conflict with ADR-0004's
degradation path. A PHP config DSL with transparent caching remains a
recorded future option, deferred. Every key is optional; absence means
zero-config defaults (ADR-0020).

**Three suppression channels**, each with its own home:

| Channel | Role | Home |
|---|---|---|
| Baseline | the accumulated past at adoption | `.steins-baseline.jsonl` (machine-managed, ADR-0022) |
| Inline ignore | a point exception at the code site | `// @steins-ignore <id> (reason)` |
| Scoped policy | structural intent ("tests don't need X") | `steins.toml` |

Per-finding entries never accumulate in config: a proof-layer finding one
wants to ignore is either an FP (our bug — corpus material) or a rare
disagreement, and policy-layer noise is governed by profiles and scoped
policy. This severs the root of PHPStan's `ignoreErrors` sprawl, which is a
compensation mechanism a zero-FP tool does not need.

**Inline ignore follows `@phpstan-ignore`'s spec verbatim** (notation,
same-line/next-line placement, parenthesized optional comment) rather than
inventing a new form — familiarity over novelty, per the flow-back stance
(ADR-0016). An ignore whose diagnostic does not occur is itself a warning
(`suppress.unmatched`), the anti-rot mechanism; teams wanting mandatory
reasons opt in via a policy rule (`suppress.missing-reason`).

**Scoped policy ergonomics** (motivated by a real-world phpstan.neon whose
ignore section repeated the same path list six times and approximated
"PHPUnit-constrained methods" with message regexes — anonymized):

```toml
[paths.sets]
tests = ["**/*Test.php", "tests/**", "service-a/test/**", "service-b/test/**"]

[[policy]]
disable = ["type.missing-return", "type.missing-iterable-value"]
in      = "@tests"
where   = { method = ["test_*", "dataProvider_*"] }
reason  = "PHPUnit constrains these signatures"
```

- **Named path sets** (`[paths.sets]`, referenced as `@name`) replace YAML
  anchors and kill list duplication.
- **`disable` takes ID arrays** with ADR-0022 prefix semantics (`"type.*"`).
- **Semantic `where`** matchers (`method`/`class`/`extends` globs) replace
  message regexes: target structure, not wording.
- **Message-regex matching is deliberately unsupported** — diagnostic
  wording is not a contract and keeps improving; IDs + semantic scopes are
  always the substitute. This severs the coupling that complicates both
  PHPStan and Mago configs.
- **`reason` is a first-class field**, visible to the triage helper.
