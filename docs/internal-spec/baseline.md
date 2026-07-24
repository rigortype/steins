# The Baseline

**Status: implemented** (`steins-cli::baseline`; ADR-0022, ADR-0050 §8).

The baseline is the accumulated past at adoption: a project records today's
findings once, and only *new* findings fail CI afterwards. It is the adoption
path that replaces gradual level-raising.

## Location

`.steins-baseline.jsonl` in the CWD by default, or `--baseline <path>`.
Machine-managed — the header says so.

| Flag | Effect |
| --- | --- |
| `--set-baseline` | write the current findings as the baseline |
| `--baseline <path>` | use a non-default location |
| `--ignore-baseline` | run as if no baseline existed |

## Format

JSONL. A header line, then one entry per line, **sorted by `(path, id, hash)`**
for diff stability.

```jsonl
{"steins-baseline":1,"note":"machine-managed; do not hand-edit","profile":"throws-direct","surface":["type.argument-mismatch","…"]}
{"id":"throw.undeclared","path":"src/Foo.php","hash":"a1b2c3d4e5f60718"}
```

Entry field order is the on-disk key order (`id`, `path`, `hash`).

Parsing is hand-edit tolerant: the header line is skipped, and blank or
unparsable lines are ignored rather than failing the run.

## The stable hash

**No line numbers.** `entry_hash` is the first 16 hex characters of SHA-256 over:

```text
id \n relative-path \n <flagged line, trimmed> \n <nearest non-empty line above, trimmed> \n <nearest non-empty line below, trimmed>
```

This is line-shift immune — the whole point. Unrelated edits elsewhere in the
file leave the entry matching; a change to the flagged line or its immediate
neighborhood correctly breaks the hash, and the finding resurfaces.

Paths are relativized to the **baseline file's directory** and normalized to
forward slashes. Both the file and the base directory are canonicalized when
possible; a file outside the base directory falls back to its canonical (or
original) path.

## The capture surface

The header records the profile the baseline was written under and the resolved
id set (ADR-0050 §8). Two consequences:

1. **Staleness is computed only over ids inside the current run's surface.** An
   unconsumed entry whose id is outside it is *dormant* — kept, not stale, not
   pruned. Without this, running `default` after baselining under `contracts`
   would prune every contract entry and silently discard the record.
2. **A run whose active surface exceeds the captured one prints a notice.** It
   "drowns loudly": the user is told the baseline was captured on a narrower
   surface, rather than discovering it through an unexpected wall of findings.

A pre-ADR-0050 header lacking `profile`/`surface` parses as `None`, and such a
baseline simply skips the surface-exceeds notice.

## Staleness

An entry inside the current surface that matches no finding this run is
**stale** — the finding it recorded is gone. Stale entries are reported so a
project can prune them; the mechanism exists so a baseline shrinks as debt is
paid rather than accumulating forever.

## What the baseline is not

- **Not a suppression config.** It records *what was*, not *what to ignore*. A
  new occurrence of the same id on a different line is a new finding.
- **Not hand-editable.** Nothing stops it, and parsing tolerates it, but the
  hash is derived — editing it by hand is editing a checksum.
- **Not message-keyed.** Messages are prose and keep improving.

Semantics of the surrounding channels:
[`diagnostic-policy.md`](../type-specification/diagnostic-policy.md).
