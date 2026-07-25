# PHP;STEINS

A **shameless knockoff** heavily 'inspired' by PHPStan, born from my grand delusions. It is a cursed dead copy designed to ~~destroy your codebase~~ deceive ***the Organization*** and rewrite the worldline of static analysis. *El Psy Kongroo.*

## Install

```
composer require --dev typedduck/steins
```

Composer installs a PHP shim that fetches the matching release binary on first use, verifies its published sha256, and runs it — so the analyzer pins in `composer.lock` beside the code it analyzes.

Or `brew install rigortype/tap/steins`, a [prebuilt binary](https://github.com/rigortype/steins/releases), or `cargo install --git https://github.com/rigortype/steins steins-cli`. [Quickstart](docs/guide/quickstart.md) covers which fits what, and `steins doctor --no-php` confirms any of them.

## Docs

- [Quickstart](docs/guide/quickstart.md) — install, first run, exit codes, limits.
- [Handbook](docs/handbook/README.md) — a guided tour of what Steins proves: the guarantee, the type system, narrowing, and effects.
- [Profiles and baseline](docs/guide/profiles-and-baseline.md) — named stages, the baseline ratchet, `steins.toml`.

### Specifications

- [Type specification](docs/type-specification/README.md) — what the analysis *means*: the value domain, acceptance, narrowing, effects, throws, diagnostic policy.
- [Internal specification](docs/internal-spec/README.md) — analyzer-internal contracts: crate topology, syntax tree, trace IR, query graph, sidecar, config, transforms.
- [Not implemented](docs/type-specification/not-implemented.md) — the honest gap list.
- [Roadmap](docs/ROADMAP.md) — milestones, exit criteria, and the refusal list.

## Copyright

Apache-2.0 ([LICENSE](LICENSE)). The `#[\Steins\Pure]` and `#[\Steins\Effect]` attributes are a separate MIT package, [rigortype/steins-attributes](https://github.com/rigortype/steins-attributes).

```
Copyright 2026 TypedDuck, USAMI Kenta <tadsan@zonu.me>

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```
