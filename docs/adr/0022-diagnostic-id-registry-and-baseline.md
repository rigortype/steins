# Diagnostic IDs: emitter-decoupled family.rule registry; JSONL baseline

Diagnostic identifiers reuse ADR-0018's design wholesale — the second
appearance of the open-registry pattern:

- **IDs name the finding, not the finder** (PHPStan 1.11's insight):
  `call.undefined-method` is the semantics; which rule or plugin emitted it —
  and which layer (proof/policy) it belongs to — is structured metadata.
  Many-to-many is allowed from day one, and a rule moving between layers is
  not a BC break.
- **The vocabulary is registry-governed** (Rigor's normative taxonomy):
  kebab-case `family.rule-name`, no numeric codes. Plugins may emit existing
  IDs on exact semantic match, register new rule-names in existing families,
  or register new domain families (`laravel.`) — the same channel as effect
  labels. Suppression works by prefix (`call.*`), mirroring label
  subsumption.

**Baseline** is a separate, machine-managed file in **JSONL**: a metadata
header line (`{"steins-baseline":1,…}`) then one `{"id","path","hash"}`
entry per line — no line numbers; entries are identified by a stable hash of
surrounding code so the baseline doesn't rot on unrelated edits (PHPStan's
known pain). JSONL is jq-native, diff/merge-friendly, and streams at
monorepo scale. Coding agents are not expected to read it directly: analysis
goes through jq or a statistics helper (a `triage`-like surface; command
placement deferred until baseline use exists — see ADR-0020).
