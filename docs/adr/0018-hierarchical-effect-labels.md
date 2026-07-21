# Hierarchical effect labels: dot-path strings, prefix subsumption, open registry

Supersedes the enum sketch in ADR-0006/0008. Three pressures killed the enum:
PHP enums are closed-world, so ecosystem and **private effects** could never
be added without a Steins release; the name `Kind` collides with type
theory's kind (type-of-type) in a tool that talks type-system language; and
real consumers need finer granularity than flat colors (the
testability-split transform sends `nondet.time` to a PSR-20 clock but
`nondet.random` to `Random\Randomizer`; architecture policies target
`io.net.http`).

**Design**: an effect's canonical identity is a hierarchical dot-path string
— the **effect label** (the row-polymorphism literature's term). Envelope
checking uses **prefix subsumption**: a declared `io` admits an inferred
`io.net.http` — declarations stay coarse while the catalog stays fine, the
same gradient as annotation restraint. Class constants (`Effects::IO_NET_HTTP`)
are completion/typo sugar; the string is the canon. Typo safety is Steins'
own job: the **label registry** (core taxonomy ∪ plugin-registered labels,
via the ADR-0012 channel) is the set of known labels, and an unregistered
label is a diagnostic.

Core taxonomy (initial): `output`; `io` ⊃ `io.fs.read`, `io.fs.write`,
`io.net` ⊃ `io.net.http`, `io.db`, `io.process`; `global.read`,
`global.write`; `nondet` ⊃ `nondet.random`, `nondet.time`; `exit`; `mutate`.
Redis/APCu and friends are ecosystem labels shipped by plugins, not core —
even their classification judgment (is APCu `io` or `global`?) belongs to
the catalog side.

**Semantic labels** layer above transport labels: a SendGrid SDK call is
both `io.net.http` (transport) and `email.send` (meaning), declared
together. Policies and transforms can target the meaning ("no email from
the domain layer") instead of the plumbing — the private-effect use case
that closed enums made impossible.

`throw` remains outside the label vocabulary entirely (ADR-0006: one color,
one spelling — `@throws`).
