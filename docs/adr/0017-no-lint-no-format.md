# Lint and format are not Steins' business: separate-process backends, never linked

Steins does not ship a linter or a formatter, and does not statically link
Mago's. Three reasons: the fork pin (ADR-0003) exists for *parser stability*
and must stay confined to the syntax crates — bundling lint/format would
freeze users on stale rules and stale formatting from our pin, exactly where
freshness matters; a bundled third-party linter's FP-tolerant culture would
be indistinguishable from Steins output and erode the zero-FP identity
(ADR-0002); and PHP already has mature formatters (php-cs-fixer, ECS, Pint,
Mago) — not our battlefield, the same division of labor as Rector for
migrations (ADR-0010).

Both are instead **separate-process backends**: Steins detects and
orchestrates the project's own configured tools. The one internal need —
styling *generated* code from transforms (e.g. DTO promotion) — is served by
minimal indentation matching plus an optional post-edit hook that runs the
project's formatter over generated regions only; span+splice editing keeps
existing regions byte-identical, so nothing else ever needs formatting.
Lint orchestration likewise: cooperate (avoid double-reporting), never
re-emit another tool's diagnostics as our own.
