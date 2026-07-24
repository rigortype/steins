# PHP;STEINS

A **shameless knockoff** heavily 'inspired' by PHPStan, born from my grand delusions. It is a cursed dead copy designed to ~~destroy your codebase~~ deceive ***the Organization*** and rewrite the worldline of static analysis. *El Psy Kongroo.*

## Docs

- [Quickstart](docs/guide/quickstart.md) — install, first run, exit codes, limits.
- [Handbook](docs/handbook/README.md) — a guided tour of what Steins proves: the guarantee, the type system, narrowing, and effects.
- [Profiles and baseline](docs/guide/profiles-and-baseline.md) — named stages, the baseline ratchet, `steins.toml`.

### Specifications

- [Type specification](docs/type-specification/README.md) — what the analysis *means*: the value domain, acceptance, narrowing, effects, throws, diagnostic policy.
- [Internal specification](docs/internal-spec/README.md) — analyzer-internal contracts: crate topology, syntax tree, trace IR, query graph, sidecar, config, transforms.
- [Not implemented](docs/type-specification/not-implemented.md) — the honest gap list.
- [Roadmap](docs/ROADMAP.md) — milestones, exit criteria, and the refusal list.
