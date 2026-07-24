# `steins.toml`

**Status: partial** — the keys in the first table are read by the binary; the
second table is designed with no code. ADR-0020, ADR-0023, ADR-0046, ADR-0047,
ADR-0050, ADR-0052.

## Format and stance

TOML, at the repo root, **visible** (not a dotfile). Rust-native parsing means
configuration is readable even in a `--no-php` sound-subset run — a PHP-file
config (Rector style) would structurally conflict with the degradation path of
ADR-0004. A PHP config DSL with transparent caching stays a recorded future
option.

**Every key is optional. Absence means zero-config defaults** — that is the
banner, and it is why there is deliberately no `init` command. Config carries
*intent*, never per-finding accumulation.

The file is located at `--config <path>` for the transform commands, and
otherwise `./steins.toml` if it exists. Unknown top-level sections are ignored so
the file can carry future config; `[runtime]` is the exception — see below.

## Keys the binary reads

### `[check]`

```toml
[check]
profile = "throws-direct"
```

The repo's default profile. The `--profile` flag beats it, which beats the
built-in `default`.

### `[profile.<name>]`

```toml
[profile.house]
extends = "throws-direct"
enable  = ["phpdoc.*"]
disable = ["effect.unknown-label"]
warn    = ["throw.undeclared"]
```

`extends` names a built-in or another user profile (`None` extends `default`).
The three arrays take ADR-0022 prefix id patterns. Errors (all exit 2): a
reserved name (`strict`, `boundary`) selected *or* defined; a built-in
redefined; an unknown name; an `extends` cycle; an id pattern that is not
registry-governed — **including** a facet-shaped token like
`throw.undeclared@direct`, which v1 does not accept in user profiles.

Mechanics ids ignore `disable`.

### `[runtime]`

```toml
[runtime]
zend-assertions = "enabled"    # default: disabled
warning-handler = "abort"      # default: abort
```

Boot-truth pseudo-constants the checker cannot observe from source.

- `zend-assertions = "enabled"` promotes `assert($expr)` narrowing from the
  `Asserted` stratum to `Verified`. Any other value keeps the production default
  (`zend.assertions=-1`).
- `warning-handler` declares what a proven `E_WARNING` *does* at runtime.
  `"abort"` (the default) assumes a handler converts it to an exception or halts,
  so proven warning-grade offset findings emit. `"null"` declares the app
  tolerates the warning, and those findings leave the proof surface.

**This section uses `deny_unknown_fields`.** A misspelled key fails the parse,
deliberately: a silently-ignored `zend-asertions` typo would leave the safe
default in force while the user believed otherwise. What the binary does with
that failure: the parse error is reported loudly and the run proceeds on the
**safe runtime defaults** — the typo can never silently masquerade as the
user's intended override, but it does not abort the run. An unrecognized
*value* on a known key warns and keeps the safe default the same way. Reserved
keys for future pseudo-constants (`include-path`, `sapi`) join here as they
land.

### `[transform.vouch]`

```toml
[transform.vouch]
sites = ["src/Legacy/Loader.php:88"]
```

The ADR-0046 §2 vouching valve. A malformed entry (not `file:line`) is a warning
and is skipped, not a hard error — a vouch typo must not stop a run. A vouch
matching no obstacle is reported as a no-op. A run that vouched anything
**downgrades its completeness claim loudly**. See
[transform-engine.md](transform-engine.md).

### `[transform.partitions]`

```toml
[transform.partitions]
observers = ["tests/**"]

[transform.partitions.sets]
svc-a = ["svc-a.example/**"]
batch = ["batch/**"]
```

The ADR-0047 §7 region map. Glob syntax is the minimal subset the design needs:
`*` matches any run of characters except `/`; `**` spans directories. No `?`, no
character classes, no brace expansion. Patterns and paths use `/`.

Assignment precedence is fixed and deterministic:

1. **Vendor always wins** — a `vendor/` file is `Shared { vendor: true }` even
   if a partition glob covers it.
2. **Observer** — a test inside a service tree is a test, not that service's
   private code.
3. **Partition** — a first-party file matching exactly one partition's globs.
4. **Shared** — every remaining first-party file.

Overlapping partition globs are a **hard config error** (unlike a vouch typo),
computed on each glob's literal segment prefix. A pattern beginning with a
wildcard has an empty prefix and is therefore treated as overlapping every
partition — deliberately conservative.

Slice A wires the map to the planners; no planner *decides* on it yet, so with
one region the behavior is byte-identical to whole-universe.

## Designed, not implemented

### `[paths.sets]` and `[[policy]]` — scoped policy

```toml
[paths.sets]
tests = ["**/*Test.php", "tests/**", "service-a/test/**"]

[[policy]]
disable = ["type.missing-return"]
in      = "@tests"
where   = { method = ["test_*", "dataProvider_*"] }
reason  = "PHPUnit constrains these signatures"
```

The full design (ADR-0023), motivated by a real `phpstan.neon` whose ignore
section repeated the same path list six times and approximated
"PHPUnit-constrained methods" with message regexes:

- **Named path sets** (`@name`) replace YAML anchors and kill list duplication.
- **`disable` takes id arrays** with prefix semantics.
- **Semantic `where` matchers** (`method`, `class`, `extends` globs) target
  structure, not wording.
- **`reason` is a first-class field**, visible to a triage helper.
- **Message-regex matching is deliberately unsupported.** Diagnostic wording is
  not a contract and keeps improving.

Today the pipeline has a no-op `[[policy]]` stage with a clear seam, and
`steins.toml` is parsed only for the sections above.

### Other declared-but-absent keys

Anything ADR-0020 or ADR-0023 mentions beyond the above — exclusion sets for
`check`, per-package vendor budget overrides, catalog paths — is not read.
Unknown sections are ignored, so writing them is harmless and has no effect.
