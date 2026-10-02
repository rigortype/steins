# Initial effect labels

> Amended by ADR-0083: the `output` root below is retired — output is an
> ambient channel under `io` (`io.output` and its ob-capturability split),
> and `io.input` joins it.
>
> Amended by ADR-0101: a locale read is an effect, `global.read.setting.locale`,
> and the printf family carries it (`%f`, `%g` and `%G` render `LC_NUMERIC`'s
> decimal point). The "Pseudo-constant settings" paragraph below now describes
> an opt-in that would drop a *setting-read label* from the folding gate, and
> it remains unbuilt.
>
> Amended by ADR-0021's 2026-10-01 amendment: the folding gate below needs a
> name to be pure, but a pure name need not fold. A builtin can be catalogued
> pure from a certified list without joining the allowlist.
>
> Amended by ADR-0021's second 2026-10-01 amendment: a catalogued builtin's row
> holds at a call only where no argument can reach user code (`__toString`,
> `Countable::count`, a callback, the autoloader); otherwise the call is `…?`.
>
> Amended by ADR-0018: the labels below are now the roots of a hierarchical
> label taxonomy (`io` ⊃ `io.fs.read`/`io.net.http`/…, `nondet` ⊃
> `nondet.random`/`nondet.time`; `global-read`/`global-write` spelled
> `global.read`/`global.write`), open to ecosystem/private labels via the
> plugin registry.
>
> Terminology (amended): the canonical term for these atoms is **effect
> label** — matching ADR-0018/0038 and the code's `effect_labels`
> registry; "effect-kind" and the body's "colors" are historical
> spellings. "Kind" is avoided as a term of art: in effect-row systems
> (Koka) *kind* classifies effect types while the atoms are precisely
> *labels*, so "effect-kind" invites the inverse reading. The structure
> is the powerset lattice over labels (the filename's "lattice");
> the declared per-function aggregate is the *envelope*, the inferred
> one an *effect set*. "Color" survives only as prose metaphor.

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
