# PHP;STEINS

A **shameless knockoff** heavily 'inspired' by PHPStan, born from my grand delusions. It is a cursed dead copy designed to ~~destroy your codebase~~ deceive ***the Organization*** and rewrite the worldline of static analysis. *El Psy Kongroo.*

## Install

```
composer require --dev rigortype/steins
```

Composer installs a PHP shim; on first use it downloads the release binary
matching the installed version, verifies its published sha256, and runs it.
Pinning the analyzer in `composer.lock` beside the code it analyzes is the
point — CI and every developer then resolve the same one. Requires PHP 8.1+.

That one command also brings in
[`rigortype/steins-attributes`](https://github.com/rigortype/steins-attributes)
— the `#[\Steins\Pure]` and `#[\Steins\Effect]` classes you write in your own
source. The analyzer is Apache-2.0; the attributes are MIT and inert at runtime.

Machine-wide instead of per-project:

```
brew install rigortype/tap/steins
```

Prebuilt binaries for Linux (glibc `x86_64`/`aarch64`, static musl `x86_64`) and
macOS (`x86_64`/`aarch64`) are on the [releases page](https://github.com/rigortype/steins/releases);
Windows is not shipped yet. For a platform with no prebuilt binary, build from
source with `cargo install --git https://github.com/rigortype/steins steins-cli`.
[Quickstart](docs/guide/quickstart.md) covers which channel fits what.

Confirm an install with `steins doctor --no-php`, which runs no checks.

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

This software is licensed under the [Apache License, Version 2.0](LICENSE).

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

The `#[\Steins\Pure]` and `#[\Steins\Effect]` attributes are a separate package
under a separate licence — see
[rigortype/steins-attributes](https://github.com/rigortype/steins-attributes).
