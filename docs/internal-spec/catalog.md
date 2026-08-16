# The Builtin Catalog

**Status: partial.** The tables below exist and are consumed, except
`failure_arms`, which is behavior-neutral data awaiting its consumer.
ADR-0008, ADR-0014, ADR-0018, ADR-0021, ADR-0033, ADR-0040, ADR-0042,
ADR-0043, ADR-0056, ADR-0069.

`steins-catalog` depends on nothing. It is a self-contained body of knowledge
about PHP's builtins and extensions, testable without an analyzer.

## Version pinning

```rust
pub const PINNED_PHP: (u16, u16) = (8, 5);
```

The generated tables are mined from php-src at a pinned commit
(`6bc7c26cf6…`, Thu Jul 9 2026) and cross-checked against **PHP 8.5.8**.

Only `(major, minor)` is pinned: builtin type edges are stable within a minor
line, so the patch component is irrelevant. A catalog-backed is-a verdict used
for **arm deletion** is demoted to `Unknown` when the sidecar reports a
different minor (ADR-0052 amendment A11) — a different minor may add or remove a
supertype edge the table does not reflect, and keeping the arm is the FP-safe
side.

## `foldable(name)` — the folding allowlist

A hand-picked list of builtins that are pure and deterministic under ADR-0008's
rule. Matching is case-insensitive.

It is deliberately **not** a computed property. Uncoloured functions widen — a
miss, never a false positive — which is the only seeding order compatible with
the zero-FP bar (ADR-0002).

Contents, in broad strokes: ASCII string transforms (`strtolower`, `trim`,
`substr`, `str_replace`, `sprintf`, `strlen`, …) and pure numeric/conversion
functions (`abs`, `intdiv`, …).

### The three portability classes

`foldable` is **derived**, not primitive. The primitive is
`portability_class(name)`, which answers `None` off the allowlist and otherwise
one of three classes — so "on the allowlist" is exactly "has a portability
verdict at all". The class decides what an engine *other than the project's own*
may fold; on a provably 64-bit engine all three fold, and on anything else (an
unreported width, a machine nobody has probed) nothing folds at all.
Default-deny throughout (ADR-0066 §4, ADR-0028's 2026-08-14 amendment §4).

The class was called `WidthClass` while every row in it was about the engine's
integer width. `preg_split` ended that: it is refused because one build's PCRE
has a JIT and the other's does not. The gate's real question has always been
whether a *second* engine may fold the name, and the word size is one answer to
it among several.

| class | evidence behind a row | folds on 64-bit | folds in the browser (php-wasm, `PHP_INT_SIZE = 4`) |
| --- | --- | --- | --- |
| `Portable` | differential probes, 32-bit against 64-bit | yes | yes, for argument tuples the range guard admits |
| `Refused` | **one recorded divergence per row**, carried as data by `refusal()` | yes | no |
| `Unverified` | **none — and that is the correct amount** | yes | no |

The evidence discipline differs per class and is the point of the split:

- **`Portable`** is a positive claim, and it is earned by probing. The
  classification as a whole stands on **1073 adversarial tuples** through the
  same dispatch core both engines run — one tuple being one `(name, arguments)`
  case, whichever way its verdict went, and a second calling convention over the
  same case being that tuple probed twice rather than a second tuple. The
  per-round ledger that defines and sums this is at the end of ADR-0066; a
  single name's evidence is its line in its round's disposition table, never the
  total. A probe of an *array*-returning name compares the response
  **bytes**: array elements travel with no per-element type tag, so an `int` on
  one engine and a `float` on the other are legible only as
  `JSON_PRESERVE_ZERO_FRACTION`'s `3000000000` versus `3000000000.0`, which any
  JSON parse erases (issue #354 found a divergence this way that the parsed
  comparison had called clean).
- **`Refused`** is also a positive claim — that the engines *disagree* — and the
  ADR-0061 refused-row discipline requires the divergence to be on record beside
  the name. It is now on record as **data**: `refusal(name)` answers a
  `RefusalAxis` and a one-line witness, `every_refused_row_carries_its_witness`
  makes the discipline mechanical, and the playground's boundary panel composes
  its sentences from that instead of writing them itself.
- **`Unverified`** claims nothing. It means nobody looked, and **the correct
  number of probes behind a row here is zero** — evidence moves the row out, to
  `Portable` if the engines agree and to `Refused` with its divergence if they
  do not. **The class is empty today**, and that is the class working rather
  than the class being retired: its last two rows, `array_merge` and `explode`,
  were measured by `cargo xtask fold-probe` in issue #382 (25 and 13 tuples,
  both calling conventions, zero silent and zero reverse) and both left for
  `Portable`. An empty list is what "no outstanding debt" looks like; the class
  stays so the next row admitted ahead of its evidence has somewhere honest to
  sit.

`Refused` and `Unverified` are *mechanically identical*: they ride the one
`portable` question the fold gate asks, and neither folds on a narrow engine.
They are kept apart because mixing unevidenced rows into the refused list would
erase the one-witness-per-row discipline that makes it worth reading.

#### The axes, and the ones the instrument cannot see

`RefusalAxis` has the kinds of divergence the differential has actually found:
`IntegerWidth` (ten rows) and `BuildOption` (one, `preg_split`). It is not a
taxonomy of everything that could go wrong, because the instrument has blind
spots and they are worth stating:

- **The operating system.** Both engines are POSIX. `DIRECTORY_SEPARATOR` and
  `escapeshellarg("a b'c")` agree byte for byte, and `PHP_OS_FAMILY` differs
  only as `Darwin` against `Unknown`. Windows is a third machine nobody probes,
  so an OS-shaped value cannot be *refused by measurement*; a name like
  `escapeshellarg` stays off the allowlist by argument, the way `strcmp` does
  for promising only a sign.
- **An ini both builds happen to share.** Both report `precision = 14` and
  `serialize_precision = -1`, so a float-rendering name agrees here and would
  not on a project that sets either differently. That exposure is named per row
  (`strval`, `implode`, `array_unique`) rather than pretended away.
- **An extension one build lacks.** Visible, but as a decline rather than a
  divergence: php-wasm 0.1.0 loads 25 extensions to the native build's 70, so
  `mb_*` answered `widen: unknown function` for all eleven probes.

A name reaches `Unverified` only when the fold is **strictly stronger** than the
Rust rung it would shadow (the amendment's §5) — the admission rule, which still
governs even with no row currently in the class: `explode`'s rung is type-level
(`non-empty-list<string>`), so the fold upgrades a type to a value on the
all-literal path and the rung survives beneath it as the no-sidecar floor.
`array_slice`, `array_combine` and `array_fill_keys` are excluded by that same
rule — their rungs are already exact, and cover non-literal arguments a fold
never can.

A foldable name must **not invoke a callback**. The allowlist gates the callee,
and a builtin taking a callable smuggles a second callee past it as an ordinary
string argument that the seam hands to the runner verbatim — measured, on a
branch that briefly admitted `array_filter`: `array_filter(["PATH"], "getenv")`
folded to `list{'PATH'}`, which is `getenv` running inside the analysis.
`no_foldable_name_invokes_a_callback` asserts no allowlisted name carries an
`invocation_shape` row, and lifting that needs a shape gate at the seam (fold
only when the callback argument is absent or a literal `null`), not a catalog
edit.

That test is a **tripwire, not a barrier**, and the difference is worth keeping
straight: `invocation_shape` is a curated table with one `callback_param`
position per row, so it cannot express `preg_replace_callback_array`'s callbacks
as array *values* or the `array_udiff` family's comparator at a variadic tail.
Admitting one of those would pass the test. Every name on the list today takes
no callable at all, so the rule holds; making it *mechanical* needs an
independent answer to "does this name take a callable", which is the mined
arginfo table issue #382 asks for. Until then a new admission is read by a
human, and the test catches the shapes the catalog can already see.

The five names that amendment deferred were probed in issue #354 and left the
deferral in both directions: `str_split`, `array_fill` and `array_unique` to
`Portable`, `range` and `preg_split` to `Refused`. None passed through
`Unverified`, which is the class working as defined — evidence moves a row *out*
of it, and a row only enters by being admitted unmeasured. `range`'s refusal is
the one that generalizes: its bounds are declared `string|int|float`, so the
engine's own width types a numeric string, and no bound on integer *arguments*
can see it. The other four take plain `int` parameters, where the same oversized
argument is a `TypeError` on the narrow engine — a decline, which is sound.

**Deliberate exclusions**, even where frequent:

- `mb_*` — encoding-dependent.
- anything affected by `setlocale`, the current timezone, or
  `mb_regex_encoding`-class settings — the value is not portable without
  ADR-0008's opt-in pseudo-constant configuration, which is not implemented.
- `nondet` builtins (`time`, `rand`, `microtime`) — excluded by definition.

One ini is **not** excluded and is worth knowing about: `precision` decides how
a float renders, so `strval`, `implode` and `array_unique` all fold under it.
`array_unique` is the one where it changes the array's *length* rather than a
spelling, since its default `SORT_STRING` compares string casts. All three are
admitted together or not at all; closing the seam is ADR-0008's opt-in
pseudo-constant configuration, which is not implemented.

## `effect_labels(name)` — effect coloring

Maps a builtin to its effect labels, or `None` for uncatalogued (which widens to
unknown-effect: exhaustiveness taint, no finding). A coloured entry wins;
otherwise a foldable builtin is catalogued with the **empty** effect set.

Coverage is frequency-seeded (`docs/notes/20260722-builtin-frequency.md`) plus
the gaps identified in `docs/research/phpsrc-mining/effects_gaps.md`:
randomness, time, filesystem read/write, output (ADR-0083's `io.output`
family), header mutation, signals, System-V IPC, global/ini state, the
read-and-relay pair `readfile`/`fpassthru`, the output-relaying
`system`/`passthru`/`curl_exec`, and the composite `session_start`.

**Every filesystem row is `io`** (issue #318). `file_get_contents`,
`file_put_contents`, `fopen`, `copy`, `rename`, `readfile`, `fpassthru`, the
resource-taking `fread` / `fgets` / `fwrite` / `fputs`, and the stat-and-unlink
family `unlink` / `mkdir` / `rmdir` / `touch` / `scandir` / `file_exists` /
`is_file` / `is_dir` all reach whatever the stream layer resolves their argument
to, so the argument-blind row can only be the `io` parent; a row of `io.fs.read`
would hide a network read under an `io.fs.read` envelope, which is precisely the
upper-bound contract's failure mode. The stat-and-unlink family is no exception
— `unlink('ssh2.sftp://…')` deletes over the network, `file_exists('ftp://…')`
stats over it — so **no argument-blind row in this table produces an `io.fs.*`
label any more**; `session_start`'s composite is the one place that label
survives arg-blind, and its default handler does write a real session file.
`narrowed_stream_labels` below is what gives the precise labels back.

Recorded imprecisions, stated rather than hidden:

- `print_r` / `var_export` are coloured `io.output.buffer` even though they are
  pure in return-mode (`$return = true`); the arg-blind upper bound is the safe
  choice. `curl_exec` keeps its `io.output` component the same way — only
  `CURLOPT_RETURNTRANSFER` suppresses the echo.
- `system` / `passthru` / `curl_exec` take the parent `io.output`, not
  `io.output.buffer`: whether an output buffer captures a relayed child's
  output is not settled, and ADR-0083 puts split evidence on the side a future
  masking cannot deduct. None of the three is wrapper-capable, so all three keep
  their precise transport component (`io.process`, `io.net`).
- The `ob_start` family is deliberately absent — widening to unknown effect is
  sound until masking exists (ADR-0083).
- `sleep` / `usleep` are `io` — an observable timing side effect, closest to the
  `io` root among the initial labels.
- `srand` / `mt_srand` / `clearstatcache` are `global.write`: all three replace
  process-global state (the RNG generator, the engine's stat cache). Drawing
  from the RNG stays `nondet.random` — seeding writes the state a draw reads,
  and conflating the two would lose both.
- `exit` / `die` are **language constructs**, not functions; they never reach
  this table and are detected structurally.

## `narrowed_stream_labels(name, first, second)` — call-site narrowing

The other half of the `io` rows above, and the reason widening them costs no
precision on ordinary code. It takes the call's first two positional arguments in
their **proven-constant** form (`StreamTarget::Literal` for a quoted string with
no interpolation, `StreamTarget::Constant` for a bare constant fetch) and answers
with the labels that target proves, or `None` when nothing here proves anything —
in which case the caller keeps the `io` default. What the second argument means
is the row's business: `fopen`'s mode, `copy`/`rename`'s destination, nothing at
all for the rest.

Each target is read through **its own role's** direction. That is what makes a
two-target row honest: `copy($from, $to)` reads one path and writes the other, so
`copy('/a', '/b')` is `["io.fs.read", "io.fs.write"]` and
`copy('https://…', '/b')` is `["io.net.http", "io.fs.write"]`. `rename` writes on
both sides — it moves a directory entry and reads no contents — so its proven
pair collapses to `io.fs.write`.

| target | narrowed to |
| --- | --- |
| no scheme (a plain path), `file://`, `zlib://`, `phar://`, `glob://`, `compress.*://`, `php://temp` | that target's own direction — `io.fs.read` for `file_get_contents`/`readfile`/`fread`/`fgets`/`scandir`/`file_exists`/`is_file`/`is_dir` and for `copy`'s source, `io.fs.write` for `file_put_contents`/`fwrite`/`fputs`/`unlink`/`mkdir`/`rmdir`/`touch`, for `copy`'s destination and for both of `rename`'s, and for `fopen` the mode (`r` → read, `w`/`a`/`x`/`c` → write, a `+` or an unprovable mode → the parent `io.fs`) |
| `http://`, `https://` | `io.net.http` |
| `ftp://`, `ftps://`, `ssh2.*://`, `tcp://`, `udp://`, `ssl://`, `tls://` | `io.net` |
| `unix://`, `udg://` | `io.ipc` — a domain socket is cross-process state, not network transport |
| `expect://` | `io.process` |
| `php://output` | `io.output.buffer` |
| `php://stdout`, `php://stderr` | `io.output.stdout`, `io.output.stderr` |
| `php://input`, `php://stdin` | `io.input` |
| `php://memory`, `data://` | `mutate.local` |
| `php://filter/…/resource=<target>` | the trailing target, resolved **one** step (a filter naming a filter stops) |
| `STDIN`, `STDOUT`, `STDERR` on a resource row | `io.input`, `io.output.stdout`, `io.output.stderr` |
| anything else — `php://fd/3`, an unknown or userland scheme | `None`: the `io` default stands |

Four deliberate refusals:

- **A userland wrapper** (`stream_wrapper_register('acme', …)`) is an unknown
  scheme, so the call keeps `io`. Ruling D-W1 is an approximation, not a
  mechanism — nothing reads the registration.
- **`copy` / `rename` with one provable side.** The row is the union of the two
  targets, and the unprovable side contributes `io`, whose union with anything is
  `io`. Both sides must be constant or the answer is `None`, rather than a
  precision the call has not earned.
- **A `php://` target on a stat-and-unlink row.** Those eight open no stream, so
  `is_file('php://stdout')` is not a question about a channel and naming one
  would be an invention; the `io` default stands. Their scheme narrowing is
  otherwise the same table as everyone else's.
- **A form mismatch.** A resource row handed a string literal (`fwrite('/tmp/x',
  …)` passes no resource) or a path row handed a constant narrows nothing.

The read-and-relay pair is the one composite: narrowing restores the
`io.output.buffer` component beside the target's own label, which the `io`
default had folded away.

`StreamTarget` is the catalog's own tiny enum, mirrored by
`steins_syntax::CallTarget` on the scan side. The duplication is the price of
this crate depending on nothing; `steins-infer` depends on both and translates.

## `method_effect_labels(class, method)` — method-shaped effect rows

The class-world twin of `effect_labels`, keyed by `(class, method)` instead of a
function name, with the same three-valued contract: `Some(labels)` is coloured,
`Some(&[])` is catalogued-pure, `None` is uncatalogued and widens. Both keys
match case-insensitively — PHP folds case on class *and* method names.

The class key is the **global** name, no namespace: these are engine classes. A
consumer resolves the receiver to an FQN first and only then keys the table, so a
namespaced `App\PDO` never collides with the engine's `PDO`; and a class the
*project* defines shadows the table entirely, because the project's own
method→method effect edge is a better answer than a hand-written row.

Membership today is one family — `PDO::query`/`exec`/`prepare` and
`PDOStatement::execute`/`fetch`/`fetchAll`, all `io.db` (issue #67). That is the
first producer of a label the registry had carried since ADR-0018 with nothing to
emit it. `prepare` takes the same coarse colour as the rest: whether it is a
round trip to the server depends on PDO's emulated-prepares setting, which is
runtime configuration the catalog cannot read, so the row takes the upper bound.

Breadth — mysqli, the rest of the mining data's method rows — belongs to the
ADR-0014 generator, not to hand-seeding. What ships here is the row format and
its receiver-resolution contract.

## `known_labels()` / `subsumes()` / `is_known_label()` / `nearest_label()` / `LabelRegistry`

The effect label registry and prefix subsumption. Semantics are specified in
[`effects.md`](../type-specification/effects.md). `nearest_label` supplies a
Levenshtein-based typo suggestion (distance ≤ 2).

`known_labels()` is the **builtin** half and stays a closed constant. What
inference actually asks is `LabelRegistry`: that table plus the extension labels
the ADR-0068 plugin channel registered for the project at hand.
`LabelRegistry::builtin()` is the default and answers identically to the free
functions, so every caller without a project in hand (a single-file check, the
browser) is unaffected. `core_roots()` / `is_core_label()` name the roots Steins
owns — the other side of the vendor-root rule a plugin registration passes.

## `hierarchy_generated` — the builtin class hierarchy

352 rows of `(lowercased class/interface name, direct supertypes)`, generated by
`cargo xtask gen-catalog` from `docs/research/phpsrc-mining/hierarchy.toml`.
Sorted by key for binary search; the TOML is the source of record and the Rust
file is `@generated` — never edited by hand.

Consulted only by `builtin_class_supers`, which the trinary is-a oracle walks
transitively. A name absent from the table is an unknown external →
`Unknown`, never `No`.

**Builtin enums are deliberately omitted**: the mining data for their implicit
interfaces and backing is incomplete, and an incomplete row would produce a
wrong `No`.

## `builtin_exception_parent(name)` / `builtin_throws(name)`

The standard SPL/engine exception tree, keyed by global simple name
(case-insensitive, no namespace). Project classes chain into it through their
`extends` once their own chain leaves the project index. A name absent here and
not a project class has an **unknown** parent — the caller keeps the chain result
at `Maybe`, never `No` (ADR-0040's FP-safe side).

`builtin_throws` gives the throw classes a builtin can raise.

## `invocation_shape(name)` — higher-order builtins

The callback parameter index, immediate-vs-deferred invocation, and the
callback's argument source. The table and its irregularities are documented in
[`closures.md`](../type-specification/closures.md). A function absent from the
table is not treated as a higher-order invoker — its callback argument stays an
opaque taint.

## `param_facts(name)` / `param_facts_mined(name)` — the engine's own arginfo

The independent witness the two hand-transcribed parameter tables are checked
against (issue #382). Mined by `cargo xtask mine-param-facts`, which reads every
internal function of the resident engine through `ReflectionFunction`, into
`docs/research/phpsrc-mining/param_facts.toml`; `cargo xtask gen-catalog` emits
the shipped `param_facts_generated.rs`.

Deliberately **not** a second pass over php-src's stubs: `out_params` and
`invocation_shape` were transcribed from those by hand, and a second
transcription would agree with them wherever they are wrong. Arginfo is what PHP
dispatches on.

A row carries, per position, `by_ref`, `callable`, `variadic` and `optional`,
plus each parameter's declared type spelling and the required-argument count.
Rows are kept for every name carrying one of the first three, and for every name
on the folding allowlist whether it carries anything or not; every other mined
name is recorded as a bare name. That second list is load-bearing rather than
padding — `param_facts_mined` is how a test tells "mined, and carries nothing"
from "nobody looked", and reading absence as agreement is the exact vacuity this
table was built to remove:

> `by_value_arg` falls back to `out_params`, so a name with **no** row answers
> `Some(true)` at every position, and a loop keyed on it skips precisely the
> omission it is hunting.

What the table can and cannot see:

- `by_ref` is exact — it is the engine's own parameter flag.
- `callable` means the parameter's **declared type** admits a callable. Sound,
  not complete: `array_udiff` takes its comparator at a variadic `mixed` tail
  and `preg_replace_callback_array` takes its callables as array *values*.
  Neither is declared, and both are covered anyway: the fold seam refuses an
  argument reaching an untyped variadic tail unless `variadic_tail_is_data`
  argues that tail carries values, and it refuses a non-empty array at the
  position `callables_in_array_param` curates. That second one is a list rather
  than a rule because nothing in a signature distinguishes `[$k => $callback]`
  from `[$k => $v]` — the curation IS the claim that the engine calls what it
  finds there.
- The universe is **the mining build's**. `[meta] extensions` records which
  build answered; a name from an extension it lacked is absent, not clean.

Six properties are enforced against it, and each fails loudly rather than
quietly: every foldable name was mined; a foldable name's by-ref positions are
exactly its `out_params` row (the ADR-0077 precondition, previously unfalsifiable);
no `out_params` row claims a position the engine denies; no foldable name takes a
declared callable; a foldable name with an untyped variadic tail is listed with
the argument for why that tail is data; and every `invocation_shape` row names a
position the engine declares callable, with every other declared-callable builtin
either rowed or named in a closed exclusion list.

> The same table drives `cargo xtask fold-probe`, the differential width probe:
> its tuple families are keyed by **declared parameter type**, read from here, so
> the generator's specification is a property of the signature rather than of
> whoever wrote a per-name tuple list. Parameter *names* are mined for one
> reason — only the name tells a size-shaped `int` (`$length`, `$times`,
> `$count`) from an offset, and an oversized probe on the first is a
> multi-gigabyte allocation and a dead runner.

## `return_fact(name)` — curated value-domain return refinements

**Consumed** (ADR-0056 R3+R4). A small hand-curated table
(`return_facts_generated::RETURN_FACTS`, generated by `cargo xtask gen-catalog`
from `docs/research/phpsrc-mining/return_facts.toml`) mapping a builtin's simple
name to a **value-domain refinement string** — thirteen rows today:

- length/count builtins to `int<0, max>` — `count`, `sizeof`, `strlen`,
  `mb_strlen`, `substr_count`, `func_num_args`, `array_push`, `array_unshift`;
- hash/id builtins to `non-falsy-string` — `sha1`, `md5`, `uniqid`.

The table is a **refinement within** the reflected return envelope, not a
replacement for it: the sidecar's `reflect` supplies the coarse envelope (the
engine's own `getReturnType()`), and a curated row narrows it where the reflected
type is looser than the runtime guarantee. It is consulted only by `return_fact`,
which the `SidecarFolder`'s `builtin_return_fact` composes with the reflected
envelope (`steins-infer`, ADR-0056 R1). A discipline of **refused rows** keeps
the table honest — a builtin whose refinement cannot be stated as a single
value-domain fact is left out rather than approximated. R1 landed the table
empty; R3+R4 seeded these eleven.

## `declared_return(name)` / `declared_return_changed_at(name)` — the Asserted return floor

**Consumed** (ADR-0069, issues #73/#79, ADR-0071). 1,708 rows of `(lowercased
builtin name, canonical phpdoc spelling)`, generated by `cargo xtask gen-catalog`
from `docs/research/phpstan-mining/declared_returns.toml`. That TOML is itself
generated, by `cargo xtask mine-function-map`, and is the source of record.

A spelling is anything the **declared-contract arm lane** carries, and each
widening kept every row before it name for name:

* the four scalar bases and their `null` pairs — 919 rows, the #73 population;
* the 439 rows issue #79 added, where functionMap genuinely exceeds reflection:
  the `T|false` failure unions (`strstr` → `string|false`, `array_search` →
  `int|string|false`) and the scalar refinements (`mb_strtoupper` →
  `uppercase-string`, `preg_match` → `0|1|false`);
* the 248 array-vocabulary rows ADR-0071 added once `subsumes` could decide an
  array pair at all (`str_split` → `list<string>`, `imagecolorsforindex` → a full
  `array{…}`);
* the 102 class rows the object slice added (`gmp_init` → `GMP`, `collator_create`
  → `?Collator`), which needed no new relation — `subsumes_class` was already
  reflexive, and a row naming the class the engine names countersigns on that
  alone.

The consumer re-lowers the string through the same `lower_str` → `flatten_arms`
seam a **project** function's declared return takes (issue #60) and seeds the
resulting arms `Asserted`. A class row is *arm-lane only*: the value domain has no
object inhabitant, so it seeds no fact at all.

This is the **bottom rung of the return ladder** and the one table here whose
lineage is not php-src: the rows are mined from PHPStan's `resources/functionMap.php`
at a pinned commit, which is itself copied from Phan's `FunctionSignatureMap.php`.
The root [`NOTICE`](../../NOTICE) carries both MIT permission notices;
`THIRD-PARTY-LICENSES.md` is untouched, because it is generated from the cargo
dependency graph and this data never enters it.

**When it speaks: per name, not per run.** `steins-infer` consults it exactly where
the sidecar-backed reflected envelope answered `None` for the asked name. `--no-php`
(and the browser before php-wasm loads) is only the total case; with a live engine
the floor still speaks where that engine is *silent* — an extension the analyzing
PHP does not load, a builtin with no declared return type. Where the engine answers,
the floor is never consulted.

**Grade: Asserted, never Verified.** The seeded fact carries `Stratum::Asserted`, so
the proof layer's all-Verified premise rule keeps every proof-layer finding off it by
construction. It reaches the dump surface (rendered `(asserted)`) and contracts-tier
reasoning, which is the whole intended blast radius.

**Never an existence answer.** The absence family reads the boot surface and never
this table. An absence finding standing beside a floor fact is complementary: the
call fails on the analyzing PHP, and this is the shape it declares where it does
exist.

**Version discipline** is A11-shaped, via `declared_return_changed_at`: the
functionMap delta files are the change oracle, and a name whose declared return type
moved at minor *m* is admitted only for a project target lying wholly at or above
*m*. An undeclared target admits (the row is Asserted anyway). The two tables now
**intersect** — ADR-0071 admitted all four version-sensitive names, which return
arrays — so the gate is live end to end rather than merely wired.

**The engine countersign** admits a row on either of two shapes: it *bounds* the
engine's own declaration (`engine ⊆ row` — a coarse upper bound, the #73 rule), or
it *refines* it arm-wise, with every engine arm still covered by some row arm. That
last clause is what keeps a `string` row from silently swallowing the `null` in the
engine's `?string`; 75 rows are refused on it and listed verbatim in the TOML.

What is **deliberately not here**, counted rather than hidden (see the TOML's
`[counts]`): 6,658 `Class::method` rows, and 474 rows in the bucket labelled
object/`callable`/`resource` — which at this pin is 322 `void`, 149 `resource`, 2
`Closure` and 1 `int-mask<…>`, the label being older than its contents. What blocks
those is that `normalize::subsumes` has no extensional denotation for them, so the
generation-time countersign would be vacuous. `resource` is the substantive
deferral: it is a `KNOWN_UNENFORCED` keyword lowering to an opaque arm, not a class,
which is exactly why the stale rows where functionMap still says `resource` and PHP
8 returns a `GdImage` stay out. Hierarchy-dependent class rows are refused for a
related reason — the reflexive floor cannot decide a subclass claim — and both wait
on the same sidecar `reflect` extension. Also absent by construction: 75 rows the
countersign refused and 2,793 names the pinned engine does not know as functions.

## `failure_arms(name)` — failure-cause classification

**Behavior-neutral catalog data: nothing consumes it yet.** The boundary
profiles of ADR-0037/ADR-0042 that would are future work.

Mined from php-src C (`docs/research/phpsrc-mining/failure_arms.toml`), it
distinguishes three states a boundary profile must tell apart:

| Value | Meaning |
| --- | --- |
| `Causes(&[FailureCause])` | the `false`/`null` arm is a real failure, with the distinct causes its arms were traced to (`curl_init` is `[Resource, Input]`) |
| `Sentinel` | the `false`/`null` is a **legitimate result** — `strpos` "not present", `array_search` "not found" — and must never be `failure.*`-labeled |
| `None` | unclassified; the catalog states nothing |

The three causes map to `failure.*` registry labels: `Resource`
(allocation/handle exhaustion — statically irrefutable, default profile exempts
it), `Environment` (filesystem/network — a normal operational outcome; not
checking it is a real bug), `Input` (argument-value-determined — statically
refutable with proven arguments).

This is the honest-union + policy-profile replacement for the erased benevolent
union ([`divergence-registry.md`](../type-specification/divergence-registry.md)).

Method-shaped rows from the mining data (`DateTime::createFromFormat`) are still
deferred **here**: `failure_arms` is function-keyed, and nothing consumes it yet
either. The effect table is no longer — `method_effect_labels` above is the row
format the other tables will follow once a consumer wants them.

## Not implemented

- **ADR-0014's sourcing pipeline** — php-src stubs as the base with a Steins
  effect layer on top, and phpstorm-stubs as a PECL supplement. What ships is
  hand-seeded.
- **Builtin *signatures*.** The catalog carries effects, hierarchy, throws,
  failure arms, invocation shapes, the curated **return-fact refinements**, and —
  since ADR-0069 — the **declared return envelopes** above. It still carries no
  parameter types and no return type richer than a single-base envelope (a shaped
  array, a `T|false` union, a refinement). That is why
  `call.too-many-arguments` for internal targets waits on the sidecar `reflect`
  slice.

  **The arity half of that wait is now over** (issue #76). The `reflect` reply
  carries `params_total` / `params_required`
  (`ReflectionFunction::getNumberOfParameters()` and
  `getNumberOfRequiredParameters()`), surfaced on
  `steins_sidecar::Reflection` and reachable through
  `steins_infer::Folder::builtin_param_counts`. It landed as ADR-0064's
  mixed-pin second leg — a rule whose name declares a bare `mixed` countersigns
  itself against the live signature — and **no checker consumes it yet**:
  `call.too-many-arguments` for internal targets is a separate slice that can now
  read this surface instead of a parameter table. Absent counts (an older runner,
  a canned replay table recorded before the field, a reflection failure) stay
  `None`, which withholds rather than guesses.
- **Flag inspection** — `json_decode`/`json_encode` throw `JsonException` only
  under `JSON_THROW_ON_ERROR`; without flag inspection those rows stay
  uncatalogued (widen) rather than manufacture a throw. The keys are present in
  the source, awaiting the machinery.
- **Plugin-registered ecosystem labels and signatures** (ADR-0012, ADR-0039).
