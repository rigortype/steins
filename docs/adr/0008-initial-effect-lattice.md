# Initial effect-kind set

The initial colors, each independently present/absent (`Pure` = all empty
except throw, per ADR-0006):

- **`throw<E…>`** — carries the set of exception classes; spelled `@throws`;
  checked/unchecked accounting per ADR-0007.
- **`output`** — writes to the output stream (`echo`/`print`, text between
  PHP tags). Separate from `io`: templates are output-heavy but io-free.
- **`io`** — filesystem, network, process.
- **`global-read` / `global-write`** — runtime-mutable global state (statics,
  superglobals, `mb_regex_encoding`-class settings), read and write split.
- **`nondet`** — `random_*`, time, object identity. Independent of `io`;
  build-time constants (ICU tables) are *not* nondet — that is value
  portability, not purity.
- **`exit`** — control does not return; also feeds reachability.
- **`mutate`** — caller-visible mutation of arguments/by-ref. Tracked as
  dataflow by inference (local by-ref writes like `preg_match`'s `$matches`
  are NOT an effect — the modular-analysis problem dissolves under call-site
  propagation); exists in the declaration vocabulary because `Pure` must
  forbid it.

**Folding gate**: an expression folds only if all colors are empty and
`nondet` is absent on the concrete path (ADR-0004's purity allowlist is this
gate applied to the catalog).

**Pseudo-constant settings**: an opt-in project config declaring set-once
globals (mb encoding, default timezone, ICU locale) removes `global-read` of
those settings from catalog entries — the (B)-class functions become foldable
for projects that pin their bootstrap.
