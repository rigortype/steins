# A locale read is an effect, decided lexically: ambient settings, their labelled reads and writes, and the `%F` remedy

**Status: proposed (2026-10-03), PENDING ratification.** Designed by the
architect under the owner's standing delegation. Three owner rulings of
2026-10-03 shape it and are recorded as rulings, not derivations: **O1** a
locale read is an effect and is labelled as one, and no "pure modulo ambient
state" posture is adopted; **O2** the printf family is settled lexically, with
`%F` recommended where `%f` reads the locale; **O3** what the PHPStan post
"Remembering and forgetting returned values" teaches is a fact about *values*
(a call result remembered as a literal until an invalidating call), not an
effect-lane convenience. Tracking issue #991; the value-side design this ADR
scopes is ADR-0102 (§5).

**Owner decisions (2026-10-03).** D1 the labels are `global.read.setting.<cell>` /
`global.write.setting.<cell>`, grouped under `setting`; D2 `transform effects-envelope`
writes a function whose only effect is a setting read as `@phpstan-impure
global.read.setting.locale`, like any other label; D3 the `%F` remedy is a fix-it on the
envelope findings only, with no bulk transform; D4 the `precision` cell is registered and
only builtin readers are coloured, operator sites waiting for ADR-0008's opt-in (recorded
in `not-implemented.md`); D5 ADR-0102 follows slices S1–S2 and is independent of S4–S6;
D6 `setlocale($c, '0')` narrows to the read in S4 with the other call-site narrowings.

ADR-0021 Decision 2 certifies a builtin pure only when php-src shows it "reads
only its arguments: no ini setting, locale, clock, environment, superglobal,
engine symbol table or error state". `sprintf` and `printf` reached the pure
row through the fold allowlist, which predates that bar, and their `%f`, `%g`
and `%G` conversions read `LC_NUMERIC`. This ADR names the state those
conversions read, gives it labels, decides a literal format lexically, and
amends Decision 2 so that a builtin which reads a setting and nothing else is
catalogued with that read rather than refused.

## 1. Context

### 1.1 The hole

Issue #991, found in the review of #990: `function f() { return sprintf('%f',
1.5); }` is exhaustive `{}` on master. Witnessed on PHP 8.5.11 (and 8.1.32):

```php
setlocale(LC_NUMERIC, 'de_DE.UTF-8');
sprintf('%f', 1.5);   // "1,500000"   reads the locale
sprintf('%g', 1.5);   // "1,5"        reads the locale
sprintf('%G', 1.5);   // "1,5"        reads the locale
sprintf('%F', 1.5);   // "1.500000"   does not
sprintf('%e', 1.5);   // "1.500000e+0"
sprintf('%h', 1.5);   // "1.5"        does not (8.0+)
```

php-src agrees with the witness. `php_sprintf_appenddouble`
(`ext/standard/formatted_print.c`) hands `php_conv_fp` the decimal point
`(fmt == 'f') ? LCONV_DECIMAL_POINT : '.'`, so `%F`, `%e` and `%E` never
consult the locale; the `g`/`G`/`h`/`H` arm sets `decimal_point = '.'` and
overrides it with `LCONV_DECIMAL_POINT` only `if (fmt == 'g' || fmt == 'G')`.
`vsprintf`, `vprintf`, `fprintf` and `vfprintf` share the formatter. #990 left
`vsprintf` uncertified for this reason and filed `sprintf`'s hole as #991.

### 1.2 What the catalog already knows, and what it refuses

The writers of this kind of state are catalogued, coarsely:
`setlocale`, `ini_set`, `putenv`, `date_default_timezone_set` and
`mb_regex_encoding` are `global.write` (`crates/steins-catalog/src/effects.rs`,
the `GLOBAL_WRITE` arm beside the handler registrations), as are `srand`,
`mt_srand` and `clearstatcache`. Three readers are `global.read`: `getenv`,
`ini_get`, `date_default_timezone_get`. ADR-0055 Part I already says what
`global.*` names: "the interpreter-global surfaces (superglobals, ini, env)".
ADR-0008's "pseudo-constant settings" paragraph planned an opt-in that would
*remove* `global-read` of set-once settings so the "(B)-class" functions could
fold; the opt-in was never built (`not-implemented.md`, "Locale/timezone
pseudo-constants").

Everything else that reads a setting is refused rather than coloured.
ADR-0021's second amendment §5 and the doc comment on `CERTIFIED_AT_CALL_SITE`
list `basename`, `pathinfo`, `strnatcmp`, `strnatcasecmp`, `substr_compare`,
`parse_url`, `escapeshellarg`, `strip_tags`, `number_format`, the `ctype_*`
family, `htmlspecialchars` and the `mb_*` family, "each reading the locale or
an ini setting". A refused name is uncatalogued, and an uncatalogued call
leaves its body `…?`. So the posture before this ADR was inconsistent in both
directions: a name that reads the locale on every call (`ctype_alpha`) kept
every caller non-exhaustive, while a name that reads it on some formats
(`sprintf`) was pure.

### 1.3 The PHPStan post, read for what it says

The post describes a mechanism over *values*: a call's narrowed return value
is remembered and a later identical call is assumed to return the same, until
an impure call forgets it; its closing example is that repeated `is_dir($x)`
"will not look at the filesystem unless you call `clearstatcache()` between
the calls". Purity there is the trigger for forgetting, not the subject. The
owner's ruling O3 keeps the two apart: the effect lane says what a call
*does*; whether a call's result may be *reused* is a question for the value
domain, with the effect labels supplying the invalidation set. §5 scopes it.

### 1.4 Witnesses

PHP 8.5.11 unless noted; `C` against `de_DE.UTF-8`; the scripts are in the
ADR's witness directory. Stable means the result did not move.

| reads the locale | stable at 8.5 |
| --- | --- |
| `sprintf`/`vsprintf` `%f` `%g` `%G` (any flags, width, precision, `n$`) | `%F` `%e` `%E` `%h` `%H` `%d` `%s`, and `%%` |
| `strcoll`, `localeconv`, `setlocale(LC_x, '0')` | `number_format` (also at 8.1; php-src renders with `%.*F`, `ext/standard/math.c`) |
| `ctype_*` on a byte ≥ 0x80, `strftime`, `sort(…, SORT_LOCALE_STRING)` | `(string) $float`, `strval`, `var_export`, `json_encode`, `serialize`, `round` |
| `preg_match('/\w/', "\xE4")` without `/u`, once a `setlocale` has run (`php_pcre.c`: tables are rebuilt from `BG(ctype_string)`, which only `setlocale` with `LC_CTYPE` or `LC_ALL` writes) | `strtoupper`, `strtolower`, `ucfirst`, `ucwords`, `strcasecmp` (8.2+; **8.1 reads it**, witnessed) |
| | `basename`, `pathinfo`, `escapeshellarg`, `strip_tags`, `parse_url`, `strnatcmp`, `substr_compare` on this platform; php-src reads `php_mblen`/C `isdigit` etc., so the row stays on the php-src evidence (§4) |

Two other process facts the design rests on. The PHP CLI resets only
`LC_CTYPE` at startup (`main/main.c` calls `zend_reset_lc_ctype_locale`, which
sets `C.UTF-8` or `C`); `LC_NUMERIC` stays `C` whatever the environment
exports (witnessed with `LC_ALL=de_DE.UTF-8` in the environment:
`setlocale(LC_ALL, '0')` answers `C`). And the sidecar runner
(`crates/steins-sidecar/runner.php`) sets `display_errors`, `log_errors`,
`error_log` and `memory_limit` and never calls `setlocale`, so every fold runs
under `LC_NUMERIC=C`.

## 2. Decision: an ambient setting is a cell; its read and write are labels

### 2.1 The concept

An **ambient setting** is a cell of process-owned state the script is born
holding, that only the script's own calls rewrite, and that a builtin reads
implicitly rather than through an argument: the locale, the default timezone,
the environment block, an ini value. Three properties separate it from its
neighbours in the taxonomy, and each is a property a consumer tests:

- Against `nondet.*`: a repeat read of a setting with no intervening write
  returns the same value. A clock or a random draw does not — the draw itself
  advances the generator, which is why `mt_rand` after `mt_srand(42)` is
  deterministic as a *sequence* and still `nondet.random` as a *call*: two
  draws with no write between them differ (witnessed).
- Against `io.*`: nothing outside the process writes a setting. The
  filesystem, the network and a database are written by the world, so a repeat
  read can differ with no write by this process. ADR-0083's **ambient
  channels** (`io.output`, `io.input`) are also born-held, which is why the
  word "ambient" is shared, and they are channels to that world, which is why
  they stay under `io`.
- Against `global.read` as it stands: `$_GET`, `$_SESSION`, `$GLOBALS` and
  `global $x` are the script's *variables* (ADR-0055's 2026-07-24 amendment),
  parsed memory a request fills. They are not settings, and this ADR does not
  move them.

A **setting read** is the effect of reading a cell; a **setting write** is the
effect of rewriting one. Glossary text is in the companion file.

### 2.2 The labels

```
global.read.setting              a read of some setting (a dynamic ini name)
global.read.setting.locale       LC_* as setlocale leaves it
global.write.setting
global.write.setting.locale
```

Four registry entries land with slice 1. The cell roster below names the
later children (`timezone`, `env`, `encoding`, `precision`, `ini`); each is
registered in the slice that colours its first row, never ahead of one, which
is ADR-0083's "reserved with no rows" case inverted: a label with no row is
noise in the registry table and a declaration nobody can discharge.

Prefix subsumption carries every existing consumer unchanged:

- a declared `global.read` or `global` envelope admits a setting read, and a
  declared `global.write` admits `setlocale`'s narrower row;
- ADR-0096's `Discardable = { global.read, … }` covers the read by prefix, so
  `sprintf('%f', 1.5);` is still a dead statement, which is right: throwing a
  locale read away leaves the world as it was;
- ADR-0046's read-shaped family (`nondet.*`, `global.read`, `io.fs.read`) gains
  the child without a new rule.

`global.read` without a child remains the row for a read the catalog cannot
place in a cell. Nothing is re-coloured *up* to the parent.

### 2.3 The cell roster

Each row names the state, who reads it, who writes it, and whether it is a
setting at all. Verified on PHP 8.5.11 and in php-src at the pinned commit.

| cell | readers | writers (the reset) | verdict |
| --- | --- | --- | --- |
| **locale** (`LC_*`) | printf family under `f`/`g`/`G`; `localeconv`; `strcoll`; `nl_langinfo`; `ctype_*`; `strftime`/`gmstrftime`; `sort` family under `SORT_LOCALE_STRING`; `preg_*` without `/u`; `basename`, `pathinfo`, `strnatcmp`, `strnatcasecmp`, `substr_compare`, `parse_url`, `escapeshellarg`, `strip_tags` by php-src; `setlocale(LC_x, '0')` | `setlocale` with a locale name | setting; **slice 1** |
| **timezone** | `date`, `mktime`, `strtotime`, `idate`, `getdate`, `localtime`, `date_create*` and `new DateTime` for a string naming no zone, `date_default_timezone_get` — each only with an explicit timestamp or string, since omitting it also reads the clock; `gmdate`/`gmmktime` with a timestamp and `checkdate` read nothing (witnessed) | `date_default_timezone_set`; `ini_set('date.timezone')` only while no `date_default_timezone_set` has run (witnessed: the function's slot wins) | setting; later slice. Today the whole family is `nondet.time` argument-blind (`effects.rs`, the time family arm) — an upper bound this cell can sharpen |
| **env** | `getenv` | `putenv` | setting; later slice. `$_ENV` is a startup snapshot the two never touch (witnessed), and stays a superglobal read |
| **encoding** (`default_charset`, `mbstring.internal_encoding`) | `mb_*` without an explicit encoding, `htmlspecialchars`/`htmlentities` without one, `iconv_*` | `mb_internal_encoding($e)`, `ini_set` of either name | setting; later slice |
| **precision** (`precision`, `serialize_precision`) | `(string)`, `strval`, `implode`, `print_r`, `%s` of a float read `precision` (`1234.5678` renders `1.23E+3` at `precision=3`); `var_export`, `json_encode`, `serialize`, `var_dump` read `serialize_precision` | `ini_set`, `ini_restore` | setting; **operator sites read it** — decision D4 below |
| **ini**, the residue (`bcmath.scale`, `include_path`, `error_reporting`, …) | `bc*` without a scale, `get_include_path`, `error_reporting()`, `ini_get` | `bcscale`, `set_include_path`, `error_reporting($l)`, `ini_set`, `ini_restore` | setting; later slices, one ini name at a time |
| RNG state | every draw | `mt_srand`/`srand`, and **every draw** | not a setting: the reader writes, so the stable window is empty. Stays `nondet.random`; the seeders stay `global.write` |
| stat cache | `stat`, `lstat`, `is_dir`, `is_file`, `filesize`, `filemtime`, … for the **last path stat'd** (one entry each for stat and lstat, `ext/standard/filestat.c`); `file_exists`, `is_readable` etc. go to `access(2)` and are never served | `clearstatcache`; every plain-stream read, write and flush (`main/streams/plain_wrapper.c` clears it "as atime/mtime got changed"); every fs mutation; a stat of **any other path** evicts | not a setting: the world writes the file, and the cache is a one-entry engine artifact. Stays `io` (narrowed to `io.fs.read` at a proven target). See §5.4 for what this means for remembering |
| error and exception handlers | a raised diagnostic; `set_error_handler` returns the previous one | `set_*_handler`, `restore_*_handler` | registration state: ADR-0021 Decision 3 attributes a handler's effects to its registration; the "read" runs user code. Stays `global.write` |
| autoload stack | every class lookup by name | `spl_autoload_register`/`unregister` | reach, not a read: ADR-0021's `Autoload` reach kind and ADR-0099's gap already answer. Stays `global.write` |
| array internal pointer | `current`, `key`, `next`, … | `next`, `reset`, … | state **of the value**, not of the process (a copy has its own pointer, witnessed). ADR-0098's place vocabulary, not this family |

### 2.4 Rows re-coloured in slice 1

| name | row before | row after |
| --- | --- | --- |
| `sprintf`, `vsprintf` | `{}` (via the allowlist; `vsprintf` uncatalogued) | `{global.read.setting.locale}` |
| `printf`, `vprintf` | `{io.output.buffer}` | `{io.output.buffer, global.read.setting.locale}` |
| `fprintf`, `vfprintf` | none | none; a stream writer's row is issue #989 |
| `setlocale` | `{global.write}` | `{global.write.setting.locale}` |
| `localeconv`, `nl_langinfo` | none | `{global.read.setting.locale}` |
| `strcoll` | none | `{global.read.setting.locale}` (a call-site certified `string` pair, ADR-0021 §3) |

`setlocale(LC_x, '0')` is a query. Narrowing it to the read at a literal `'0'`
is cheap and is listed as D6; the argument-blind row is the write.

## 3. Decision: the printf family is decided lexically

### 3.1 The rule

For a **literal** format the row's locale read is kept or dropped by the
format alone: it is kept iff some conversion spec ends in `f`, `g` or `G`.
`F`, `e`, `E`, `h`, `H`, every integer and character conversion, `s`, and
`%%` never read the locale. A format the parser cannot read as the engine does
(a `*` width or precision, an unknown conversion, a bare `'` pad, a position
of `0` or past the bound — the `None` cases of ADR-0021's 2026-10-03 note)
keeps the row. A **non-literal** format keeps the row. The verdict is over the
whole format, never a prefix, for the reason that note gives: a malformed
format may have rendered an earlier spec before it fails.

The rule is a second verdict of the same parse. `format_reach`
(`crates/steins-catalog/src/reach.rs`) already matches the conversion letter
of every spec to decide `Object` versus `Inert`; it grows a `reads_locale`
bit, so one walk of the bytes answers both questions and the two can never
disagree about which spec they saw. Witnessed with the parser's own fuzz
posture in mind: over 1,488 (spec, value) pairs across `%`, `%.0`…`%.100`,
widths, `+`, `0`, `-` and `'*` padding, the `f`/`g`/`G` forms moved under
`de_DE` in 308, 266 and 266 cases and `F`/`h`/`H` in none; `e`/`E` moved in
none; `%s` of a float and `number_format` in none.

### 3.2 Where the verdict is read

At the call site, in the effects pass, at the point where the same literal
format's reach verdict is read — the engine half of #860 (S7-engine), which
has not landed: `format_reach` has no reader in `steins-infer` today, and
`arity.rs`'s `printf_family_shape` is ADR-0078's slot counter. Until that seam
exists, the row's label stands at every printf call, which is the sound side
(a call reads the locale unless shown not to). When the seam lands, a literal
`'%d-%s'` drops the read and is pure again, and `'%.2f'` keeps it.

### 3.3 The fold

`sprintf` stays on the fold allowlist; the allowlist is permission, not a
promise (`crates/steins-catalog/src/fold.rs`). The fold seam asks
`steins_catalog::foldable(name)` (`crates/steins-infer/src/cx.rs`, the two name
gates of `try_fold` and its lane enumerator) and already reads a printf
family's literal format at the same seam for budgeting
(`fold_budget.rs`, `FoldAllocation::Format { format: 0, values: 1 }`). The
fold gains one refusal there: a printf family call whose literal format keeps
the locale read does not fold. ADR-0008's folding gate — an expression folds
only if all colours are empty — is thereby applied to the call rather than to
the name, which is the reading ADR-0021's first amendment already gave the
allowlist ("pure given literal arguments").

Folding `sprintf('%f', 1.5)` under the runner's `LC_NUMERIC=C` was considered
and declined (§8). A fold is a claim about the project's runtime, and the
project has declared nothing about its locale; ADR-0008's pseudo-constant
opt-in is the only road to that fold and it is not built here. ADR-0066's
`TableFolder` consults no ambient state and replays what the table recorded,
so the browser twin needs nothing.

### 3.4 A native transfer that assumed the C locale

`transfers/str_preds.rs` forces `StrPreds::NUMERIC` on a `sprintf`/`vsprintf`
result whose whole format is one `%e`, `%f` or `%g` conversion with a value
shown to be an `int`. Witnessed: `sprintf('%f', 1)` is `1,000000` under
`de_DE.UTF-8` and `is_numeric` of it is `false`; `sprintf('%g', 1000000)` is
`1,0e+6`. The `%e` leg is right. The `f` and `g` legs are the same hole as
#991 in the value lane, and are closed in slice 1 by holding the arm to the
set of §3.1: `e`, `E`, `F`, `h`, `H` may force `NUMERIC`; `f`, `g`, `G` may
not. `b`/`d`/`o` are unaffected.

### 3.5 Envelopes do not admit the read

A `#[\Steins\Pure]` body that keeps a locale read is `effect.envelope-exceeded`
at the printf site, and so is an interop `@phpstan-pure` body (ADR-0082 §2:
reading the tag is taking it as a checkable claim). PHPStan itself has no
locale concept and accepts `sprintf` under its purity model, so this is a
finding Steins can make and PHPStan cannot, not a divergence of imported type
semantics; it is recorded in the interop spec's informative section, not in
ADR-0030's registry. A project that wants the old silence has ADR-0084:
`tolerated = ["global.read.setting"]` discharges the read at judgment and
`--no-tolerated-effects` shows it again. No built-in tolerance is added and
no new mechanism is needed, which is why O1 costs nothing to implement.

### 3.6 The remedy: a fix-it to the locale-independent conversion

The finding carries a **fix-it** (ADR-0010's `Fix { title, edits }`,
`crates/steins-infer/src/project.rs`; one fix title ships today) when its
origin is a printf family site with a literal format: every spec ending in `f`
becomes `F`, `g` becomes `h`, `G` becomes `H`, in place, title "use the
locale-independent conversion". The edit is byte-identical in effect under the
`C` locale (§3.1's 1,488 pairs, zero mismatches, `%%f` untouched because it is
not a spec, `%1$.2f` → `%1$.2F`), and under any other locale it changes the
output from the locale's decimal point to `.` — which is the point, and the
message says so. `h`/`H` exist since PHP 8.0, inside ADR-0011's floor.

The fix-it rides `effect.envelope-exceeded` and `effect.liskov-widened`, both
strata. It is not a standalone id over every `%f` in a project: without an
envelope nothing is claimed about the function, and ADR-0017 keeps style out
of Steins. A bulk `transform printf-locale-free` for a project that wants
every format moved is cheap on the same parser and is D3.

### 3.7 Consumers that keep their answer

- `statement.no-effect`: `sprintf('%f', 1.5);` reports (§2.2).
- `loop-to-array-map`: a body with a setting read refuses with `body-effects`
  today (ADR-0076 §2.1, proven lane empty) and keeps refusing. The equivalence
  argument — both spellings run the body the same number of times in the same
  order, and `fn` captures no setting — is recorded here for a later measured
  amendment and not adopted: O1 forbids reading the label as harmless, and
  the purity cells of that transform have never fired on the corpus (its
  2026-08-07 amendment), so there is nothing to buy.
- Exhaustiveness: a setting read is a known label and never a gap. The
  opposite movement is the win: a reader refused under ADR-0021 §5 becomes a
  known builtin when its slice lands, and its callers stop being `…?`.

## 4. Decision: ADR-0021 Decision 2 is amended

Decision 2's bar — "reads only its arguments: no ini setting, locale, clock,
environment, superglobal, engine symbol table or error state" — remains the
bar for an **empty** row, and "certified pure" keeps exactly that meaning. No
"certified pure modulo X" vocabulary is introduced. What changes is the
disposition of a name that fails the bar *only* on a setting: it is
catalogued with the setting-read row instead of being left uncatalogued. The
refusal list of ADR-0021's second amendment §5 therefore becomes a queue for
slice S4, each name entering with its witness or php-src line, and the
call-site rule (arg reach) is unchanged and orthogonal: `sprintf('%f', $o)`
has `Inert` reach and reads the locale.

`number_format` leaves that list on its own evidence: it reads no setting at
8.1 or 8.5 (`_php_math_number_format_ex` renders with `%.*F`), so it is a
call-site certified `string`-parameter name under ADR-0021 §3, with nothing
from this ADR on its row.

#991 is resolved as: `sprintf('%f', $x)` is `{global.read.setting.locale}`
and not exhaustive-pure; `sprintf('%d-%s', 1, 'a')` is `{}` once S7-engine
reads the literal; a dynamic format keeps the read; `vsprintf` and `vprintf`
follow the same rule; `printf` keeps its output label beside it; a pure
envelope over the read reports and offers `%F`.

## 5. Decision: remembered call results are a value-dimension concept

### 5.1 The concept

A **remembered call result** is a call's result held as a fact on the call
itself — the callee, its argument places and the receiver place — so that a
later call with the same key, in the same scope, with no invalidating site
between them, answers the remembered fact rather than the row's return type.
`if (is_dir($d)) { … is_dir($d) … }` reads `true` in the branch;
`setlocale(LC_ALL, '0')` twice in a row answers once. It is PHPStan's
mechanism named in Steins' vocabulary, with one change: the invalidation set
is derived from effect labels rather than from a purity bit.

### 5.2 What Steins has today

Narrowing acts on variables and properties (ADR-0052's guard facts, ADR-0070's
by-value survival, the branch-scoped forgetting of `branch.rs`). A call
expression is not a subject: `operands.rs` files "a call result" under
everything else, and nothing in `steins-infer` remembers one. A call with
literal arguments is answered by the fold, which executes it; `is_dir($p)`
with an unknown `$p` is `bool` and a second `is_dir($p)` is `bool` again. So
the concept is new machinery in the value lane, not a refinement of an
existing seam.

### 5.3 Relation to the value domain and to trust

A remembered result is a fact in ADR-0035's four layers on a new subject kind,
the call key: a guard narrows it (`Refined`/`OneOf`), a fold pins it
(`Singleton`), and widening is layer descent as for any fact. Its stratum is
Verified in ADR-0037's sense with a stated condition: it is a proof about
**what the engine returns**, not about the world. The stat cache witness is
the exact case: after an external `rmdir`, `is_dir($d)` on the last stat'd
path still answers `true` from the cache (PHP 8.5.11, Darwin), while
`file_exists($d)` answers `false` because it never consults the cache. A
remembered `is_dir` is therefore right about PHP and wrong about the disk,
and a finding built on it ("this branch is dead") is a proof-layer claim
only if the predicate is about the engine. ADR-0102 has to price that under
ADR-0002.

### 5.4 The invalidation set, derived from labels

This is what the effect lane supplies, and it is decided here. For a call
whose callee row is `R`, a remembered result is forgotten at the first later
site in program order (ADR-0099's site list) that:

1. carries `global.write.setting.X` for any `global.read.setting.X` in `R`,
   or the coarse `global.write.setting` or `global.write` — the cell was reset;
2. carries any `io*` label, when `R` is the stat family (`io` narrowed to
   `io.fs.read` at a proven target): a plain-stream read, write or flush
   clears the cache, a stat of any other path evicts the one entry, and
   `clearstatcache` is `global.write`. This is stricter than PHPStan, which
   would keep `is_dir($p)` across `is_dir($q)`; the engine does not;
3. is a coverage gap (`…?`) or an escape hatch (`eval`, `ffi`): ⊤ to any
   consumer asking what a call could touch (ADR-0046);
4. writes an argument place or the receiver place: an assignment, a by-ref
   pass, a `mutate*` label on the receiver, an `unset`;
5. — and two rows are never remembered at all: any `nondet.*` row (the draw
   writes), and any `io.*` row other than the stat family (the world writes).

A row of `{}` is remembered until 3 or 4. A row of `{global.read.setting.X}`
until 1, 3 or 4. The stat family until 2, 3 or 4, and whether to remember it
by default at all is ADR-0102's zero-FP call (§5.3).

### 5.5 A separate ADR

The value side is ADR-0102, not a later slice of this one: it adds a subject
kind to narrowing, a key, scope rules, the interplay with ADR-0036's heap and
ADR-0098's places for receivers and argument places, the parity decision
against PHPStan's `rememberPossiblyImpureFunctionValues`, and the
calibration question of §5.3. Each of those is larger than anything in §2–§4.
ADR-0101 owns the cells and the derivation in §5.4; ADR-0102 consumes them.

## 6. Measurement

Proxies by grep over the ten public corpus packages (a slice measures the
real thing with the instrumented ranking of ADR-0021):

- printf family: 1,476 calls; 753 with a literal format; **10** with an `f`,
  `g` or `G` conversion (PHP-Parser's pretty printer `%.16G`/`%.17G`,
  Composer's `%.3f` timings and `%.1fMiB`, symfony/console's `%.1f`), 2 with
  `F`/`e`/`E` only. So slice 1 moves about ten bodies from `{}` to
  `{global.read.setting.locale}` and no body's exhaustiveness.
- `setlocale`: 38 calls, one outside tests, and that one a query
  (`setlocale(LC_TIME, '0')` in Carbon's translator), so no public body
  carries the write.
- queued readers (S4/S5): `ctype_*` 65 calls in 7 non-test files, `mb_*` 88 in
  27, `getenv` 148, `ini_get` 71, `basename` 50, `preg_*` with a literal
  pattern 341, of which 22 carry `u`. These are the exhaustiveness releases the
  later slices buy and must measure.

Gates a slice must pass, against its base: `check` default and strict
byte-identical on the ten packages (no public envelope covers a printf site);
the effect baseline diff naming exactly the printf bodies; `transform
effects-envelope` tags: a function whose only effect was the locale read now
writes `@phpstan-impure global.read.setting.locale` where it wrote nothing
or a class-level pure tag (D2); `transform throws-envelope` byte-identical.

## 7. Consequences

- The registry grows four entries and the spec's label table with them;
  `effect.unknown-label` suggestions reach them by distance.
- `effect_labels` answers a coloured row for `sprintf` ahead of the allowlist's
  empty one (the `colored.or_else(…)` order already does this); the test
  `foldable_builtins_are_catalogued_pure` keeps its five names and gains the
  reading that a foldable name may carry a setting read. `effects-envelope`
  and `annotate` show the label plainly, no marker.
- A frozen generation written before the rows answers the old `{}` for a
  printf body; the analyzer fingerprint moves and refuses it (ADR-0092's
  amendment), no schema bump.
- The native `NUMERIC` transfer stops answering for `%f`/`%g` with an int
  (§3.4): a value-lane precision loss on the corpus of at most the ten sites
  above, in exchange for a sound answer under every locale.
- Deferred, with the evidence that forced the deferral recorded:
  - the preg family's locale tables (S5): the biggest queued reader and the
    one with a lexical escape (`/u`); it needs the pattern-literal read and a
    measurement before the fold allowlist's `preg_match` learns a refusal;
  - the timezone cell: a call-site rule "timestamp present → setting read,
    not `nondet.time`" sharpens the whole date family, and `gmdate($f, $ts)`
    becomes pure;
  - the `precision` cell at operator sites (D4);
  - `strtoupper`'s 8.1 locale read: the catalog has no version axis
    (ADR-0021), and the row follows `PINNED_PHP`.
- ADR-0008 gains an amended-by pointer (its pseudo-constant paragraph now
  describes an opt-in that would *drop a setting-read label*, and remains
  unbuilt); ADR-0021 gains one for Decision 2; ADR-0046 and ADR-0096 need
  none, their read-shaped families carry the child by prefix.

## 8. Considered and rejected

- **"Pure modulo ambient state", as a built-in tolerance or as the meaning
  of `Pure`.** Rejected by O1, and on the merits: ADR-0084 refused to promote
  telemetry to a built-in tolerance because "unobservable to whom" is the
  project's call, and a locale read is observable to the program the moment
  anything calls `setlocale`. The project-level valve exists already.
- **A flat `global.read.locale`.** Loses the one prefix the family is defined
  by (born-held, process-written), which a policy or a later tolerance would
  have to enumerate by hand; ADR-0083 put the masking boundary in the
  hierarchy for the same reason. D1 keeps the owner's say.
- **Folding `%f` under the runner's C locale.** Would make a fold a claim about
  a setting the project never declared; the ADR-0008 opt-in is the honest
  road and is unbuilt.
- **A standalone id over every literal `%f`.** ADR-0017: a format preference
  with no envelope behind it is a linter's business.
- **The stat cache as an ambient setting.** The world writes the file and any
  plain-stream operation evicts the entry; it is `io`, and remembering it is
  ADR-0102's measured call.
- **The value side as a later slice of this ADR.** §5.5.
