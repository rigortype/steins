# Syntax tree contract owned by Steins; Mago's parser evaluated behind it

> **Spike verdict (2026-07-22): ADOPTED**, with four conditions — see
> [spike/mago-spike/REPORT.md](../../spike/mago-spike/REPORT.md).
> 4,433/4,433 real vendor files parse clean at 27 MB/s; all bytes are
> recoverable (punctuation lives in struct `Span` fields, not `children()`),
> so the Steins contract is **span+splice based**, not tree-rendering based.
> Error recovery is excellent for local damage (99% tree survival on a
> deleted `;`) but coarse for unclosed delimiters (enclosing declaration
> dropped) — absorbed by a **last-good-tree policy** in the LSP layer and
> targeted as an upstream contribution. Conditions: span+splice contract,
> pin a fork, last-good-tree, upstream recovery improvements.

Refactoring (Rector-class rewriting) and LSP are premises, not add-ons, so the
syntax layer must satisfy three requirements PHPStan never needed: (1) lossless
— byte-identical round-trip including comments and trivia; (2) error-tolerant —
incomplete code still yields an analyzable tree; (3) ideally incremental
reparse. PHP has no official parser consumable from Rust (unlike ruby-prism
for rigor-rs), so we mirror rigor-rs's Rubydex posture: **Steins owns the
syntax-tree contract (trait)**, and Mago's parser is evaluated behind it in a
spike, gated on lossless round-trip and error tolerance against a real-code
corpus. If it fails, fall back to an own rowan-based (red-green) CST; a
from-scratch parser is the last resort given PHP's grammar size. Parsing stays
in Rust — no IPC on the LSP hot path (the PHP sidecar, decided separately, is
for semantics, never syntax).
