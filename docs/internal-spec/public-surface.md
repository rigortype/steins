# The Public Surface

**Status: implemented as policy** (pre-1.0). ADR-0020, ADR-0022, ADR-0023,
ADR-0025.

## There is no stable Rust API

Every crate in this workspace is **internal**. `pub` in `steins-domain`,
`steins-infer`, `steins-edit`, and the rest means "visible to the workspace",
not "supported for external use". Type names, module paths, and function
signatures change without notice, and no deprecation cycle applies to them.

The crates are not published. If they ever are, that decision comes with its own
ADR and its own compatibility statement.

This is why every Rust identifier in this documentation set is introduced as
"the implementing type" — the name is a pointer into the code, not a contract.

## What *is* the compatibility surface

For a user of the tool, the things that must not break silently:

| Surface | Contract | Home |
| --- | --- | --- |
| **Diagnostic ids** | closed, registry-governed, layer-pinned. An id's identity survives emitter rewrites. | [diagnostic-shape.md](diagnostic-shape.md) |
| **Exit codes** | `0` clean, `1` fail-level finding displayed, `2` usage/config error | [diagnostic-shape.md](diagnostic-shape.md) |
| **`--format json` document shape** | fields are **additive**; a new field may appear, existing ones keep their meaning | [diagnostic-shape.md](diagnostic-shape.md) |
| **`steins.toml` keys** | every key optional; unknown sections ignored (`[runtime]` excepted — see below) | [config.md](config.md) |
| **`.steins-baseline.jsonl`** | versioned header (`"steins-baseline": 1`), stable hash derivation | [baseline.md](baseline.md) |
| **`@steins-ignore`** | follows `@phpstan-ignore`'s spec verbatim | [diagnostic-policy.md](../type-specification/diagnostic-policy.md) |
| **`EditPlan` JSON** | the dry-run → diff → approve currency | [transform-engine.md](transform-engine.md) |

Two deliberate non-contracts:

- **Diagnostic messages are prose.** They keep improving and are explicitly not
  a suppression key (ADR-0023). Nothing may key on wording.
- **The `[runtime]` config section rejects unknown keys.** Ignoring a misspelled
  `zend-asertions` would leave the safe default in force while the user believed
  otherwise, so a typo there is a **hard config error: the run aborts with exit 2**
  (like any other unparseable `steins.toml`) rather than proceeding on defaults —
  the one place where silent leniency is the wrong default.

## Versioning posture

Pre-1.0, and the roadmap's release order is binding: the checker releases first;
LSP and editing release after it. Within that, the id registry's
register-ahead-of-emission pattern is the stability mechanism — an id gets its
name and layer pinned *before* anything emits it, so a project's config and
ignores do not break when the emitter lands.

## Licensing and repository boundaries

Recorded here because they gate what "public" can even mean:

- The core is AGPL today; relicensing to Apache-2.0 / MPL-2.0 is an open
  decision to be settled before the first external contribution (ADR-0025,
  roadmap gate G3, with a DCO/CLA as the recorded fallback if contributions
  arrive first).
- The attribute vocabulary package (`Steins\Pure`, `Steins\Effect`) is intended
  to be MIT and separately distributed — a project that annotates its code must
  not thereby take on the core's license.
- Public repository creation is roadmap gate G2.

Until those resolve, this documentation set describes an internal tree.
