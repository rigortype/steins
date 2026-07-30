# The Plugin Contract

**Status: partly implemented.** The **manifest channel** loads (issue #68): a
Composer package of `type: steins-plugin` registers effect labels and colors
plain functions, read straight from a JSON file with no PHP running. The
**sidecar channel** — the half that boots the project's own autoload and answers
`plugin(id, "declare", …)` — is still the documented stub returning
`{kind: "widen", reason: "unimplemented"}`, and synthetic declarations are
downstream of it. This document records the whole design so a reader can tell
the seam from the feature; the table at the end says which is which.
ADR-0012, ADR-0039. For *effect* facts specifically — which lane they enter,
who may register which label roots, how facts version against the described
package — see ADR-0068.

## Why it matters that this is absent

Two user-visible consequences follow directly, and they are documented elsewhere
as facts rather than as complaints:

1. **Framework knowledge is unavailable.** ADR-0044/0045's packs (Valinor,
   Serde, PSR, PSL) are designed against the sidecar half of this contract, and
   none of them can ship until it exists.
2. **A plugin can name a function, not a shape.** The manifest carries label
   registrations and function colorings only; a plugin still cannot supply a
   type signature for anything.

The consequence that is *no longer* true: ecosystem effect labels can be
registered. `acme.cache` becomes a legal envelope label the moment a plugin under
that vendor root registers it ([`effects.md`](../type-specification/effects.md)).

## The design

**A plugin is a Composer package** (`type: steins-plugin`), loaded **in the
sidecar** via the project's own autoload — the lazy path ADR-0024 already
provides. Discovery is from installed package types plus explicit `steins.toml`
listing, with the explicit listing winning.

The crucial architectural point: a plugin is a **fact producer, not an analysis
participant**. It is written in PHP, hosted in the sidecar, and may boot the real
framework and ask it. It is not part of the inference engine.

### What a plugin supplies (v1 — deliberately narrow)

- **Synthetic declarations** — type signatures for functions, methods, and
  classes, expressed as **PHPDoc type strings**. ADR-0029's grammar doubles as
  the wire format; no new schema is invented.
- **Label registrations** — effect labels and value-provenance labels
  (ADR-0018 / ADR-0038 registries), plus catalog fragments coloring functions.
- **Not diagnostics.** The zero-FP banner cannot vouch for third-party finding
  quality. Plugin-emitted diagnostics arrive in a later version under their own
  registered families (ADR-0022's channel).

### Subscription model

Learned from Mago: the manifest declares which symbols the plugin can speak
about, via exact / prefix / namespace name patterns. Steins queries
`plugin(id, "declare", {symbols})` **on demand** when matching symbols are
encountered — never an upfront universe dump.

Mago's other extension face, AST-event hooks, is deliberately **not** imported:
per-node hooks over IPC would be a hot-path disaster, and our plugins produce
facts rather than participate in the walk.

The distribution trade-off against Mago's compiled-in Rust plugins is recorded
rather than glossed: theirs is fast but closed (third parties must fork or
upstream); ours is open and can boot the real framework, with the cost paid once
per environment fingerprint (`composer.lock` hash + plugin versions) under which
responses are cached — so an LSP session never boots Laravel twice.

### Merge rules

Imported from Rigor:

- **Core and native declarations are authoritative.** Plugins refine, never
  weaken.
- A supplied declaration conflicting with a native type is **rejected and
  recorded** as a plugin inconsistency.
- Supplied declarations enter the [trust
  order](../type-specification/trust-stratification.md) *below* verified PHPDoc,
  as "plugin assertions": when propagation disproves one, the truth keeps
  flowing.

### Versioning

Bundles carry `steins-plugin-api: 1`. An unrecognized newer version is **not
loaded and is reported by name** — silence names itself, the same discipline the
sidecar and the budget cutoffs follow.

## The manifest fast path (what landed first)

The two facts the effect lane needs — *which labels exist* and *which plain
functions carry them* — are static per installed version. Asking the sidecar for
them would make label resolution depend on a working `php`, so they arrive
instead in a file Rust reads directly, `vendor/<name>/steins-plugin.json`:

```json
{
    "steins-plugin-api": 1,
    "labels": ["acme.cache"],
    "effects": { "acme_cache_get": ["acme.cache"] }
}
```

Discovery reads `vendor/composer/installed.json` for `type: steins-plugin`
packages; `steins.toml`'s `[plugins] allow = […]` replaces that list with exactly
the named packages and vouches for them. Refusals — an unsupported
`steins-plugin-api`, a label outside the plugin's vendor root, a coloring naming
a label the plugin never registered — each print one `steins: plugin <name>: …`
line on **stderr** and are never diagnostics.

This is a fast path, not a replacement: it answers nothing that requires running
the framework, which is exactly what the sidecar channel is for.

## What exists today

| Piece | State |
| --- | --- |
| Manifest format (`steins-plugin.json`) | **implemented** — api version, label registrations, function colorings |
| Discovery (`installed.json` + `steins.toml [plugins] allow`) | **implemented** |
| Registry openness | **implemented** — `LabelRegistry` = builtin table + registered extensions; `known_labels()` remains the closed builtin half |
| Label-root ownership (ADR-0068 §2) | **implemented** — vendor-root rule, explicit listing exempt, refusals on stderr |
| Effect facts into the declared lane, taint kept (ADR-0068 §1) | **implemented** |
| `plugin` JSON-RPC method | stub, returns `widen` |
| Synthetic declarations | none |
| Pattern subscriptions | none |
| Method (as opposed to plain function) colorings | none |
| Value-provenance registrations | none |
| Caching by environment fingerprint | none |

Framework support (Laravel first-class, per ADR-0012) is downstream of the
sidecar half and is not started.
