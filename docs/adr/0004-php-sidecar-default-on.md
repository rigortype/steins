# PHP sidecar: default-on, runs the project's own PHP, degrades to a sound subset

Steins types literals by executing real PHP functions over IPC (the rigor-rs
model). We adopt rigor-rs's *final* posture directly, skipping the policy
reversal it went through: the sidecar is **default-on**; a run without it
(`--no-php`, or PHP genuinely unavailable) produces the **sound subset** — the
zero-FP bar holds but findings that require executing PHP widen away — and the
**coverage posture** of every run is surfaced (doctor command, startup notice,
structured-output metadata), so incompleteness is never silent.

The sidecar executes the **project's own PHP** (version, extensions, composer
autoload), not a bundled one: folding must yield "the value this code produces
on the runtime it actually runs on," or zero-FP acquires lies (ICU tables,
version-dependent builtins). Functions whose extension is absent are not
folded — they widen, always the safe side. The sidecar is lazily spawned,
stays resident as a request loop during LSP sessions, and is never used for
syntax (ADR-0003). Folding is gated by a purity allowlist whose design is
shared with the effect system (separate decision).
