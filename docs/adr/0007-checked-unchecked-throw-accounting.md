# Throw-envelope accounting: Error + LogicException unchecked, the rest checked

In `@throws` envelope checking (ADR-0005/0006), the `Error` family and the
`LogicException` family are **unchecked** — they never count against a
declared envelope. `RuntimeException` and all other `Exception` descendants
are **checked**: leaking one past a written `@throws` envelope is a violation.

Rationale: SPL itself documents `LogicException` as "errors that should be
detected at compile time" — Steins *is* the compile time PHP never had, so
these are prey for the proof layer (prove the throwing branch dead via
call-site value propagation), not bookkeeping for envelopes. `Error` likewise
marks engine-level defects the proof layer targets (`TypeError` foremost).
This avoids Java's checked-exception fatigue: a noisy default gets the whole
tool discarded in the first week — the crying-wolf prohibition is
load-bearing here, as everywhere in Steins. The family boundary is preserved
as a config knob; the default stays quiet.
