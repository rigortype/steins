# Syntax tree contract owned by Steins; Mago's parser evaluated behind it

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
