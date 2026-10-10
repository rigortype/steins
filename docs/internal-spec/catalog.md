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
  (`strval`, `implode`, `array_unique`) rather than pretended away. Since S6a
  (ADR-0101 §3.15) the seam refuses the float renderers it can name when a literal
  argument holds a float (`strval(1.5)`, `json_encode([1.5])`), so the exposure
  that remains is `array_unique` and the coercion of a float to a `string` parameter.
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
a float renders, so `strval`, `implode` and `array_unique` all fold under it
(`strval` and `implode` of a literal that holds a float no longer do: S6a, below).
`array_unique` is the one where it changes the array's *length* rather than a
spelling, since its default `SORT_STRING` compares string casts. All three are
admitted together or not at all; closing the seam is ADR-0008's opt-in
pseudo-constant configuration, which is not implemented.

## `effect_labels(name)` — effect coloring

Maps a builtin to its effect labels, or `None` for uncatalogued (which widens to
unknown-effect: exhaustiveness taint, no finding). A coloured entry wins;
otherwise a builtin is catalogued with the **empty** effect set when it is
foldable or **certified pure** (issue #851, ADR-0021's 2026-10-01 amendment).

Purity is a precondition of folding, not its definition, so the certified list
sits beside the coloured rows rather than on the allowlist. A name is certified
when php-src at `PINNED_PHP` shows that, for every argument its parameter types
accept, it runs no userland (no callback, no `string` parameter whose coercion
runs `__toString`, no loose comparison or string cast of an argument, no
autoload, no lazy-object initialization), takes no reference, reads only its
arguments, performs no I/O, and throws only the `TypeError` its types state.
An `E_WARNING` or `E_DEPRECATED` on bad input does not disqualify a name; the
error handler's effects belong to its registration. Two families are
certified:

- the type questions `is_string`, `is_int`/`is_integer`/`is_long`,
  `is_float`/`is_double`, `is_bool`, `is_array`, `is_null`, `is_object`,
  `is_scalar`, `is_numeric`, `is_iterable`, `is_countable`, `is_resource` and
  `get_debug_type`;
- the array readers `array_first`, `array_last`, `array_key_first`,
  `array_key_last`, `array_values`, `array_flip`, `array_reverse`,
  `array_slice`, `array_key_exists`/`key_exists` and `array_is_list`.

`array_keys` is certified only at one positional argument
(`pure_at_arity(name, positional)`): its search form compares loosely, which
runs an object's `__toString`, so its argument-blind row stays `None` and the
effects pass reads the call's arity. `current`, `key`, `get_class`,
`is_callable`, `is_a`, the `*_exists` questions, `array_combine` and the
string family stay out, each for a reason the amendment records
(`array_search`, which compares values, is certified at a call site instead,
below). A certified name is also "known" to every pass that asks the
catalog whether a name is a builtin (`knows(name)`, below). It is **not**
thereby throwless: the throw lane reads `throws_of(name)`, which answers only
for a row or an audited name (below).

A row describes what the builtin does; whether its **arguments** can reach user
code is a separate, per-parameter table (`arg_reach(name)`, issue #856,
ADR-0021's second 2026-10-01 amendment). Each position answers one `ArgReach`:

| Reach | What runs | Ruled out at a call site by |
|---|---|---|
| `Inert` | nothing | always |
| `Coerced` | `__toString`, through a coercive `string` parameter | a non-object argument, or `strict_types=1` |
| `Object` | the builtin converts or counts an object itself | a non-object argument |
| `Nested` | the builtin also converts or compares what an array holds | an argument with no object at any depth |
| `Callback` | the callable | never, at a plain call |
| `Autoload` | the autoloader, for a class named in a string | never |

The table is derived from the mined arginfo (`param_facts`) by declared type:
a scalar other than `string` is `Inert`, `string` is `Coerced`, a class,
interface, `object` or `iterable` is `Object`, `array` and `mixed` are
`Nested`, a declared `callable` is `Callback`, a resource position is `Inert`,
and a union takes its strongest member. A curated override list records where
php-src does less (`count` never reads an array's elements, `intval` and
`gettype` read a tag, the comparator sorts hand values to the callback) or
where the type cannot say (`is_callable` autoloads, `preg_replace_callback_array`
maps to callables). A certified name is `Inert` everywhere.

The effects pass applies the table to every catalogued function, coloured,
pure or out-parameter-only: a call is `…?` unless the call site rules out
every reaching position, by what the syntax layer shows the argument holds
(`ArgShape`) or, for `Coerced`, by the calling file's `declare(strict_types=1)`.
The row's labels apply either way.

The **string family** is certified under the same rule rather than
argument-blind (`certified_at_call_site(name)`): `strcmp`, `strncmp`,
`strcasecmp`, `strncasecmp`, `strspn`, `strcspn`, `substr_count`, `ord`, `chr`,
`bin2hex`, `hex2bin`, `dirname` and `unpack`, joined by `array_search` (issue
#860) and `number_format` (ADR-0101 §4, which reads no setting: it renders with `%.*F`,
witnessed on 8.1 and 8.5): `array_search` is not on the fold allowlist, so without a place here the effect lane
answered `no-effect-row` for it, which no proof at the call site can
discharge. `vsprintf` stays out because `%f`, `%g` and `%G` read `LC_NUMERIC`
(issue #991, as `sprintf`'s do); it is coloured instead, below. They are not on
`effect_labels`, so no other pass reads them as known builtins; the effects
pass resolves an otherwise unresolved call against the list and answers pure
only where the call site rules the reaching arguments out. Names that read the
locale (`basename`, `pathinfo`, `strnatcmp`, `strnatcasecmp`, `substr_compare`,
`parse_url`, `escapeshellarg`, `strip_tags`, the `ctype_*` family) are coloured with the read
below (S4) and are not certified pure; names that read an ini setting (the `mb_*` family,
`htmlspecialchars`) are coloured with the encoding cell's read (S6d, below) and are not certified pure.

**The locale cell** (ADR-0101, issue #991) has four registry labels,
`global.read.setting`, `global.read.setting.locale`, `global.write.setting` and
`global.write.setting.locale`, and the `precision` cell has `global.read.setting.precision`,
registered with its first coloured row (a `%s` of a float, ADR-0101 D4); S6-core adds the cells the ini names reach (below). These are the coloured rows. The coloured row answers
ahead of the fold allowlist's empty one, so `sprintf` is on the allowlist and
carries a read; the allowlist is permission to ask the engine, not a promise
that a call is pure, and Decision 2's bar for an **empty** row is unchanged.

| name | row |
| --- | --- |
| `sprintf`, `vsprintf` | `{global.read.setting.locale, global.read.setting.precision}` |
| `printf`, `vprintf` | `{io.output.buffer, global.read.setting.locale, global.read.setting.precision}` |
| `localeconv`, `nl_langinfo`, `strcoll` | `{global.read.setting.locale}` |
| `basename` | `{global.read.setting.locale}` (S4: `php_basename` consults the locale-derived `CG(ascii_compatible_locale)` before it looks at a byte, so every call reads it) |
| `ctype_alnum`, `ctype_alpha`, `ctype_cntrl`, `ctype_graph`, `ctype_lower`, `ctype_print`, `ctype_punct`, `ctype_space`, `ctype_upper`, `strnatcmp`, `strnatcasecmp`, `escapeshellarg`, `strip_tags`, `parse_url`, `sort`, `rsort`, `asort`, `arsort`, `ksort`, `krsort`, `substr_compare`, `pathinfo` | `{global.read.setting.locale}` as the upper bound the **call** decides (below) |
| `strftime` | `{global.read.setting.locale, global.read.setting.timezone, nondet.time}` (the locale half is decided by the format, below; the clock is dropped by a literal timestamp, below) |
| `gmstrftime` | `{global.read.setting.locale, nondet.time}` (it formats UTC and reads no zone) |
| `date`, `idate`, `mktime`, `strtotime`, `getdate`, `localtime` | `{global.read.setting.timezone, nondet.time}` (S6b-1: every call reads the timezone cell; the clock only where the timestamp is left out, below) |
| `gmdate`, `gmmktime` | `{nondet.time}` as the upper bound a supplied timestamp drops to `{}` (below) |
| `date_default_timezone_get`, `date_default_timezone_set` | `{global.read.setting.timezone}`, `{global.write.setting.timezone}` |
| `strval`, `settype`, `implode`, `join`, `json_encode`, `serialize` | `{global.read.setting.precision}` as the upper bound the **value rendered** decides (S6a, below) |
| `print_r`, `var_export`, `var_dump`, `debug_zval_dump` | `{io.output.buffer, global.read.setting.precision}`; `print_r` and `var_export` in return mode narrow to `{global.read.setting.precision}` (S6a, below) |
| `preg_match`, `preg_match_all`, `preg_replace`, `preg_replace_callback`, `preg_replace_callback_array`, `preg_filter`, `preg_split`, `preg_grep` | `{global.read.setting.locale}` as the upper bound the **literal pattern** decides (S5, below). `preg_quote` compiles nothing and keeps its empty row; `preg_last_error` and `preg_last_error_msg` have no row |
| `ctype_digit`, `ctype_xdigit` | none: C fixes their sets in every locale and no byte moved, so they read no setting that changes an answer (left uncatalogued, not certified) |
| `bcadd`, `bccomp`, `bcdiv`, `bcdivmod`, `bcmod`, `bcmul`, `bcpow`, `bcpowmod`, `bcsqrt`, `bcsub` | `{global.read.setting.ini}` as the upper bound the **`$scale`** decides (S6e, below) |
| `bcscale`, `error_reporting` | `{global.read.setting.ini, global.write.setting.ini}`: the old value is read on every call, and the write happens when a non-`null` value is given (S6e) |
| `get_include_path`, `set_include_path` | `{global.read.setting.ini}`, and `{global.read.setting.ini, global.write.setting.ini}` for `set_include_path`, which returns the old value |
| `set_time_limit` | `{global.write.setting.ini}` (it writes `max_execution_time`, which `ini_get` then reports) |
| `ini_get_all` | `{global.read.setting}`: its default lists every entry, `precision` and `date.timezone` among them (S6e) |
| `setlocale` | `{global.write.setting.locale, global.read}` (the argument-blind row: the write, and the environment block read for `''` and `null`, coarse until the env cell has a label; a call with exactly two arguments whose locale is a written non-empty string other than `'0'` narrows to `{global.write.setting.locale}`, and the exact string `'0'`, the query form, narrows to `{global.read.setting.locale}` with no write (ADR-0101 D6, `narrowed_setlocale_labels`; `"0\0x"` is not the query, php-src compares the whole string) |

**The time family** (S6b-1, `setting_reads/clock.rs` in `steins-catalog`; ADR-0101 §3.14). The row was
`nondet.time`, argument-blind. The two causes it joined are now apart. The zone is read on every call by `date`,
`idate`, `mktime`, `strtotime`, `getdate`, `localtime` and `strftime` (`get_timezone_info()`; `strtotime('… UTC')`
and `strtotime('@0')` read it though their value is stable), and the clock only where the timestamp is left out
(`if (ts_is_null) ts = php_time()`), so the row is the upper bound `{global.read.setting.timezone, nondet.time}`
and a **clock gate** (`ClockGate`, `clock_gate`) drops `nondet.time` where the call shows its timestamp as an
integer literal or constant, or by any argument shown **not `null`** at the call (an `int` parameter that the frame never writes, `time()` or another call whose declared return excludes `null`, arithmetic, a cast, a property declared so; `NullEvidence` in `ConstArgs::timestamps`, read by `Frame::non_null`): `date($f, 0)` is
`{global.read.setting.timezone}`, `date($f)` and `date($f, null)` are both, `gmdate($f, 0)` is `{}` and `gmdate($f)`
is `{nondet.time}`. The deciding argument is the timestamp (position 1 for `date`, `idate`, `gmdate`, `strftime`,
`gmstrftime` and `strtotime`'s base; 0 for `getdate` and `localtime`); `gmmktime` reads the clock unless all six
fields are shown supplied; `mktime` is **ungated** and keeps the clock at every arity, because the seed's DST
flag, taken from the current time, still decides the repeated hour of a fall-back transition. A timestamp that
may be `null` or the scan cannot place (a `?int`, an untyped or local variable, a named or spread list) keeps the label and is **no gap**: the clock is
an upper bound there, as it always was. `ConstArgs::literals` carries the literal arguments of the nine names.
`checkdate` reads nothing and has no row. The `DateTime` constructors and `date_create*` keep the argument-blind
`nondet.time` until their per-method table (S6b-2). None of these names is on the fold or the remembered
allowlist, so a call that reads a setting is neither folded nor remembered.

**The float renderers** (S6a, `setting_reads/precision.rs` in `steins-catalog`; ADR-0101 §3.15). A float becomes
text through `precision` (`strval`, `settype` to a string, `implode`, `print_r`) or `serialize_precision`
(`var_export`, `json_encode`, `serialize`, `var_dump`, `debug_zval_dump`); the catalog gives both the one
`global.read.setting.precision` label. The read is **value-conditional**: a renderer reads the entry only if the
value it renders is a float, so the row is the upper bound and a **precision gate** (`PrecisionGate`,
`precision_gate`) names which positions are rendered and how deep the renderer reads each (`RenderDepth`): `Value`
(`strval`, `settype`, `implode`'s separator: converted whole, an array is `"Array"`), `Elements` (`implode`'s array:
each element converted whole, one level) and `Nested` (`print_r`, `var_export`, `json_encode`, `serialize`,
`var_dump`, `debug_zval_dump`: every array and every object property). The call site reads each rendered position's
float evidence (`ConstArgs::rendered`, a `FloatEvidence` with a `Members` variant for an array literal) at that
depth, three-way as a printf `%s` is: a float, or an array literal holding one at the depth walked, is the proven
read; every value shown to hold none drops it; any other is `value-dependent-read`. Beyond what S3 reads (forms,
declared parameter and property types, constants, declared returns), a walked value is shown to hold no float by a
scalar-only declared type, a mined `list<string>`-shaped return, or an array literal of such members; `array`, a
class, `mixed`, a local (the scan reads a local's writes for the value, not for what an array in it holds) and a
parameter the frame writes are not. `json_encode` adds two arguments: `JSON_NUMERIC_CHECK`, or flags the scan cannot
evaluate, makes a float-free value undecided (a numeric string becomes a float), and a `$depth` makes a float
undecided (a deep value is refused before it is written). `settype` renders only to the type `'string'`; its
variable is rebound by the call itself, so it is always the gap. `number_format`, `round`, `intval` and the operator
sites (`(string) $f`, `"$f"`, `.`, `echo`; D4) carry no label. The fold refuses a renderer whose literal arguments
hold a float at the depth it renders them, and `json_encode` with `JSON_NUMERIC_CHECK`.

**Call-decided readers** (S4, `setting_read_gate` in `steins-catalog`, `site/setting.rs` in `steins-infer`).
Apart from `basename`, a locale reader reads only where the call reaches the routine that consults the
C library, so its row is the upper bound and the call site decides it as it does a printf format: an
omitted argument is the parameter's default, a literal the scan evaluates decides it lexically (a
string, an integer, an engine constant read as PHP resolves it, a `|` of such terms, a `true` or
`false`, and for `ctype_*` a `null`, float or array literal, a `new` expression or a by-value parameter the frame never
writes whose declared type has no `string`, `int`, `mixed` or `callable` member), and anything else (a
variable, an unevaluable expression, a named or spread argument list, the builtin handed over as a
callback) is the `value-dependent-read` gap and no label. A **mode** decides the sorts (`$flags`, position 1:
`SORT_LOCALE_STRING` and `SORT_NATURAL` read, with or without `SORT_FLAG_CASE`; `ksort` and `krsort` also
under `SORT_STRING | SORT_FLAG_CASE`, whose key comparison folds case through `tolower`; a data sort under that
pair reads before 8.2, so it is the gap on every floor, the persisted per-file row not knowing the PHP floor), `substr_compare` (`$case_insensitive`, position
4, reads when true) and `pathinfo` (`$flags`, default `PATHINFO_ALL`, reads unless `PATHINFO_DIRNAME` alone).
The **content** decides the rest, each by the trigger php-src shows: `ctype_*` a non-empty string or an `int`
in -128..=255 (every other type returns `false` before a table); `strnatcmp` and `strnatcasecmp` both operands
non-empty; `escapeshellarg` a non-empty string without a NUL byte; `strip_tags` a `<`; `parse_url` a first
colon past index 0, or no colon, no leading `//` and a byte other than `?` and `#` (the shapes that may fail
before any component are undecided); `strftime` a conversion that names the locale (`a A b B c h p r x X`, with
flags, width and `E`/`O`), the numeric ones reading nothing and any other undecided. `ConstArgs::bools` reaches
position 4, `ConstArgs::ints` position 0 for a `ctype_*` call, and `ConstArgs::not_text` carries the evidence of
the `ctype_*` argument.

**The preg family** (S5, `pattern_reads_locale` in `steins-catalog`'s `preg/locale.rs` and `preg/locale/scan.rs`, the
`PregPattern` kind of `setting_read_gate`). Every `preg_*` that compiles a pattern reaches one compiler, which asks the
tables `pcre2_maketables()` builds from the process locale once a script has called `setlocale`. The literal
pattern at position 0 (an array literal of string literals for `preg_replace`, `preg_replace_callback`,
`preg_filter`; the keys of the map for `preg_replace_callback_array`, carried as `ConstArgs::patterns`) decides
the call. Outside `u` and a leading `(*UCP)` (UCP, not UTF, is what leaves the tables: `(*UTF)` alone reads) a
pattern reads iff it holds `\w \W \s \S \b \B`, `[[:<:]]`, `[[:>:]]`, a POSIX class other than `[:digit:]` and
`[:xdigit:]`, or a name above ASCII. In every mode it reads iff it holds a caseless flag (`i`, `(?i…)`) and a
character that can match a letter (a letter, a high byte, a range spanning a letter, `.`, a negated class, `\w`,
`\D`, `\p{..}`, a POSIX class with letters, a numeric escape or a back reference in any spelling), the `x` flag over
a raw byte of `0x80..=0xFF` outside an `x` comment, `[[:ascii:]]`, or, under `(*UCP)` without `u`, a name above
ASCII. `\d`, `\h`, `\v`, literal bytes and ranges without a caseless flag, `\Q..\E` and a `preg_quote`d literal read
nothing, and an `x`-mode `#` comment is not scanned. A pattern the reader cannot parse as PCRE2 does, a pattern
that is not a literal, an array with an element that is not a string literal, a named or spread argument list and
the function handed over as a callback are the `value-dependent-read` gap and no label. The fold seam refuses a
`preg_match`, `preg_match_all` or `preg_split` whose literal pattern reads or that the reader declines
(`fold_reads_ambient_setting`); `preg_quote` still folds.

**The setting cells and the ini names** (S6-core, `SettingCell`, `ini_cell` and `narrowed_ini_labels` in
`steins-catalog`'s `setting.rs`). `SettingCell` is the roster of ADR-0101 §2.3 (`Locale`, `Precision`,
`Timezone`, `Env`, `Encoding`, `Ini`), each with the label pair `global.read.setting.<cell>` and
`global.write.setting.<cell>`; a call-decided gate names its cell (`SettingReadGate::cell`), and the effects
pass drops the label that cell spells. Seven labels join the registry with the first rows that colour them:
the reads of `timezone`, `encoding` and `ini` and the writes of `precision`, `timezone`, `encoding` and `ini`
(the precision read was S3's). The `env` pair joined the registry with its first rows, `getenv` and `putenv` (S6c). Registering a label is not inert: `effect.unknown-label` stops firing on it, did-you-mean suggestions can
offer it, and an interop docblock tag naming it binds as an envelope. The first rows to name those cells are the ini
functions with a **literal option name** (`ini_call`, `narrowed_ini_labels`): `ini_get` (one argument) reads the cell
that owns the name, `ini_set` and `ini_alter` (an alias of `ini_set`; it had no row, as `ini_restore` had none) with two
arguments read **and** write it (`zif_ini_set` returns the old value, via `zend_ini_get_value`, unconditionally), and
`ini_restore` (one argument) writes it, so `ini_set('precision', '3')` is
`{global.read.setting.precision, global.write.setting.precision}` where the argument-blind row is `{global.write}`.
The value an `ini_set` or `ini_alter` stores is converted to a string first, and a float goes through `precision`: on
the narrowed path the value is held to the three-way rule over S3's float evidence (`ConstArgs::float_evidence` now
also carries `ini_set` and `ini_alter`, `site/setting.rs`'s `ini_value_read`): a float adds the proven
`global.read.setting.precision`, a value shown no float adds nothing, any other is `value-dependent-read`. The names are exact
(the engine finds an entry by a case-sensitive lookup) and each is in php-src's table of entries that feed a
reader: `precision` and `serialize_precision` (precision); `date.timezone` (timezone); `iconv.{internal,input,output}_encoding` and
`mbstring.{language,detect_order,http_input,http_output,substitute_character,strict_detection}` (encoding);
`bcmath.scale`, `include_path` and `error_reporting` (ini). `default_charset`, `internal_encoding`, `input_encoding`,
`output_encoding` and `mbstring.internal_encoding` feed the encoding readers too and also reset the mb-regex
encoding; S6-core left them on the coarse row for that reason and S6d maps them to the encoding cell, which holds the
mb-regex state. A name no cell owns, a name the call
does not spell as a literal (a variable, a concatenation, a constant, a named or spread argument list), a count
that is not the function's own and an ini function handed over as a callback keep the coarse `global.read` or
`global.write`, which prefix subsumption already makes admissible wherever the cell is. `ini_get_all` and the
cells' builtin readers and writers are later slices'.

**The encoding readers** (S6d, `setting_reads/encoding.rs` in `steins-catalog`; ADR-0101 §3.13). The cell is the
default character set and the mbstring state a call reads without being handed it. The 42 functions whose generated
parameter list names an `encoding` or `from_encoding` and take it as an optional string carry
`global.read.setting.encoding` as an upper bound, and the gate (`SettingReadGate`, kind `Encoding`) reads the
argument at the position the generated table gives: omitted or `null` is the proven read; a literal name drops it
for the **plain** `mb_*` class (`mb_strlen`, `mb_strwidth`, `mb_strpos`, `mb_strrpos`, `mb_stripos`, `mb_strripos`,
`mb_substr_count`, `mb_strcut`, `mb_check_encoding`, `mb_chr`, `mb_ord`), and leaves it undecided for the 21
**substituting** ones, which rebuild the string under `MBSTRG(current_filter_illegal_substchar)` and the illegal
mode that `mb_substitute_character()` writes, so an invalid subject reads the cell whatever encoding is named; the
HTML functions also read for `''` (`determine_charset`) and `htmlspecialchars`, `htmlentities` and
`html_entity_decode` return before that on an empty subject, or one with no `&`, so their subject is a deciding
argument too; the iconv functions are undecided for `''`, `char`, `locale` and any name with a `//` suffix (the C library's own charset; glibc's `//TRANSLIT` consults the locale), and
`iconv_strrpos` returns on an empty needle first. The accessors (`mb_internal_encoding`, `mb_regex_encoding`,
`mb_http_output`, `mb_detect_order`, `mb_language`, `mb_substitute_character`) carry the read and the write: with no
argument or `null` the write is dropped, with any other argument the read is, and `mb_convert_encoding` and `mb_scrub` also write the cell (the illegal-character counter, `MBSTRG(illegalchars)`); `mb_regex_set_options` reads on
every call (it returns the previous options) and writes when given a string. `mb_ereg`, `mb_eregi`,
`mb_ereg_replace`, `mb_eregi_replace`, `mb_ereg_match` and `mb_split` read the cell on every call (they compile under
the mb-regex encoding and options), with no gate; `mb_ereg` and `mb_eregi` also have an out-parameter row for
`$matches`. `ConstArgs::literals` carries the literal arguments of a call whose spelling starts `mb_`, `iconv`,
`html` or `get_html`, at any position up to the fifth. `mb_detect_encoding`, `mb_get_info`, `mb_http_input`,
`mb_encode_mimeheader`, `mb_ereg_search*` (which keep a search state outside the cell) and `mb_ereg_replace_callback`
(which runs user code) are not coloured.

**The residue cell** (S6e, `setting_reads/ini.rs` in `steins-catalog`; ADR-0101 §3.16). The cell is the ini entries no
other cell owns: `bcmath.scale`, `include_path`, `error_reporting` and `max_execution_time`. The ten bcmath functions
with a `$scale` carry `global.read.setting.ini` as an upper bound, and the gate reads the scale at the position the
generated table names (`param_facts_generated.rs`; `bcsqrt` at 1, `bcpowmod` at 3, the rest at 2): omitted or `null`
is the read, and a value shown not to be `null` (a literal, a typed parameter, arithmetic) drops it, through the same
null evidence the time family uses (`ConstArgs::timestamps`, which a residue call now records too). A scale the scan
cannot show keeps the label, as the clock gate keeps `nondet.time`: no `value-dependent-read` gap is raised, because
the label is the upper bound there. `bcscale` and `error_reporting` return the old value, so they read on every call,
and they write only when the argument is given and is not `null`; both gate kinds are `Ini` in `SettingReadGate`.
`bcceil`, `bcfloor` and `bcround` take no scale and read nothing (witnessed), so they keep no row. `set_time_limit`
writes `max_execution_time` and reads nothing, and `set_include_path` reads and writes its entry. `ini_get_all` lists
every entry, so it reads the parent `global.read.setting` and no one cell. `ini_restore` and `ini_set` of a name the
call does not spell keep the coarse row (S6-core). The `@` operator sets `error_reporting` for the call it silences
without touching the ini entry, so `@error_reporting()` is `4437` while `ini_get('error_reporting')` is unchanged; the
operator is not coloured (D4), and the read of `error_reporting()` is the cell's either way. The other readers of `include_path` are not coloured either: `include`/`require` resolve through it, `fopen`, `file_get_contents` and the other stream openers read it with `$use_include_path`, and `stream_resolve_include_path` and `spl_autoload` read it; the openers keep `io.fs.read` alone and the last two still have no row.

`fprintf` and `vfprintf` still have no row. Both reads of a printf row are **conditional on the
call** (`'%d'` reads neither), so the row is an upper bound and not a claim about every call: the
effects pass reads a literal format and the arguments at the call site, proves, drops or gaps each
read (`site/printf.rs` in `steins-infer`, below), and never leaves a conditional read as a label it
cannot show (ADR-0101 §3.2); `strcoll` is a rowed name, so its `string` parameters are
held to the reach rule like any other coloured row. The fold seam refuses a
printf-family call whose literal format keeps the read (`fold_reads_ambient_setting`
in `steins-infer`'s `fold.rs`): the runner always answers under `LC_NUMERIC=C`,
and a fold of `sprintf('%.2f', 1.5)` would claim a locale the project never
declared. `sprintf('%d-%s', 1, 'a')` and `sprintf('%.2F', 1.5)` still fold.

A literal printf format refines the printf family's value positions
(`format_reach(format)` and `printf_family(name)`, issue #860, ADR-0021's
second 2026-10-01 amendment, the 2026-10-03 note). The row says `Object` at
`sprintf`'s values because a `%s` renders an object through `__toString`; a
literal format says which values that is. `format_reach` parses php-src's
`php_formatted_print` grammar (`%%`, `n$`, the flags `' '`, `0`, `-`, `+`,
`'c`, width, `.precision`, `l`) into one `ArgReach` per value: `Object` where a
conversion naming it is `%s`, `Inert` where every one is numeric (`d u c o x X
b e E f F g G h H`, which turn an object into a number with a warning), the
strongest where several name it, and `Inert` for a value no conversion names.
`None` leaves the call to the row: an unknown or missing conversion, a padding
quote with nothing after it, an argument number of zero or past `INT_MAX`, a
`*` width or precision, a `%` behind modifiers, a position past an internal
bound. The same parse gives a second verdict (`read_format` returns both,
`format_reads_locale` is the bit alone): a conversion ending in `f`, `g` or `G`
reads the locale, `F`, `e`, `E`, `h`, `H`, every integer and character
conversion, `s` and `%%` never do, and an unreadable format keeps the read, so
one walk of the bytes answers both questions and they cannot disagree about
which specs the format holds. `printf_family` names the format position (0 for `sprintf`, `printf`,
`vsprintf`, `vprintf`) and whether the values are one array, which is `Nested`
when some conversion is `%s` and `Inert` when none is. `fprintf` and
`vfprintf` have no row of any kind and are not in it. The catalog answers only for the
format it is given; the effects pass reads the call's literal format
(`ConstArgs::first`, a string literal with no interpolation) and does three things with
the answer (ADR-0101 §3.2 and D4, ADR-0021's 2026-10-03 note on call-site refinements):

- **Reach.** `reaches_user_code` maps `format_reach`'s per-value reaches back onto call
  positions (`PrintfFamily::reach_at`), so a position no `%s` names is `Inert` and a
  vector is `Nested` only when some conversion is `%s`. A format the parser cannot read, or
  that is not a literal, leaves the row.
- **The locale read.** A literal format with an `f`, `g` or `G` proves
  `global.read.setting.locale`; one with none drops it; a format that is not a literal, or that the
  parser cannot read, is the `value-dependent-read` gap and no label.
- **The `precision` read** is three-way at each `%s` of a literal format. A value **shown a float**
  proves `global.read.setting.precision`; values all **shown no float** drop it; any other value is
  the `value-dependent-read` gap, recorded beside any `user-code-reach` the value carries. A
  non-literal format, a callback use of a printf name and a vector's elements are the gap too. A
  position the call does not supply renders nothing (`ArgumentCountError`).
  The syntax layer records evidence per position for `sprintf` and `printf`
  (`ConstArgs::float_evidence`, a `FloatEvidence`) and `Frame::float_class` reads it, as `Yes`, `No` or
  `Unknown`: a float form (a float literal, an integer literal wider than `int`, a cast to `float`,
  their negation) is `Yes` and a no-float form (a string, integer, boolean or `null` literal, a
  concatenation, an interpolated string, a comparison, a cast to `int`, `string`, `bool` or `array`,
  an array literal) is `No`; a ternary or `??` is as its two branches agree; a by-value parameter is
  as its declared type says (`float` alone is `Yes` while the frame never writes it, a type with no
  `float` or `mixed` is `No`, anything else `Unknown`) and a local as the writes the scan carried
  say, each only while no named call may rebind it; a typed `$this->p` or `self::$p` is as its declared
  type says; a call is as its declared return says (a builtin's mined row, a project function or
  method's native hint); a global constant is as its value is (the catalog's table and
  ADR-0094's platform classes), and a class constant as its literal initializer is.

A literal `true` strict flag is the other call-site refinement of the reach rule
(`strict_flag_position`, `ConstArgs::bools`): `in_array` and `array_search` compare by identity,
which runs no `__toString`, so needle and haystack are `Inert`; no flag, `false` and a flag
that is not a literal `true` keep the loose comparison.

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
  this table and are detected structurally. So are `eval` (the `eval` label)
  and the four inclusion constructs (`io.fs.read`), each of which also taints
  exhaustiveness (ADR-0046 amendment).

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

The first family was `PDO::query`/`exec`/`prepare` and
`PDOStatement::execute`/`fetch`/`fetchAll`, all `io.db` (issue #67). That is the
first producer of a label the registry had carried since ADR-0018 with nothing to
emit it. `prepare` takes the same coarse colour as the rest: whether it is a
round trip to the server depends on PDO's emulated-prepares setting, which is
runtime configuration the catalog cannot read, so the row takes the upper bound.

`PDO::setAttribute` (`io.db`) and `PDOStatement::setFetchMode` (`mutate`: it
stores on the statement what the next fetch observes) are rowed
for what they **register** (issue #870). A class or object named by an earlier
call is constructed or written through by a later fetch, and an argument-reach
row cannot see it there, so the user code is attributed to the registration, as
ADR-0099 §4.5 attributes a handler: `setFetchMode`'s `FETCH_CLASS` name and
`FETCH_INTO` object, and `setAttribute`'s `ATTR_STATEMENT_CLASS` and
`ATTR_DEFAULT_FETCH_MODE` value, have `Autoload` reach in `method_arg_reach`,
and `fetch` and `fetchAll` stay as they are. So does `setFetchMode`'s `int
$mode`: `FETCH_CLASS | FETCH_CLASSTYPE` names no class at the call, a column
names it at each later fetch, and the reach rule does not read constants, so no
`setFetchMode` call is complete. A body that only fetches runs no user code by
that route, and the body that registered the class holds the gap.

Constructor rows (`__construct`, issue #804) are what `new C(...)` and a
subclass's `parent::__construct(...)` run: `PDO` is `io.db`, `DateTime` and
`DateTimeImmutable` are `nondet.time`, and every engine `Throwable`, the SPL
containers, `ArrayObject`, `WeakMap`, `DateInterval` and `stdClass` are pure. The
`Throwable` accessors (`getMessage`, `getCode`, `getFile`, `getLine`,
`getPrevious`, `getTrace`, `getTraceAsString`, issue #847) are pure on every
engine `Throwable`; `__toString` has no row. The static
`DateTime::createFromFormat` and `DateTimeImmutable::createFromFormat` take the
constructors' `nondet.time`, as their function spellings do in `effect_labels`
(issue #848), and the copying factories (`createFromImmutable`,
`createFromMutable`, `createFromInterface`) are pure.

A consumer reaches a row through a project subclass by walking the class's chain
until it leaves the project, provided no project class on the way declares the
method or uses a trait. A receiver that names its class exactly (`new Foo`,
`Foo::`, `parent::`) may use any row found that way. One that names only a bound
on its runtime class (`$this`, `self::`, a declared parameter or property) may
use a row only when `final_method_effect_labels(class, method)` answers, which
holds only for methods the engine declares `final`, so that no subclass can
replace the body. Today that means the `Throwable` accessors. The
`Throwable` interface qualifies too, because PHP refuses a class that implements
it without extending `Exception` or `Error`.

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
transitively, and the throw lane with it: catch absorption, `@throws` coverage
and the checked/unchecked split all read this table past the project. A name
absent from the table is an unknown external → `Unknown`, never `No`.

**Builtin enums are deliberately omitted**: the mining data for their implicit
interfaces and backing is incomplete, and an incomplete row would produce a
wrong `No`.

### `class_aliases_generated` — a class's second name

A class-level `/** @alias X */` in a stub gives one class entry two names
(`DOMException` is also `Dom\DOMException`, the only one at `php-8.5.11`). The
miner emits it as `aliases = [...]` on the declared class's row and `gen-catalog`
renders `(lowercased second name, declared name)` pairs. The second name is **no
row** of the hierarchy or display tables: `builtin_class_supers` and
`builtin_class_display` answer through `builtin_class_alias`, and `Cx::class_identity`
resolves the pair to one identity before the is-a oracle compares names (ADR-0043,
class identity is resolved, not spelled).

## `builtin_throws(name)`

`builtin_throws` gives the throw classes a builtin can raise. The rows are
per name and hand-transcribed from `throws.toml`; the fold-allowlist names with
an input-determined `ValueError` arm (`str_repeat`, `count`, `sprintf`, …)
carry theirs since issue #320, each reproduced by probe.

## `knows(name)` — what is a builtin

One predicate (ADR-0099 §3.1, issue #864) for "the catalog knows this spelling":
the union of the effect colours, the out-parameter rows, the by-value
certification (which includes the mined arginfo), the mined parameter facts, the
invocation shapes, and the two call-site certified lists. Every pass that
resolves a function name asks it, so a project function shadowing a builtin
spelling is ambiguous for all of them or for none. The shadow check
(`Cx::resolve_shadow`, which answers "no catalog at all") and the
runtime-existence questions (`function_exists` folding, the absence family) keep
their own tables.

Knowing a name says nothing about which axes have a row: a known name with no
row on an axis is a coverage gap there, never pure and never throwless.

## `throws_of(name)` — what a call raises

The throw lane's one composition (ADR-0099 §3.2, §3.3): the `builtin_throws` row
if there is one, an empty row for a name on the **audited throwless table**, and
`None` otherwise. `None` is *unknown* and the lane reads it as a gap. No
pass reads `builtin_throws` itself (a test in `steins-infer` pins that).

The table lists the builtins php-src shows raise nothing for any argument their
parameter types admit, argument checking's `TypeError` and `ArgumentCountError`
aside. The fold allowlist and the certified lists are candidates, not evidence.
Each name was audited by reading its body and by a runtime witness on PHP 8.5.11
(12,000 argument tuples per name, drawn from every parameter's admitted values);
the module doc of `knowledge.rs` states the method and what it refused.
A third, mechanical witness is the generated note
`docs/research/phpsrc-mining/throwless_audit.md` (issue #881):
`audit_throwless.py` beside it resolves each name through the `*_arginfo.h`
entry tables (aliases and the `FileFunction` macro family included) to its C
function at the pinned php-src, lists the raise calls its call graph reaches and
classifies the name as `none`, `argument-checking`, `destructor-hazard` or
`needs-row`, and a test holds the table to that note. It reads the `php-8.5.11`
tag, enters function-like macros, and records a call through an object handler
pointer, which no static walk resolves, as a name that needs a review where the
reach table calls the operand `Inert`; two fuzzes beside it
(`fuzz_throwless_values.php`, `fuzz_throwless_uninit.php`) are the run-time
witness. The audit found fourteen names that raise an `Error` for a value their
types admit, an array that contains itself by reference, a `DateTimeZone`
subclass that never ran its parent's constructor, or a `SimpleXMLElement`
subclass instance made without its constructor, and they carry a row instead
(`in_array`, `array_search`, `array_keys`, `array_unique`, `sort`, `rsort`,
`asort`, `arsort`, `array_replace_recursive`, `array_walk_recursive`,
`date_create`, `date_create_immutable`, `boolval`, `array_filter`). Two
consequences are call-site rules, not table facts: a call below the arity that
carries the value raises less (`throws_at_arity`: `array_keys($a)`,
`date_create('now')`, a `*_from_format` without its time zone), and a call whose
arguments are all flat literals cannot pass such a value (`throws_of_literals`).
A raise that php-src's development branch adds after 8.5 (`array_filter`'s
`$mode`, `pathinfo`'s `$flags`, the stream error mode) is not visible to the
audit and has no row.

Converting an object argument to a string raises an `Error`, and that is not the
table's business: every position that can hold an object is an `ArgReach`
position, and the resolver turns it into a gap of its own unless the call site
rules the object out.

`json_encode` and `json_decode` raise only under `JSON_THROW_ON_ERROR`
(`flag_gated_throw(name)`). Their throws are the flag-free ones when the call's
flags argument is absent or a constant expression the scan evaluates without the
flag, and a `flag-dependent-throw` gap otherwise.

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

  **The parameter-type half is over too, and it never becomes a catalog row**
  (issue #423, ADR-0056 §9). The same `reflect` reply carries `params` —
  `ReflectionFunction::getParameters()` per position, with the parameter name, the
  `(string)` rendering of `getType()` and the `by_ref`/`variadic`/`optional` bits —
  reachable through `steins_infer::Folder::builtin_param_types`, and
  `type.argument-mismatch` plus the possibly pair consume it at a builtin call
  site. That surface is deliberately **engine-only**: a parameter type premises a
  proof-layer finding on the default floor, and ADR-0069 §2's firewall forbids an
  imported row from carrying that authority (the ADR-0069 note of 2026-08-17 states
  the whole argument). So the "no parameter types" sentence above stays true of the
  catalog and always will be, and `--no-php` judges no builtin argument at all.
- **A proven `JsonException`.** `json_decode`/`json_encode` throw `JsonException`
  only under `JSON_THROW_ON_ERROR`. The flag is read (`flag_gated_throw`), but a
  call that sets it is a coverage gap rather than a proven throw: the synthetic
  `json_*_throwing` keys are present in the source, awaiting the decision to
  state the class.
- **Plugin-registered ecosystem labels and signatures** (ADR-0012, ADR-0039).
