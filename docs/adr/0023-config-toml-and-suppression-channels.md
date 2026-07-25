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

## Amendment (2026-07-26): scoped policy reports reach, not entries (issue #32/#15)

`[[policy]]` is unimplemented (#15), which makes this the moment to fix a
constraint in advance rather than after the format ships. A cross-analyzer
survey of ten open-source PHP projects, run through PHPStan, found the same
"the suppression ledger does not grow" promise holding **vacuously**
wherever a ledger entry had unbounded, unenumerated scope. Two
identifier-keyed `ignoreErrors` ledgers made the point starkly: one project
suppressed **10,379 diagnostics with 16 lines**, another **2,484
diagnostics with two lines** — adding a new occurrence under an
already-listed identifier costs zero new lines, so entry count cannot
distinguish a ledger that stopped growing from one still absorbing
thousands. The first project's own six-week history is the sharper
warning: its entry count fell **54 → 16** while its reach barely moved
(**10,762 → 10,379**), and by every metric defined at the time that read
as exemplary cleanup, not as a ledger about to swallow ten thousand-plus
diagnostics under 16 lines. A message-only entry (a `message:` regex with
no `count:`) has the identical property for a subtler reason — it matches
however many occurrences exist, present or future, with nothing in its own
text bounding that count: 25 such entries measured at reach 366, and over
two months the entry count moved by **1** while the reach moved by **52**.

**The constraint**: a `[[policy]]` scope is not measured, reported, or
reviewed by its entry — the `disable`/`enable` list, the `in` path set,
the `where` matcher — but by its **reach**, the count of diagnostics it
actually suppresses at the point it is evaluated. Any surface that prints
a `[[policy]]` entry (a `steins.toml` listing, a future `doctor` policy
section, a triage view keyed on `reason`) prints the reach beside it,
unconditionally — no config flag may turn the reach column off, on the
same footing as `suppress.unmatched`'s always-on posture (ADR-0050 §1,
amended below). Reach-blind reporting is exactly how the survey's third
failure mode went unnoticed: one project shipped 13 of 25 `ignoreErrors`
entries at reach zero — dead exclusions matching nothing — because the
analyser's own unmatched-reporting flag was off. A `[[policy]]` scope at
reach zero is the same shape: not a quiet success, a dead entry, and
silence about it is the bug.

This binds #15's eventual implementation: the resolved reach of a
`[[policy]]` scope is data the filtering pass itself must produce (which
findings a scope actually removed, not merely which ids/paths it was
configured to consider) — a reach computed any other way than diffing
against the unfiltered run is not honest. `where` narrowing is exactly why
the entry alone cannot substitute: a scope's `disable` list can name a
whole family (`"type.*"`) while `where` narrows it to almost nothing live
in a given tree, and only the reach number tells "broad by design, thin in
practice" apart from "broad and biting."
