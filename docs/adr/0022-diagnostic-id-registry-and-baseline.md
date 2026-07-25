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

## Amendment (2026-07-26): occurrence-keyed entries are load-bearing, not incidental (issue #32)

A ten-project PHPStan survey (the same one behind the ADR-0023
reach-not-entries amendment and the ADR-0050 `suppress.unmatched`
amendment) found ledgers keyed on a bare identifier holding unbounded,
unenumerated scope by construction: one project suppressed **10,379
diagnostics with 16 `ignoreErrors` lines**, another **2,484 with two
lines** — a single line's scope was "every occurrence of this id,
anywhere, forever," so a growing count of live diagnostics under that id
never touched the ledger's size. This baseline's `{"id","path","hash"}`
shape structurally cannot hold that entry. Scope is the *conjunction* of
all three fields: one id, at one path, at one hashed neighborhood of code.
There is no field to widen past that — no identifier-only entry, no
path-glob entry, no message-pattern entry — so a new occurrence of an
already-baselined id at a new site costs a new entry with a new hash,
always, one line per occurrence.

This is why a "the baseline does not grow" promise made against this
format holds **contentfully**, never vacuously (contrast the survey's
54 → 16 entries / 10,762 → 10,379 reach split, where it held vacuously for
six weeks and read as exemplary the whole time): if this baseline's entry
count stays flat, it is because occurrences stopped appearing, because the
format has no other way to stay flat.

The invariant this binds going forward: **no future baseline-format
addition may let one entry cover more than the single occurrence it was
captured from.** An id-only bulk-ignore, a path-prefix entry, or a
`count`-style aggregate would each reintroduce, inside the one channel
proven immune to it, exactly the hazard the survey measured — and each
must be refused on that ground alone, not re-litigated as a convenience
trade-off. The format's narrowness is the safety property.
