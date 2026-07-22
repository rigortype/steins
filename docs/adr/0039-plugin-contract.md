# Plugin contract: composer-distributed declaration suppliers with pattern subscriptions

Concretizes ADR-0012. A plugin is a **composer package** (`type:
steins-plugin`), loaded in the sidecar via the project's own autoload
(ADR-0024's lazy path), discovered from installed package types plus
explicit steins.toml listing (which wins).

**What a plugin supplies (v1 — deliberately narrow):**
- **Synthetic declarations**: type signatures for functions/methods/classes,
  expressed as **phpdoc type strings** — ADR-0029's grammar doubles as the
  wire format; no new schema is invented.
- **Label registrations**: effect labels and value provenance labels
  (ADR-0018/0038 registries) plus catalog fragments coloring functions.
- **Not diagnostics** (v1): the zero-FP banner cannot vouch for third-party
  finding quality; plugin-emitted diagnostics arrive in a later version
  under their own registered families (ADR-0022's channel).

**Subscription model (learned from Mago)**: the manifest declares which
symbols the plugin can speak about via exact/prefix/namespace name
patterns — Steins queries `plugin(id, "declare", {symbols})` on demand when
matching symbols are encountered, never requiring an upfront universe dump.
Mago's other extension face — AST-event hooks — is deliberately NOT
imported: our plugins are fact producers, not analysis participants, and
per-node hooks over IPC would be a hot-path disaster. The distribution
trade against Mago's compiled-in Rust plugins is recorded: theirs is fast
but closed (third parties must fork or upstream); ours is open and can
boot the real framework, with the cost paid once per environment
fingerprint (composer.lock hash + plugin versions) under which responses
are cached — an LSP session never boots Laravel twice.

**Merge rules (Rigor import)**: core and native declarations are
authoritative; plugins refine, never weaken — a supplied declaration
conflicting with a native type is rejected and recorded as a plugin
inconsistency. Supplied declarations enter the ADR-0037 trust order below
verified phpdoc as "plugin assertions": when propagation disproves one,
the truth keeps flowing. Bundles carry `steins-plugin-api: 1`;
unrecognized newer versions are not loaded and are reported by name —
silence names itself.
