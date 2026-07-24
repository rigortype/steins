# Diagnostic Policy

**Status: partial** — the registry, layers, facets, profiles, inline ignores,
the vendor filter, and the baseline are implemented; scoped policy
(`[paths.sets]` / `[[policy]]`) is designed and absent. ADR-0022, ADR-0023,
ADR-0050.

## The id registry

Diagnostic ids are `family.rule` (`type.argument-mismatch`,
`throw.undeclared`), and the set is **closed**: `DIAGNOSTIC_REGISTRY` pairs each
id with its [layer](overview.md) and is the single source of truth. Ids are
decoupled from emitters — one emitter may produce several ids, and an id's
identity does not change when its emitter is rewritten.

Three lists, bound by a workspace totality test so none can rot silently:

| List | Meaning |
| --- | --- |
| `DIAGNOSTIC_REGISTRY` | every id and its layer |
| `ALL_EMITTABLE_IDS` | every id that reaches a construction site |
| `REGISTERED_NOT_YET_EMITTED` | registered ahead of emission — nameable, unemitted |

The test asserts the registry equals the union of the other two, that the two
are **disjoint**, and that an emitted-but-unregistered id fails the build.
Registering ahead of emission is the deliberate pattern: an id gets its layer
pinned and becomes nameable in `@steins-ignore` and in profiles before its
emitter lands, so nothing breaks when it lights up.

**Prefix semantics** (ADR-0022): a pattern matches an id when it equals it or is
a dot-path ancestor. `type.*` and bare `type` both match
`type.argument-mismatch`. Prefixes work in `@steins-ignore` and in profile
id-arrays.

## Layers and facets

Layers are described in [overview.md](overview.md): `proof`, `contract`,
`mechanics`, `debug`. The layer is a **registry attribute**, not a string
prefix — the prefix spelling (`throw.*`) is a config convenience only, and both
the FP gate and every user-facing surface key on the layer.

A **facet** is an additional registry-declared classification axis a finding
carries, recorded at emit time from walk-local data. v1 declares exactly one:
`origin` (`direct` | `propagated`), carried only by `throw.undeclared`
([throws.md](throws.md)). It is kept a closed enum rather than an open string
key so a second facet is an ADR-forced change, never an ad-hoc addition.

## Profiles

A **profile** is config-resolved *data*: a selection over layers and ids that
decides which post-inference findings a run prints. It is never a change to
inference behavior — the trust-toggle refusal (ADR-0050 §10) holds absolutely.

Built-ins:

| Profile | Surface |
| --- | --- |
| `default` | proof + mechanics |
| `throws-direct` | default, plus `throw.undeclared` where `origin = direct` |
| `contracts` | default, plus the whole contract layer |

`strict` and `boundary` are **reserved** names (ADR-0042): selecting *or*
defining one is a config error until their ADR lands.

User profiles extend a built-in or another user profile:

```toml
[check]
profile = "house"

[profile.house]
extends = "throws-direct"
enable  = ["phpdoc.*"]
disable = ["phpdoc.undefined-method"]
warn    = ["throw.undeclared"]
```

Cycles, unknown names, redefining a built-in, and unknown id patterns are config
errors (exit 2). **Mechanics ids ignore `disable`** — the anti-rot channel
cannot be turned off. Facet selectors in user profiles are deferred with design:
v1 reaches the `origin` facet only through the built-in `throws-direct`, and a
facet-shaped token (`throw.undeclared@direct`) is rejected as an unknown id
pattern.

`warn` demotes an id to **report-without-fail**. Every surfaced finding is
`fail` by default in every layer but `debug` — see below.

## The debug lane (ADR-0053)

The dump surface's contract, decided in full; implementation state at
verification time: the lane, its three registered ids, and the shared
plain-text rendering landed (D1/D2, gate-excluded and byte-identical); the emit
slices (D3 explicit pair, D4 `var_dump`) were in flight.

- **`debug.type` / `debug.phpdoc-type`** (explicit `PHPStan\dumpType` /
  `dumpPhpDocType`, recognized unconditionally by resolved FQN): level
  **fail**, fixed — the call names a function that does not exist at runtime,
  so a committed call is a guaranteed fatal. **Profile-inert**, like
  mechanics: no profile disables or demotes them.
- **`debug.var-dump`** (a call resolving to the *global* `var_dump` under
  PHP's own fallback rule): one report per argument, level **warn**, fixed —
  structurally **exit-neutral forever**; no channel can promote it to fail
  (that would be a lint rule, which Steins refuses). Default-ON in the
  built-in profiles, **disableable** in a named profile.
- All three are **exempt from all three suppression channels** — never
  baselined, never matched by `@steins-ignore` or `[[policy]]`. The remedy
  for an unwanted dump is deleting the call; an ignore naming a debug id
  reports `suppress.unmatched`.
- The layer is **excluded from every fp-gate counter** — a dump is an
  answer, not a finding.

## Composition

The pipeline is fixed and ordered (ADR-0050 §6):

```text
vendor filter → profile surface → [[policy]] → inline ignores → baseline
```

- **Vendor filter** — findings under a `vendor/` path component are suppressed
  unless `--vendor-diagnostics` is passed. The match is on whole path
  components, so `vendor_proj/` and `vendor.php` are not vendor. Vendor trees
  are still *analyzed* as source (ADR-0015) — they are just not reported on.
- **`[[policy]]`** — **designed, not implemented.** The stage exists in the
  pipeline as a no-op with a clear seam.
- **Inline ignores** and **baseline** below.

## The three suppression channels

Each channel has one home and one job (ADR-0023). Per-finding entries never
accumulate in config — that is the root of `ignoreErrors` sprawl, and a zero-FP
tool does not need the compensation.

| Channel | Role | Home | Status |
| --- | --- | --- | --- |
| Baseline | the accumulated past at adoption | `.steins-baseline.jsonl`, machine-managed | implemented |
| Inline ignore | a point exception at the code site | `// @steins-ignore <id> (reason)` | implemented |
| Scoped policy | structural intent ("tests don't need X") | `steins.toml` | **designed, not implemented** |

### Inline ignores

The notation follows `@phpstan-ignore`'s spec verbatim — familiarity over
novelty. A comment **trailing code on a line** suppresses matching findings on
*that* line; a comment **alone on its own line** suppresses findings on the
*next* line.

Ids are registry-governed and prefix-aware. Two always-on mechanics ids keep the
channel from rotting:

- `suppress.unmatched` — an ignore that matches nothing on its target line.
- `suppress.unknown-id` — an unknown or malformed id in an ignore.

Both are **exempt from every suppression channel**: suppressing the suppressor
would be a loop.

### Baseline

`.steins-baseline.jsonl`, machine-managed and line-shift-immune. Entries are
`{id, path, hash}` where the hash is over the id, the relative path, and the
flagged line's text plus its nearest non-empty neighbors — so unrelated edits
elsewhere in the file do not invalidate it, and a change to the flagged line
correctly resurfaces the finding.

The header records the **capture surface** (the profile name and resolved id
set). Two consequences: staleness is computed only over ids inside the current
run's surface (an entry outside it is *dormant*, not stale), and a run whose
surface exceeds the captured one prints a notice — it "drowns loudly", never
silently.

Format details are in
[`docs/internal-spec/baseline.md`](../internal-spec/baseline.md).

## What is deliberately not a suppression mechanism

- **Message-regex matching.** Diagnostic wording is not a contract and keeps
  improving. Ids plus semantic scopes are always the substitute (ADR-0023).
- **Numeric levels.** Profiles are named intent, not a ladder.
- **`ignoreErrors`-style per-finding config.** A proof-layer finding one wants
  to ignore is either a bug in Steins — corpus material — or a rare
  disagreement; neither belongs in a growing config file.

## Output and exit codes

`--format text | json`. In JSON each finding carries `id`, `layer`, `level`,
`path`, `line`, `column`, `message`, plus its facet as an additive key when it
declares one; the document also reports the active `profile` and the
`vendor_suppressed` / `suppressed` / `baselined` counts.

| Exit | Meaning |
| --- | --- |
| 0 | no `fail`-level finding displayed |
| 1 | at least one `fail`-level finding displayed |
| 2 | usage or config error |

`sarif` and `github` formats are designed in ADR-0054 (with format invariance as
the binding rule — a format is a serialization of the displayed surface, never a
second surface) and are **not implemented** — decided out of v0.1.0 by owner.
Neither is `doctor`, the posture report that would answer "what is my coverage,
is the sidecar healthy, what does the catalog know" without running an emitter;
a minimal `doctor` is scoped into v0.1.0, not landed.
