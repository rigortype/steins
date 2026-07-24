# The Plugin Contract

**Status: designed, not implemented.** Nothing in the workspace loads a plugin.
The only code that exists is the sidecar's `plugin` method, which is a
documented stub returning `{kind: "widen", reason: "unimplemented"}`. This
document records the design so a reader can tell the seam from the feature.
ADR-0012, ADR-0039.

## Why it matters that this is absent

Two user-visible consequences follow directly, and they are documented elsewhere
as facts rather than as complaints:

1. **Ecosystem effect labels cannot be registered.** `io.redis`, `email.send`
   and kin are *correctly* unknown, because the channel that would make them
   known does not exist ([`effects.md`](../type-specification/effects.md)).
2. **Framework knowledge is unavailable.** ADR-0044/0045's packs (Valinor,
   Serde, PSR, PSL) are designed against this contract.

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

## What exists today

| Piece | State |
| --- | --- |
| `plugin` JSON-RPC method | stub, returns `widen` |
| Manifest format | not defined in code |
| Discovery | none |
| Registry openness | `known_labels()` is a closed constant |
| Caching by environment fingerprint | none |

Framework support (Laravel first-class, per ADR-0012) is downstream of all of
the above and is not started.
