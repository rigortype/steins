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
envelope findings only, with no bulk transform; D4 the `precision` cell is in the roster; its
label `global.read.setting.precision` is registered with its first coloured builtin row,
which is S3's `%s` of a value that is a float (landed, §3.8), and float-to-string operator sites
wait for ADR-0008's opt-in (recorded in `not-implemented.md`); a builtin reader whose read is
value-conditional follows the three-way rule of §3.2, never an argument-blind row (this wording
of D4 confirmed by the owner, 2026-10-07; the other builtin readers of `precision` and
`serialize_precision` — `strval`, `implode`, `print_r`, `var_export`, `json_encode`, … — are
coloured by slice S6); D5 ADR-0102 follows slices S1–S2 and is independent of S4–S6;
D6 `setlocale($c, '0')` narrows to the read in S4 with the other call-site narrowings (landed, §3.9);
D-S5a the `i` flag (or `(?i…)`) on any pattern that contains a letter reads the locale tables (adopted
by the owner, 2026-10-07; landed in S5, §3.10).

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

Four registry entries land with slice 1, and a fifth, `global.read.setting.precision`,
with slice S3 (§3.8); S6-core registers seven more with the first rows of `timezone`, `encoding` and `ini`
and the precision write, the ini functions with a literal option name (§3.11), and `env` waits for `getenv`.
The cell roster below names the later children (`timezone`, `env`,
`encoding`, `ini`); each is registered in the slice that colours its first row, never ahead of one, which
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
| **locale** (`LC_*`) | printf family under `f`/`g`/`G`; `localeconv`; `strcoll`; `nl_langinfo`; `ctype_*`; `strftime`/`gmstrftime`; `sort` family under `SORT_LOCALE_STRING`; `preg_*` without `/u`; `basename`, `pathinfo`, `strnatcmp`, `strnatcasecmp`, `substr_compare`, `parse_url`, `escapeshellarg`, `strip_tags` by php-src; `setlocale(LC_x, '0')` | `setlocale` with a locale name | setting; **slice 1**, and the readers beyond printf in S4 (§3.9) |
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
| `sprintf`, `vsprintf` | `{}` (via the allowlist; `vsprintf` uncatalogued) | `{global.read.setting.locale}`, and with S3 `global.read.setting.precision` beside it (§3.8) |
| `printf`, `vprintf` | `{io.output.buffer}` | `{io.output.buffer, global.read.setting.locale}`, and with S3 `global.read.setting.precision` beside them |
| `fprintf`, `vfprintf` | none | none; a stream writer's row is issue #989 |
| `setlocale` | `{global.write}` | `{global.write.setting.locale, global.read}` |
| `localeconv`, `nl_langinfo` | none | `{global.read.setting.locale}` |
| `strcoll` | none | `{global.read.setting.locale}` (a call-site certified `string` pair, ADR-0021 §3) |

A row's setting read is **proven at a call only where the read is unconditional for the
call as written**, by name, arity, a literal flag or a literal format. A read conditional on a
value's runtime type or on a format the site cannot see is `value-dependent-read` until the
call site rules it in or out (§3.2). `mb_strlen($s)` with one argument reads the encoding
unconditionally and `date($f, $ts)` reads the timezone unconditionally; neither is a gap, and
the printf family is the first row whose reads are conditional.

`setlocale(LC_x, '0')` is a query. S4 narrows it to the read alone at a literal `'0'` (D6,
§3.9); the argument-blind row is the write. A locale of
`''` or `null` also takes the name from the environment block
(`putenv("LC_ALL=fr_FR.ISO8859-1"); setlocale(LC_ALL, "")` answers `fr_FR`,
witnessed in review), which is a read; with no environment cell registered yet
the row carries the coarse `global.read` beside the write, and that read narrows
to the env cell's label when it lands (S6). A call whose only locale is a
written non-empty string literal other than `'0'` reads no environment, and S1
narrows it at the call site to the write alone (`narrowed_setlocale_labels`,
exactly two positional arguments: a third is a fallback locale that may be
`''`); `setlocale(LC_ALL, 'C')` therefore stays admitted by a `global.write`
envelope. An array of locales is left on the row.

## 3. Decision: the printf family is decided lexically

### 3.1 The rule

For a **literal** format the row's locale read is kept or dropped by the
format alone: it is kept iff some conversion spec ends in `f`, `g` or `G`.
`F`, `e`, `E`, `h`, `H`, every integer and character conversion, `s`, and
`%%` never read the locale. A format the parser cannot read as the engine does
(a `*` width or precision, an unknown conversion, a bare `'` pad, a position
of `0` or past the bound — the `None` cases of ADR-0021's 2026-10-03 note)
and a **non-literal** format are `value-dependent-read` (§3.2): the row is the
upper bound, and at a call the locale read is proven by a literal format naming
`f`, `g` or `G`, absent at a literal naming none, and a coverage gap at the
other two. The verdict is over the whole format, never a prefix, for the
reason that note gives: a malformed format may have rendered an earlier spec
before it fails.

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
format's reach verdict is read: the engine half of #860 (S7-engine), landed
with slice S3 (§3.8). A literal `'%d-%s'` drops the locale read and is pure
again, and `'%.2f'` keeps it. Dropping the *locale* label is not the claim that
the call reads no setting: a `%s` of a float renders it through `precision`, and
no other conversion does (witnessed with variable arguments, since 8.4 folds a
literal `sprintf('%s', 1.5)` at compile time). Under D4 that read is the
`precision` cell's, and the rows carry `global.read.setting.precision` beside the
locale read, registered with them.

**The criterion.** A row's label is *proven* at a call only where the read happens
on every run of that call as written. A literal `%f` is that: the locale is read
whatever the value is (an int is converted, an object becomes a number with a
warning). A `%s` reads `precision` only if its value is a float at run time, and a
printf call with a format the site cannot see reads the locale only if the format
turns out to hold an `f`, `g` or `G`: neither is true on every path (`'%d'` reads
nothing), so the argument-blind row is an upper bound and not a claim. `fopen($x)`
is the counter-example that shows the difference: it opens something on every
call, so `io` is true on every path and the argument decides only the child, which
is why the parent is an honest proven label (ADR-0083); `date($f)` with no
timestamp reads the clock on every such call for the same reason. For
`sprintf($fmt, …)` there is no label true on every path. ADR-0021 §3 (the reach
rule: `Coerced`, `Object` and `Nested` are ruled out by evidence or the call is
`…?`) and Decision 4 (`array_keys($a, $v)` is a gap, not a coloured upper bound)
already answer an unknown trigger with a gap and never with a proven label, because
a proven label on a declared body is a default-floor finding and ADR-0002 admits
only what is proven on a live path. ADR-0100 §2 is where the Maybe goes:
`effect.maybe-envelope-exceeded` at `strict`, naming the gap. A read conditional on a
value is therefore the gap kind `value-dependent-read`, recorded on the site beside any
`user-code-reach` the same value carries, until the call site rules it in or out. Owner
ruling O1 forbids silencing a *known* read and is untouched (a literal `%f` under a
pure envelope reports at the default floor with the `%F` fix-it); ADR-0002 and
ADR-0100 forbid *manufacturing* one where the trigger is unknown.

The first calibration read the row as the sound side wherever the site could not
decide, and put `precision` on 2,395 public summaries and 723 findings on one private
project's pure envelopes, all from `%s` of values nothing showed to be floats; this
paragraph replaces that reading.

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

Only a **proven** read is a default-floor finding. A body whose printf call has a
`value-dependent-read` gap is `…?` in the effect lane, and under a declared envelope it
reports at `strict` as `effect.maybe-envelope-exceeded` naming the gap, as every other
effect whose trigger the site cannot see does.

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

**Slice S2 (2026-10-03) — PENDING ratification.** The fix-it landed as S2 of the ambient-settings
run (#1000). What it decides beyond the text above:

- **Where it rides.** On the envelope finding the proven read raises at the printf call in the
  enveloped body (`effect.envelope-exceeded`), and on `effect.liskov-widened` for the locale label
  when every origin of the read in the method body is such a call. A finding about a callee's printf
  (`via` another function) carries none: its origin is a different declaration, and the edit would
  change a function nobody declared an envelope for. D3 is unchanged: no bulk transform and no
  standalone id.
- **Byte-exactness.** The scan starts at the callee's name, reads `(`, trivia and one `'` or `"`
  literal that must be followed by `,` or `)`, decodes it as PHP does (`\\` and `\'`; `\n \r \t \v \e
  \f \\ \$ \"`, octal, `\x`, `\u{}`) and compares the decode to the literal the tree holds. Any
  difference, a heredoc, a concatenation or a named argument gets no fix. Each conversion letter is
  edited at the source piece that spelled it, so `"%\x66"` becomes `"%F"` (the whole escape is the
  piece) and the escapes around it keep their bytes.
- **The title is the message.** A `Fix` has a title and edits and no other text, so the registered
  title says what changes: `use the locale-independent conversion (F, h, H): under a locale whose
  decimal point is not '.' the output changes, the decimal point becomes '.' always` (a locale
  whose decimal point already is `.` does not change). It is interned in the summaries table; the
  stored types are unchanged, so no schema bump, and a stored generation naming an older spelling
  of the title misses and walks, as any unregistered title does.
- **The PHP floor.** `h` and `H` are PHP 8.0's (on 7.4 `%h` prints nothing), so only a known floor
  of 8.0 or later (the declared target's floor, or the runtime's minor when none is declared)
  offers them: a call with a `g` or `G` conversion gets no fix where the floor is below 8.0 or
  unknown (nothing declared and no runtime answering, as under `--no-php`), since a partial edit of
  the `f` conversions alone would leave the read. `F` is offered on any floor. The floor is
  `Fixpoints::php_floor`, read by the effects pass only.
- **Liskov origins are structural.** The method's own printf sites are the body's resolved sites;
  a callee, a closure or a method the body reaches is an edge, and an edge whose proven effects
  hold the read means an origin the edits cannot reach, so no fix. A finding's provenance (a name
  and a line) is not used, since a callee's `sprintf` on the same line shares it.
- **Overlapping fixes.** `check --fix` leaves an edit wholly inside a different, larger edit of the
  same file to the larger one (a printf in the argument of a deleted `dumpType` statement), and
  still refuses partial overlaps. An octal escape above `\377` is refused: PHP truncates it with a
  warning the rewrite would lose.
- **Witnessed** on PHP 8.5.11: the 1,488 pairs of §3.1 and the literals the fix writes (flags,
  width, `n$`, `'c` padding, `l`, an escape-spelled letter) are byte-identical under `C`, and
  `%.2f` is `2,50` where `%.2F` is `2.50` under `de_DE`.

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

### 3.8 Slice S3: the call site reads the literal format (2026-10-03) — PENDING ratification

Landed as S7-engine of Run 2 (#915, #860) and S3 of this run (#1000), and merged with S1,
since S1 alone over-reports the locale read at every printf call. What it reads and decides:

- **The reach of a literal format.** `reaches_user_code` reads the call's literal format
  (`ConstArgs::first`) through `read_format` and maps its per-value reaches back onto call
  positions (`PrintfFamily::reach_at`): a value no `%s` names is `Inert`, a vector is `Nested`
  only when some conversion is `%s`, and a format the parser cannot read, or that is not a
  string literal, leaves the row. The rows of the S7 witness table that this decides
  (`sprintf('%d', $o)`, `'%s %d'` with a literal on the `%s`, `'%2$d %1$s'`, `'%.2f'`,
  `printf("%05d\n", $o)`, `'100%% %d'`, `"%'*10d|%-5s|…"`, `'%d %d'` with one argument,
  `vsprintf('%d-%d', $a)`) are exhaustive in both lanes, and the ones that must stay
  (`'%s'`, `'%d %s'`, `'%2$s %1$d'`, `'%1$d|%1$s'`, a non-literal format, `'%q'`, the loose
  `in_array`, `in_array` with a flag that is not a literal, `vsprintf('%s', $a)`, `fprintf`)
  read as they did.
- **The strict flag.** `ConstArgs::bools` carries a literal `true` or `false` at position 2 or 3;
  `strict_flag_position` names `in_array` and `array_search` (position 2). A literal `true`
  compares by identity, which runs no `__toString` at any depth, so needle and haystack are
  `Inert`; no flag, `false` and a flag that is not a literal keep the loose comparison.
- **The locale read** is proven at a literal format with an `f`, `g` or `G`, absent at a literal
  with none, and `value-dependent-read` at a format that is not a literal or that the parser cannot
  read (§3.1, §3.2), for `sprintf`, `printf`, `vsprintf` and `vprintf` alike, and for a printf name
  handed over as a callback (the invoker chooses the format).
- **The `precision` read** (D4) is three-way at each `%s` of a literal format, over a `FloatClass`
  (`Frame::float_class`): a value **shown a float** (every value its fact admits is one) proves
  `global.read.setting.precision`; values all **shown no float** drop it; any other value is the
  `value-dependent-read` gap (`GapKind::ValueDependentRead`, appended last, effect lane only, strict
  floor by ADR-0100 §2), recorded beside any `user-code-reach` the same value carries. A non-literal
  or unreadable format and a vector's elements are the gap too. `printf` and `vprintf` keep
  `io.output.buffer`, which is unconditional. A position the call does not supply renders nothing
  (`ArgumentCountError`). The rows carry the label as the upper bound, and the call site decides.
- **The evidence** is recorded per position for `sprintf` and `printf` (`ConstArgs::float_evidence`,
  a `FloatEvidence`: trace payload, no schema bump) and read by `Frame::float_class`:
  - *forms*: a float literal, an integer literal wider than `int`, a cast to `float` and a negation of
    one are `Yes`; a string, integer, boolean or `null` literal, a magic constant, an interpolated
    string, a concatenation, a comparison or logical connective, `!`, `isset`, `empty`, a cast to `int`,
    `bool`, `string` or `array`, an array literal and a negated integer literal are `No`; arithmetic is
    neither (integers overflow into a float);
  - *conditionals*: a ternary, `?:` and `??` are as their branches agree, each branch read as a
    top-level value;
  - *variables*: a by-value parameter is as its declared type says (`float` alone is `Yes` while the
    frame never writes it, a type with no `float` or `mixed` is `No`) and a local as the writes the scan
    carried say (a plain no-float form is `No`; a call, a constant or a ternary written to it is carried
    and judged as it would be alone), each only while no named call may rebind it, and withheld when a
    write may leave a float the scan cannot name (arithmetic, `++` and `--`, a loop or `catch` binding,
    a destructuring target, a by-reference argument);
  - *properties*: a typed `$this->p` and `self::$p`, `Foo::$p`, `parent::$p` are as their declared type
    says, on the rule of an operand shape's property (an ancestor's private property, a hooked one and
    an untyped one answer nothing);
  - *calls*: a plain, static or `$this` call, and a method on a declared receiver, is as its declared
    return says (S8's classifier: a builtin's mined row, a project function or method's native hint,
    on the same gates, the version gate included); the `Throwable` accessors are read for their
    declared type here even though they stay out of the object-free reading, since the declared return
    holds whatever the property did;
  - *constants*: a global constant by ADR-0094's resolution (the mined table, the platform classes, a
    project declaration) and a class constant by its literal initializer, or by its declared type where
    it has one (`const float X = 123456789` holds `float(123456789)`, and any other typed constant
    that is not plainly no-float is not read);
  - *nullable defaults*: a parameter declared `float $f = null` is implicitly nullable, so it is read as
    `?float` is, never as a float.
  A fully literal `sprintf('%s', 1.5)` is folded at compile time on PHP 8.4 and later, against the
  `precision` the ini holds when compiling; the proven label still stands, since the ini is read then.
  Float-to-string operator sites (`(string) $f`, `.`, `echo`) and the other float renderers (`strval`,
  `implode`, `print_r`) still carry no `precision` read; that waits for ADR-0008's opt-in and the rows of
  S4, and a builtin reader whose read is value-conditional follows this three-way rule when its row
  lands.
- The `numfmt_get_attribute` row stated `int|false`, which the evidence would have read as no float;
  the function returns a float for the float attributes (PHP 7.4.33, 8.4.25 and 8.5), so the miner
  corrects the row at its source (`SOURCE_CORRECTIONS` in `xtask/src/mine_function_map.rs`, which
  refuses to run once the pinned map says anything else) and the table reads `int|float|false`.
  A sweep of 2,192 functions and 3,584 engine methods found no other.
- No persisted format changes: `ConstArgs` is trace payload, and its two new fields are appended
  after `ints`.

Measured on the ten public packages (`check --profile strict --no-php --vendor-diagnostics
--no-cache`, `effect-diff`, the five transform dry-runs), head against `origin/master` (what
lands), against the first S3 head (before the calibration) and against S1's head:

- `check` against master moves only in `throw.maybe-undeclared`: 79 findings fewer (composer 30,
  phpunit 37, console 4, process 3, monolog 2, guzzle 2, Carbon 1) and 8 reworded, each at a site
  whose `user-code-reach` is gone (61 at an `in_array` or `array_search` with a literal `true`, 11 at
  a printf call whose literal format reaches no value, 15 at a call of a function that lost the gap
  inside); every other id and the default profile's findings are identical (composer's
  vendor-suppressed count falls from 325 to 323, the two vendored `Filesystem.php` sites). The
  calibration moves no strict finding against the first S3 head either: `value-dependent-read` is an
  effect-lane gap and no public package declares an envelope over a printf call, so
  `effect.maybe-envelope-exceeded` stays 0 and the possibly-grade rows of
  `xtask/fp-gate/possibly_expected.toml` are unchanged by it.
- `transform effects-envelope` against master: **+5** `@phpstan-all-methods-pure` class tags, none
  removed, no `@phpstan-impure` tag written; S1's churn (95 `@phpstan-impure
  global.read.setting.locale` tags written or extended, 35 class-wide pure tags lost) is retracted in
  full. `throws-envelope`, `loop-to-array-map`, `phpdoc-honesty` and `phpdoc-to-native` are
  byte-identical to every base.
- `effect-diff` against master: `global.read.setting.locale` on **16** of 28,846 function summaries
  (S1 alone: 4,184; the first S3 head: 352), each a function that contains, or reaches through a
  callee, a literal `%f`, `%g` or `%G`; **no** `global.read.setting.precision` (the first S3 head: 2,395),
  since no `%s` on the public packages is shown a float; and the 12 `setlocale` refinements, which are
  S1's. The 336 summaries the first head kept a locale label on through a non-literal format and the
  2,395 precision labels became `value-dependent-read` gaps or, where the evidence grew, nothing.
- Per file (`annotate`, no cross-file edges), 801 of 29,444 summaries carry `value-dependent-read`,
  every one already `…?` on master, so no function loses exhaustiveness to the calibration (5,687
  exhaustive, 18 more than master) and no exhaustive function carries a setting read. The gap's call
  sites, by what the evidence lacked (a token scan, an upper bound, since the engine proves some of
  them): a method call result on a receiver the scan cannot name (186), a format that is not a
  literal (92), a local assigned from a call the write summary does not carry (77), a local bound by
  a `foreach`, `list`, `catch` or closure (54), a local assigned from something else (38), an untyped
  `$this->p` (32), an array element (32), a function call result whose row the target's version gate
  declines or that has none (25), a ternary or `??` with a branch that shows nothing (20), a property
  of another object (18), a class or global constant the project does not state (12), an untyped
  parameter (4).

Left, the evidence queue, each with its reason: a method call on a receiver the scan cannot name
(`$this->a->m()`, a `catch` variable, a result held in a variable), and a local assigned from one,
need the frame's flow; a `foreach` or `catch` binding needs the iterated type; an array element
and a property of another object need a shape and a class fact the syntax layer does not carry; a
vector's elements (`vsprintf`) could be read element by element for an array literal; the declared
return of a builtin is read only where the target's version gate admits it; `array_keys`' third
argument is the same strict flag and is not read; `fprintf` and `vfprintf` have no row (#989).

### 3.9 Slice S4: the locale readers beyond printf (2026-10-03) — PENDING ratification

Landed as S4 of the ambient-settings run (#1000): ADR-0021's second amendment §5 listed names it refused because
each "reads the locale or an ini setting", and §4 below made that list a queue. S4 audits the locale half of it
against php-src at `php-8.5.11` and against the engine, and enters each name that earns a row. What it decides:

- **The evidence bar.** A name is coloured where php-src shows its own path consulting the locale (a C-library
  character-class or case table, `strcoll`, `strftime`, `mbrlen`, or an engine flag `setlocale` derives) **and** the
  engine moved under a locale other than `C` on this machine; a name that did not move anywhere stays as it is, and
  the reason is recorded below. The witness is the oracle table `locale_readers_oracle.rs` (steins-catalog tests): 55
  probes, each under `C`, `de_DE.UTF-8`, a Latin-1 locale and, where installed, `ja_JP.eucJP`, asserted in both
  directions (a probe that moved is a name the catalog does not call silent; a name it proves has a probe that
  moved). Whether a byte is a letter or a space is the C library's table, so a probe is either asserted on every
  platform (a Latin-1 letter, a day name, a collation) or only where it was witnessed, macOS's libc
  (`MovesOnMacos`: glibc's UTF-8 locales classify bytes `0x80..=0xFF` as nothing, and the probe may stand still
  there). The CI test job generates `de_DE.UTF-8` (`locale-gen`) and a Latin-1 locale (`localedef -i de_DE -f
  ISO-8859-1`, since `locale-gen` accepts only the names of `/usr/share/i18n/SUPPORTED`) and proves PHP can select them.
- **The criterion, applied as §3.2 states it.** A row is proven at a call only where the call as written makes the
  read unconditional, and the criterion is the same for every reader here: a **literal** argument decides
  lexically, by the trigger php-src shows for that function (below); a **type** that rules the read out means no
  read; and anything else (a variable, an expression the scan cannot evaluate, a named or spread argument list,
  the builtin handed over as a callback) is the `value-dependent-read` gap and never a label. An omitted argument
  is the parameter's default. This holds for `ctype_*`, `parse_url`, `strip_tags`, `escapeshellarg`, `strnatcmp`
  and `strnatcasecmp` as much as for the sorts and `pathinfo`: each consults a C-library table only for some
  contents, so `ctype_alpha($s)` over a `string $s` that may be empty is the gap, while
  `#[\Steins\Pure] function f(bool $b) { return ctype_alpha($b); }` is not a finding, because php-src returns
  `false` for a `bool`, `float`, `null`, array, object or an `int` outside -128..=255 before any table
  (`ctype_fallback`). `basename` is the one reader whose trigger is the call itself: `php_basename` consults the
  locale-derived `CG(ascii_compatible_locale)` before it looks at a byte, so every call reads it, `''` included,
  and no argument can rule it out (the flag selects the algorithm, which walks `php_mblen` where it is false).
  The row is the upper bound and the call site decides it (`setting_read_gate`,
  `site/setting.rs`).
- **What decides each name** (php-src `php-8.5.11`; the trigger is the first thing the routine does with the
  argument).
  - `ctype_alnum`, `ctype_alpha`, `ctype_cntrl`, `ctype_graph`, `ctype_lower`, `ctype_print`, `ctype_punct`,
    `ctype_space`, `ctype_upper` (`ext/ctype/ctype.c`): a non-empty string classifies its first byte, an `int`
    in -128..=255 is classified as a character, every other int (`allow_digits`, `allow_minus`) and every other
    type returns before a table. A literal string, int (a negative one included), bool, null, float, array
    literal or `new` expression decides; a by-value parameter whose declared type has no `string`, `int`,
    `mixed` or `callable` member, that the frame never writes and no named call may take by reference, is shown
    to hold neither (`ConstArgs::not_text`); a `string` or `int` parameter, `mixed` or an untyped value is the
    gap.
  - `strnatcmp`, `strnatcasecmp` (`strnatcmp_ex`): both operands non-empty; a literal empty operand settles the
    call whatever the other is.
  - `escapeshellarg` (`php_escape_shell_arg`): `php_mblen` over each byte of a non-empty string; a NUL byte
    throws first, so it is the gap.
  - `strip_tags` (`php_strip_tags_ex`): `isspace(p[1])` at the first `<`, reached in the start state; a string
    without `<` reads nothing, with or without an allowed-tags argument.
  - `parse_url` (`php_url_parse_ex2`): `isalpha` over the scheme when the first `:` is not at index 0, and
    `iscntrl` over every component it produces (`php_replace_controlchars`). A literal with a colon past index 0 reads; one with no colon, no leading `//` and a byte
    other than `?` and `#` produces a non-empty path, query or fragment and reads; the empty string reads nothing;
    a leading colon, a leading `//` and a string of only `?` and `#` may fail before any component, so they are
    the gap.
  - `strftime`, `gmstrftime` (`php_strftime`, then the C `strftime`): the locale is read by the conversions that
    name it, `a A b B c h p r x X` (and the same behind an `E` or `O` modifier), which POSIX names locale-dependent
    and the engine moved on (`%A` in 241 of 288 macOS locales, `%x` in 285, `%c` in 278, `%p` in 227, `%r` in 235,
    `%X` in 36); the numeric conversions `C d D e F g G H I j k l m M n R s S t T u U V w W y Y z Z %` moved in
    none, so a format of only those reads nothing. A flag (`_ - 0 ^ #`), a width and an `E` or `O` modifier may
    precede a conversion and change nothing about which ones read; a conversion this table does not know (`%P` is
    glibc's, `%v` and `%+` BSD's), a modifier on a numeric conversion and a dangling `%` are the gap. The empty
    format returns `false` before the C call. The row keeps the time family's argument-blind `nondet.time`
    whatever the format, and a non-literal format is the gap for the locale half.
  - `sort`, `rsort`, `asort`, `arsort`, `ksort`, `krsort`: `$flags` (position 1) selects the comparison. The base
    type `SORT_LOCALE_STRING` reads (`strcoll`) and so does `SORT_NATURAL` (`strnatcmp_ex`'s `isspace`, `isdigit`,
    `toupper`), with or without `SORT_FLAG_CASE`; the key sorts also read under `SORT_STRING | SORT_FLAG_CASE`
    (`php_array_key_compare_string_case_unstable_i` folds case through `zend_binary_strcasecmp_l`, a C `tolower`;
    `krsort` of `"\xC4"` and `"\xE4"` moved under Latin-1). A data sort under that flag pair reads on 8.1 and not on
    8.2 or later (`string_case_compare_function` became ASCII-only), so on a PHP floor below 8.2 it is the
    `value-dependent-read` gap and no label. It is the gap on every floor, because the own rows a file contributes
    are persisted per file and are read before the run's PHP floor (`Fixpoints::php_floor`) is known; a
    floor-keyed verdict would put the floor into the persisted fact. On 8.2 or later the gap is the conservative
    side by that one case. The slice list named `SORT_LOCALE_STRING` alone; the witness moved `SORT_NATURAL` and the key sorts too.
  - `substr_compare` reads under a true `$case_insensitive` (`zend_binary_strncasecmp_l`); `pathinfo` for the
    basename, extension and filename parts and not for `PATHINFO_DIRNAME` alone.
  - A bare constant in a flag is read as PHP resolves it (`global_const_fact`): `namespace App; const
    PATHINFO_DIRNAME = 2; pathinfo($p, PATHINFO_DIRNAME)` is the namespaced twin's `2` and reads, `\PATHINFO_DIRNAME`
    stays the global `1`, and a twin the scan cannot read is the gap. `ConstInt` gains `Global` for the fully
    qualified spelling and `ConstArgs::ints` position 0 for a `ctype_*` call. The same shadow reaches the
    `json_encode` flags of ADR-0099 §3.3, which evaluate a bare constant without the project and are left for their
    own slice.
- **Certified with no read**: `number_format` joins `CERTIFIED_AT_CALL_SITE` (§4). `_php_math_number_format_ex`
  renders with `%.*F`, whose decimal point `xbuf_format_converter` fixes at `.` (only `%f` takes
  `LCONV_DECIMAL_POINT`), splits at either `.` or `,` and takes the separators from its arguments; its one C
  `isdigit` tests the first character of the rendering, a set C fixes in every locale. The oracle row covers
  twelve calls (values, precisions, non-finite values, a multibyte separator), stable under every witness locale, and
  8.1 agrees. Its `string` separators are the reach rule's; the fold allowlist still refuses it (a certification of
  purity is not a permission to execute).
- **Left as they are, with the reason.** `ctype_digit` and `ctype_xdigit`: C fixes their sets in every locale
  (C11 7.4.1.5 and 7.4.1.12) and a 256-byte table under every witness locale did not move, so they read no setting
  that changes an answer; no row, and no certification either (a row is written only where the read is real, and
  certifying them is the same claim from the other side, which no witness here needs).
  `strcasecmp`, `strncasecmp`, `strtoupper`, `ucfirst`, `trim`, `str_contains`, `json_encode` did not
  move on 8.5 and are oracle rows that must stay stable. The data sorts under `SORT_STRING | SORT_FLAG_CASE` did not
  move on 8.5 either, and their oracle rows assert the catalog leaves them undecided, not that it calls them silent.
- **`setlocale($c, '0')` narrows to the read alone (D6).** `try_setlocale_str` compares the **whole string** with
  `"0"` and passes `NULL` to the C `setlocale`, which answers the current locale and changes nothing
  (`narrowed_setlocale_labels`: exactly two positional arguments, the literal `'0'`). The test is on the whole
  string and not on what C would read: `"0\0x"` is not the query. It asks C for a locale named `0`, answers `false`
  (witnessed: `setlocale(LC_ALL, '0')` after `de_DE.UTF-8` answers `de_DE.UTF-8`, `"0\0x"` answers `false`), and
  keeps the row. S1 had read it as the query; the S1 test list keeps it on the row either way, so nothing moves, and
  the doc comment is corrected here. An integer `0`, a third argument and an array keep the row too. The oracle
  asserts that the query changes nothing.
- **A discarded locale read is a dead statement** (§3.7, `statement.no-effect`): the read is covered by
  `global.read`, so `basename('/a/b');`, `strnatcmp('a', 'b');`, `pathinfo('/a/b.c');` and `ctype_alpha('a');` are
  reported once the name has a row, as `sprintf('%f', 1.5);` is. The names whose literal call raises a diagnostic
  the catalog row cannot record are refused: `strftime` and `gmstrftime` are deprecated outright
  (`REFUSED_ON_LITERALS`), and the `ctype_*` predicates deprecate anything but a string since 8.1
  (`ctype_alpha(65)`, `(null)`, `(1.5)`, `(true)`, `([])` and `ctype_upper(new stdClass)` raise `E_DEPRECATED`, which
  reaches `set_error_handler`), so they are admitted over a string literal alone (`STRING_LITERAL_ONLY`): a discarded
  `ctype_alpha('abc');` is reported and `ctype_alpha(65);` is not, although a setting read is discardable by prefix
  (ADR-0096). The public corpus has no such statement.
- **PHP versions.** The rows follow `PINNED_PHP` (8.5), as `strtoupper`'s 8.1 read already does (§7), except where
  the version changes the answer and a per-file fact cannot know the floor (the data sorts above, the gap on every
  floor). Witnessed on 8.1.32: `ucfirst` also reads there, and `strip_tags` and `parse_url`, whose 8.1 source passes a
  signed `char` to `isspace` and `isalpha`, did not move on these probes; every other row moved as on 8.5. The oracle
  is gated by the running `PHP_VERSION_ID`: a probe whose answer is 8.2's (`ucfirst` and the two data sorts) evaluates
  to a constant on an older engine, so it observes nothing there, and below 8.2 the positive direction of every other
  row (a coloured name moved under a witness locale) is not asserted. The soundness direction (nothing the catalog
  calls silent moved), every `Stable` row and the catalog's own verdicts are asserted on every version. The oracle
  does not skip itself on an older engine.

Measured on the ten public packages (`check --profile strict --no-php --vendor-diagnostics --no-cache`, default
profile, `effect-diff`, the five transform dry-runs), head against `origin/master` (the merge base):

- `check` under both profiles is byte-identical on every package: no finding moves, none is reworded.
- `effect-diff`: of 28,846 function summaries, `global.read.setting.locale` is added to **485**. The first
  calibration of this slice proved the per-byte readers on every call and added it to 718; of those 718, 485 keep
  the label, and the 233 that lose it are all the gap: 81 functions gain `value-dependent-read` and 152 already
  carried it from a printf site (none is left with neither). The direct callers that keep it are `basename` 28 (always proven), `pathinfo` 7, `escapeshellarg` 5, a
  `sort` or `ksort` 6 (the `SORT_NATURAL` calls of Composer's `FilesystemRepository` and Console's
  `Application`) and `parse_url` 2 (token matches, an upper bound); callees carry it to the rest. 89 functions gain
  `value-dependent-read` where a `parse_url($url)`, `strip_tags($html)`, `escapeshellarg($arg)`,
  `strnatcasecmp($a, $b)` or `pathinfo($p, $component)` sees a value, and 4 gain `user-code-reach` (the reach rule now
  applies to a row that was unrowed); each was `…?` already. The two `setlocale` summaries of Carbon's translator lose
  the write and the coarse read (`proven-removed-maybe`: the query, `setlocale(LC_TIME, '0')`). No other label moves,
  none is added to a non-locale name, and nothing leaves a function.
- **Coverage.** Two bodies become exhaustive, each with the read: `Factory::getLockFile` (`pathinfo($f,
  PATHINFO_EXTENSION)`) and `TestSuiteLoader::classNameFromFileName` (`basename`). 66 functions lose the
  `no-effect-row` gap, those two to exhaustiveness and the rest keeping another cause. The public corpus has little
  else to release: its `ctype_*` calls are `ctype_digit` and `ctype_xdigit` (left as they are) and one
  `ctype_alnum` inside a polyfilled-name body that stays `unknown-function`, and `number_format` sits in bodies with
  other gaps.
- `transform effects-envelope`: **+2** edits, each a `@phpstan-impure global.read.setting.locale` tag on one of
  the two bodies (both create a docblock), none removed. The planner refuses 40 more functions that now carry the read
  and stay `…?` (`effects-not-exhaustive`), and 9 class-wide `@phpstan-all-methods-pure` refusals disappear: a class
  with a method that reads the locale is no longer a candidate. `throws-envelope`, `loop-to-array-map`,
  `phpdoc-honesty` and `phpdoc-to-native` are byte-identical.

### 3.10 Slice S5: the preg family reads the locale's tables (2026-10-07) — PENDING ratification

Landed as S5 of the ambient-settings run (#1000), the biggest reader §7 deferred: `preg_*` without `/u`. It
decides lexically, the way S4's readers do, with the owner's adoption of D-S5a. What it decides:

- **What php-src does.** `pcre_get_compiled_regex_cache` is one compiler for every `preg_*`; once a script has
  called `setlocale` with `LC_CTYPE` or `LC_ALL` (the only writer of `BG(ctype_string)`) it compiles with the
  tables `pcre2_maketables()` builds from the process locale, and the cache key carries the locale string. The
  modifier `u` sets `PCRE2_UTF` **and** `PCRE2_UCP`, and UCP is what routes `\w`, `\s`, `\b`, the POSIX classes
  and caseless matching to Unicode properties; `(*UTF)` alone does not. The read is of the locale cell exactly as
  `setlocale` leaves it, and the exemption is UCP, not UTF.
- **The rule** (`pattern_reads_locale`, `steins-catalog`'s `preg/locale.rs` and `preg/locale/scan.rs`; one byte
  scan of its own, since `capture_groups` declines on `x`, `n` and every `(*…)` verb, which is where this verdict
  has to answer). Outside `u` and a leading `(*UCP)` a literal pattern reads iff it holds `\w \W \s \S \b \B`,
  `[[:<:]]` or `[[:>:]]` (PCRE2 rewrites them to `\b`), a POSIX class other than `[:digit:]` and `[:xdigit:]`, or a
  group or reference name with a byte of `0x80..=0xFF`. In **every** mode it reads iff it holds
  - a caseless flag (the modifier, or an `i` an inline `(?…)` group sets) together with a character it can match
    that is a letter: a literal letter or byte of `0x80..=0xFF`, a range whose span holds a letter (`[!-~]`), `.`, a
    negated class, `\w \D \S \N \p{..}`, a POSIX class with letters (`[:xdigit:]` has `a` to `f`), a numeric escape
    or a back reference in any spelling (`\1`, `\k<n>`, `\g{n}`, `(?P=n)`: **D-S5a**, extended to `u` below). It
    reads nothing only where every character it can match is provably no letter: digits, punctuation, or
    ranges confined to those;
  - the `x` flag together with a raw byte of `0x80..=0xFF` (or an escape spelling one) outside an `x` comment;
  - `[[:ascii:]]` or `[[:^ascii:]]`, which PCRE2 keeps on the table under UCP (`pcre2_compile.c:752`), and, under
    `(*UCP)` without `u`, a name with a byte of `0x80..=0xFF` (`read_name`).

  It does not read for `\d`, `\h`, `\v`, `\R`, literal bytes and ranges without a caseless flag, a group name or
  POSIX name (not a letter of the pattern), `\Q..\E` without a caseless flag, and a `preg_quote`d literal, which
  compiles nothing. An `x`-mode `#` outside a class (with the flag's scope kept per group: `(?x)`, `(?-x)`, `(?x:`)
  comments out the rest of its line, which is not scanned, so a `\Q` or `(?#` in the comment swallows nothing
  after the newline; it ends exactly where the newline convention says (LF unless a leading `(*CR)`, `(*CRLF)`,
  `(*ANYCRLF)` or `(*NUL)` says otherwise, a lone CR, VT, FF or NUL being comment text under LF, and `(*ANY)`, which
  differs by UTF mode, declines), since an early end would scan a `\Q` or `(?#` the comment still holds. **D-S5b** stands:
  the subject literal does not exempt a reading pattern, since an ASCII-only subject cannot meet a high byte only
  where the table is C's. `(*UTF)` alone is not UCP.
  The `i` rule reads every lettered pattern because on glibc's `tr_TR` the case map of ASCII `I` and `i` is not the
  C one; macOS did not move (S1, S2, S3, S23, P10, P14 stood still), so the oracle asserts those rows only as the
  rule's (`ReadsByRule`), and the narrower reading (a high byte only) needs a glibc witness first.
  **The rule extends to `u`.** Under UTF and UCP, caseless matching of an ASCII pattern character still compares
  through the locale's lowercase table (`pcre2_match.c:1052-1056`, and the fcc table the JIT builds from it), so on
  glibc's `tr_TR` `/^id$/iu` against `ID` moves; macOS's tables fold `i` as C's do and the witnesses D1 to D5 stand
  still. The owner's D-S5a reasoning (the `tr_TR` case map) covers `u` as much as the bytes below it, so a caseless
  flag reads wherever the pattern can match a letter, `u` or not, and a `u` pattern with no caseless flag reads
  nothing. The narrowing to a high byte only waits for a glibc witness in both modes.
- **Two tables the design's exemption missed, witnessed on macOS.** The design exempts `u` and `(*UCP)` wholesale.
  The oracle showed three tables they do not leave. PCRE2's `x` flag skips what `isspace` says of a byte below 256
  in every mode, so `/^a\xC2\xA0b$/xu` (a no-break space in the pattern) matches `ab` under `de_DE.UTF-8` and not
  under `C`, and `/(*UCP)^a\xA0b$/x` the same. `[[:ascii:]]` under `u` moves (`/^[[:ascii:]]$/u` against `ä`). And
  a name with a byte of `0x80..=0xFF` under `(*UCP)` without `u` moves (`/(*UCP)(?<\xE4>a)/` compiles under a locale
  and not under `C`), where under `u` it does not (R1 to R8). Each oracle row fails without its clause.
- **Which functions.** The eight that compile: `preg_match`, `preg_match_all`, `preg_replace`,
  `preg_replace_callback`, `preg_replace_callback_array`, `preg_filter`, `preg_split` and `preg_grep`, each
  `{global.read.setting.locale}` as the upper bound the pattern at position 0 decides (`PregPattern`, the tenth
  kind of `setting_read_gate`). `preg_quote` compiles nothing and keeps its empty row; `preg_last_error` and
  `preg_last_error_msg` have no row and gain none. Three names were `{}` through the fold allowlist
  (`preg_match`, `preg_match_all`, `preg_split`), three were out-parameter rows only and so `{}` at a call that
  passes no out-parameter (`preg_replace`, `preg_replace_callback`, `preg_replace_callback_array`), and
  `preg_filter` and `preg_grep` had no row (`no-effect-row`): all eight now carry the read. `preg_replace_callback` and `preg_replace_callback_array` are
  invokers, whose own row the effects pass read **before** it asked the call; `higher_order` now narrows an
  invoker's row by the same gate.
- **The pattern argument.** A string literal is `ConstArgs::first`. An **array literal** (a list for
  `preg_replace`, `preg_replace_callback` and `preg_filter`; the keys of the map for
  `preg_replace_callback_array`) is carried as `ConstArgs::patterns`, appended after `not_text`: every element must
  be a string literal, decoded, or the field is absent. An array reads if any pattern reads, an empty array
  compiles nothing, and an array with an element the scan cannot read as a literal (a variable, a concatenation,
  a spread, a nested array, a non-string key) is the gap whole: the patterns before a bad one are compiled and a
  later one is not certain to be, so no partial claim is made. A trace payload, no schema bump.
- **The gap.** A pattern that is not a literal (a variable, a concatenation, an interpolation, a class constant,
  a named or spread argument list), a pattern the reader declines (an unterminated class, an unbalanced group, an
  unknown escape or verb, an unknown modifier: PCRE2 would refuse it, or the reader does not know it) and a
  `preg_*` handed over as a callback are `value-dependent-read` and no label. The reader knows `r` (PHP 8.4) and
  `(?r)` as valid flags. A `preg_*` called with no pattern throws before it compiles and reads nothing.
- **The fold.** `fold_reads_ambient_setting` refuses a `preg_match`, `preg_match_all` or `preg_split` (the
  foldable preg names) whose literal pattern reads **or that the reader declines**: the runner has never called
  `setlocale`, so it answers under the C tables, a claim about the project's runtime the project never made, and a
  decline is not proof that PCRE2 refuses the pattern (the first cut folded a decline, and a valid `[[:<:]]` that
  reads the tables was one). An invalid pattern that no longer folds widens to the declared type
  (`preg_match('/[/', 'abc')` is `0|1|false`, not `false`); three folding tests pin that. `preg_quote` folds.
  ADR-0102's `remembered.rs` allowlist is untouched: `preg_*` stays off it, and a `none` verdict makes a site `{}`
  without making the name rememberable.
- **Witnessed.** `locale_readers_oracle.rs` gains 71 rows: the S5 witness table (`s5-preg.php`,
  `s5-preg-2.php`; PHP 8.5.11 and 8.1.32 agree on every row) as one probe per row, the five functions of P18, P19,
  P28, S21 and S22 beside `preg_match_all` and `preg_replace_callback`, and the rows the first table missed: the `x`
  flag over a byte under `u` and `(*UCP)`, `(?P=a)`, `\k<a>` and `\1` back references, an `x` comment holding `\Q`
  or `(?#`, `[[:<:]]` and `[[:>:]]`, `[[:ascii:]]` under `u`, a name under `(*UCP)` and under `u`, and `r`. Of the
  rows that must stay every one moves under `de_DE.UTF-8` or the Latin-1 locale on macOS and is asserted coloured;
  the exempt rows are asserted `Stable` and the verdict `Some(false)`; the lexical-rule rows that stood still (the
  caseless ASCII ones, with or without `u`, `[!-~]`, `.`) are `ReadsByRule`. Each assertion fails on a mutation
  that returns the other answer. A sweep of 123 patterns over every byte, every two-byte string of a small
  alphabet and every code point below 0x180, under five locales, found no pattern that moves and is called silent.

Measured on the ten public packages (`check --profile strict --no-php --vendor-diagnostics --no-cache`, default
profile, `effect-diff`, the five transform dry-runs), head against `origin/master` (the merge base; the machine's
load average was about 12, which no number below depends on). The numbers below were taken twice, on the first cut and on the one that
adds the caseless rule under `u`, the `x` comments, the named back reference, `[[:<:]]`, `[:ascii:]` under UCP and
the fold's refusal of a decline, and are identical: the public packages hold no pattern those rules move. The result:

- `check` under both profiles is byte-identical on every package: no finding moves, none is reworded; no public
  envelope covers a preg site, and `possibly_expected.toml` does not move.
- `effect-diff`: of 28,846 function summaries, `global.read.setting.locale` is added to **241** and
  `value-dependent-read` to **67**; nothing is removed and no other label moves. Of the 241, 56 hold the reading
  literal themselves (43 through `\w \W \s \S \b` or a POSIX class, 13 through `i` alone: `GelfMessageFormatter::format`
  and `BrowserConsoleHandler::handleCustomStyles`, `cleanClassName` and `pluralize`) and 185 inherit it through a
  callee (`BrowserConsoleHandler::generateScript`, `handleStyles`; 3 of them hold only literals that read nothing,
  `HeaderProcessor::parseHeaders` and `Terminal::initDimensions`). Of the 67 gaps, 63 sit on bodies already `…?`;
  **4** were exhaustive and are not any more, each with a pattern the site cannot read as a literal:
  `Standard::pSingleQuotedString` (the pattern is held in a local), `Standard::containsEndLabel` and
  `SourceMapper::isInHiddenDirectory` (concatenated), `CarbonPeriod::addMissingParts`. No function gains
  exhaustiveness (5,880 exhaustive on master, 5,876 here). The design's proxy counted 93 reading patterns among
  323 without `u`; a grep of the same packages finds 97 of 367 literal single patterns (74 through the classes
  and `x`, 23 through `i` alone), 21 of them with `u`, and the 28 patterns the reader declines are artifacts of the
  grep (a concatenated pattern), not literals.
- Folds: **none lost**. The one preg call with every argument literal in these packages is `preg_match('/^.[/u',
  'a')`, which still folds. The design's "1 fold lost" was a `preg_replace` with an interpolated subject, which is
  not on the fold allowlist and never folded.
- `transform effects-envelope`: **+3** edits, none removed: a `@phpstan-impure global.read.setting.locale` tag on
  Guzzle's `HeaderProcessor` (a docblock and its tag, two edits) and on PHP-Parser's `VoidCastEmulator` (one docblock).
  The planner refuses 118 more functions that now carry the read and stay `…?` (`effects-not-exhaustive`),
  rewords 58 of its `proven` lists to include it, and 22 class-wide `@phpstan-all-methods-pure` refusals disappear
  (21 `effects-not-exhaustive`, 1 `uses-trait`): a class with a method that reads the locale is no longer a
  candidate. `throws-envelope`, `loop-to-array-map`, `phpdoc-honesty` and `phpdoc-to-native` are byte-identical.
- Left, with the reason: a pattern held in a local (`$regex = '/…/'; preg_replace($regex, …)`) or a constant
  (`self::PATTERN`) is the gap although the literal is one assignment away, as S3's printf evidence was before its
  calibration; the four exhaustive bodies above are the cost, and reading a once-assigned local literal is the
  evidence queue. A subject literal does not exempt a reading pattern (D-S5b).

### 3.11 Slice S6-core: the setting cells, the gate that names its cell, and the ini names (2026-10-08) — PENDING ratification

Landed as the first sub-slice of S6 of the ambient-settings run (#1000), the shared base the later cell slices (env,
encoding, timezone, precision, ini residue, the `DateTime` constructors) build on. The owner adopted the design's
recommendations on 2026-10-08: **D-S6f** (the case folders follow `PINNED_PHP`, with the 8.1 readers recorded as a
floor divergence), **D-S6b** (the clock half of the time family follows the criterion too) and the sub-slice order
S6-core, env, encoding, timezone (functions), precision readers, ini residue, timezone (constructors), case folders.
Neither D-S6f nor D-S6b colours a row here. What S6-core does:

- **The roster as a type.** `SettingCell` (`steins-catalog`'s `setting.rs`) is the roster of §2.3: `Locale`,
  `Precision`, `Timezone`, `Env`, `Encoding`, `Ini`, each with `read_label()` and `write_label()`, the pair
  `global.read.setting.<cell>` and `global.write.setting.<cell>`. `precision` and `serialize_precision` are one cell
  (the design's choice: a call that reads one has no reason to be told apart from a call that reads the other, and
  `ini_set` of either is its write and its old-value read).
- **The registry (§2.2's rule applied).** A cell's label is registered with the first row that colours it, and S6-core
  colours no builtin row of a later cell, but it does colour rows: `ini_get`, `ini_set`, `ini_alter` and `ini_restore`
  with a literal option name (below) carry the cell's read, read and write, or write, and those are the first rows of the
  `timezone`, `encoding` and `ini` cells and the first write of `precision`. Seven labels are registered with them
  (`global.read.setting.timezone`, `.encoding` and `.ini`, and `global.write.setting.precision`, `.timezone`,
  `.encoding` and `.ini`), and every one is a child of a coarse label, so an envelope that admits the parent admits
  it. Registering them is not inert, though: a label that was unknown is known, so `effect.unknown-label` stops
  firing on an attribute that names one, the did-you-mean suggestion for a near miss can now offer them (and, for
  `global.read.setting.env`, the nearest registered neighbour, until S6c registers it), and an interop docblock tag
  (`@phpstan-impure global.write.setting.precision`) that was an unrecognised vocabulary now binds as an envelope. **`env` is not registered**: no ini entry feeds it, and its first row is
  `getenv` (S6c), which registers the pair; `SettingCell::Env` is in the enum and carries its labels, and a test pins
  that they stay out of the registry until a row names them.
- **The gate names its cell.** `LocaleReadGate` is `SettingReadGate { cell, kind }` (`setting_reads.rs`, from
  `locale_reads.rs`), `locale_read_gate` is `setting_read_gate`, and `steins-infer`'s `site/locale.rs` is
  `site/setting.rs`. `narrow_labels` and `unreadable_mode` drop the label the gate's cell spells (`gate.cell().read_label()`)
  and no other. Every gate of S4 and S5 names `Locale`, so no locale verdict moves; the later slices add kinds that name
  their own cell. The oracle file keeps its name (`locale_readers_oracle.rs`) until a row of another cell joins it.
- **The ini names** (`ini_cell(name)`). The table maps an option name to the cell that owns it, by php-src
  (`6bc7c26cf6`, the 8.5 line): `precision` (`PHP_INI_ENTRY("precision")`, `EG(precision)`) and `serialize_precision`
  (`PG(serialize_precision)`) to precision; `date.timezone` (`guess_timezone`, once no `date_default_timezone_set`
  has run) to timezone; `iconv.{internal,input,output}_encoding` and
  `mbstring.{language,detect_order,http_input,http_output,substitute_character,strict_detection}` (each the default of
  an `iconv_*` or `mb_*` reader) to encoding; `bcmath.scale`, `include_path` and `error_reporting` to the ini
  residue. **Five names that feed the encoding readers stay unmapped**: `default_charset`, `internal_encoding`,
  `input_encoding`, `output_encoding` and `mbstring.internal_encoding`. Rewriting any of them runs
  `_php_mb_ini_mbstring_internal_encoding_set` (`mbstring.c`), which also resets the mb-regex encoding
  (`php_mb_regex_set_default_mbctype`), the state `mb_regex_encoding()` writes. That state belongs to no cell yet, so
  `ini_set('default_charset', …)` narrowed to `global.write.setting.encoding` would be admitted by an envelope that
  refuses `mb_regex_encoding()`, which writes the same state (witnessed on 8.5). **S6d** puts the mb-regex encoding
  into the encoding cell, colours `mb_regex_encoding`, `mb_ereg*` and `mb_split`, and maps the five names then; until
  then they keep the coarse `global.read` and `global.write`. `mbstring.encoding_translation`,
  `mbstring.http_output_conv_mimetypes` and `mbstring.regex_*` feed an output handler or `mb_ereg*`, whose rows are
  S6d's, and every other entry has no reader a row names yet: they map to no cell. The residue is one name at a time (§2.3), so a name joins it with the row that reads it. The lookup is
  **exact**: the engine finds an entry by a case-sensitive hash lookup (`zend_ini_get_value`, `zend_alter_ini_entry_ex`),
  and `ini_get('PRECISION')` is `false` (witnessed on 8.5), so a miscased name is no cell's.
- **The rule** (`narrowed_ini_labels`, called from `function_effects` beside `narrowed_setlocale`, the call-site
  narrowing S1 and S4 use for a row that is replaced rather than dropped). `ini_get` with one argument is the owning cell's
  read; `ini_set` and `ini_alter` (php-src's `@alias ini_set`) with two arguments are its read **and** write, because
  `zif_ini_set` calls `zend_ini_get_value` unconditionally and returns the old value (a body declared with the write
  alone and calling `return ini_set('precision', '3')` is therefore refused, as it should be); `ini_restore` with one
  argument returns nothing and is its write. The option is a written string literal ([`ConstArgs::first`]) that the cell
  owns. The criterion of §3.2 holds: each reads or rewrites its entry on every run of the call as written, so the label
  is proven and not an upper bound. The cell's labels are **not the whole of an `ini_set`'s setting effect**: the new
  value is converted to a string first (`zval_get_tmp_string`), and a float is rendered through `precision`
  (`ini_set('include_path', 1234.5678)` stores `1.23E+3` under `precision=3`, witnessed). The value argument takes the
  three-way rule of §3.2 over S3's float evidence, extended to `ini_set` and `ini_alter` at position 1: a value shown a
  float adds the proven `global.read.setting.precision`; a value shown no float adds nothing; any other is the
  `value-dependent-read` gap and no label. For the precision cell itself the old-value read is already proven, so the
  value needs no second verdict. The rule is on the narrowed path only: `ini_set($name, $float)` and an unmapped name
  keep the coarse `global.write`, as before. A call
  of any other count raises an `ArgumentCountError` before it touches an entry, and a named or spread list has no count,
  so both keep the coarse row; a name that is not a literal (a variable, a concatenation, an interpolation, a constant) and a
  name no cell owns keep it too, and so does an ini function handed over as a callback (`builtin_callback` asks
  `function_effects(builtin, None, None)`). The coarse rows are the argument-blind ones of before, `global.read` for
  `ini_get` and `global.write` for the others: §2.2's sketch gave a dynamic ini name `global.read.setting`, and moving
  the coarse rows there would reword findings that have nothing to do with a cell, so it is left to a decision of its own.
  `ini_alter` and `ini_restore` had **no row** (`no-effect-row`); they are now the coarse `global.write`, which is the row
  `ini_set` always had, and `ini_get_all` stays with the residue slice (S6e).
- **Witnessed.** `setting_cells.rs` (`steins-infer` tests) is the S6-core witness table, one test per row group over every
  name the table owns: `ini_get` carries the cell's read, `ini_set` and `ini_alter` its read and write, and
  `ini_restore` its write (including `\ini_get`, an upper-case `INI_GET` and a double-quoted name); the value of an
  `ini_set` is a float, no float or unknown (the three-way rule above); a variable, a concatenation, a constant, a
  call, an interpolation, a named argument and a spread keep the coarse row; unmapped or miscased names (the five
  encoding names among them), and a call with the wrong count, keep it; a callback keeps it; a declared envelope
  admits the cell it names and the parents and no other cell, in either direction, and an `ini_set` needs both halves;
  and the locale verdicts of S1 to S5 are unchanged. Against the merge base the
  table fails on 7 of its 9 tests (the cells are not there) and passes on the other two (the callback and the locale
  rows); the `keeps the coarse row` tests fail on the base only through `ini_alter` and `ini_restore`, which had no row.
  The catalog's own tests pin the table (every name once, every cell registered, `Env` not), the arity gate and the
  label pair.

Measured on the ten public packages (`check --profile strict --no-php --vendor-diagnostics --no-cache` and the default
profile, `effect-diff`, the five transform dry-runs; a release binary built from the merge base `4af4103a` against one
from the head; the machine's load average was about 20, which no number below depends on; the base and head outputs
are non-empty and the binaries differ). The corpus holds 113 `ini_get` and `ini_set` call sites, 4 of them with a
literal name a cell owns (`ini_set('precision', …)` twice, `ini_get('include_path')` twice, the second pair beside an
`ini_set` of the same entry):

- `check` under both profiles is **byte-identical** on every package: no public envelope covers a mapped call
  and no public docblock names one of the seven labels, so no finding moves here; code that does name them
  sees the changes the registry decision above lists (an `ini_set` under a write-only envelope now reports its read).
- `effect-diff`: of 28,846 function summaries, **25 events on 11 functions**, none in the eight packages that hold no
  such call. `global.read.setting.precision` and `global.write.setting.precision` replace `global.write` on
  `ModifyTest::testAddRealMicrosecondWithLowFloatPrecision` (Carbon; `ini_set('precision', '9')` and the restoring
  `ini_set('precision', $old)`), and `global.read.setting.ini` and `global.write.setting.ini` replace `global.read` and
  `global.write` on `PhpHandler::handleIncludePaths` (PHPUnit; the literal `include_path` calls, whose value is a
  concatenation and so shown no float). The other nine are callers that inherit them (`Application::run`,
  `PhpHandler::handle` and seven `PhpHandlerTest` methods), which keep their coarse labels from a call with a name that
  is not a literal and gain the two cell labels beside them: 22 `proven-added` events (10 ini reads, 10 ini writes,
  1 precision read, 1 precision write) and 3 `proven-removed-maybe` (`global.write` twice, `global.read` once), all on
  the two functions that held the literal calls and no other source of the coarse label. No `value-dependent-read`
  gap is added (no mapped `ini_set` in these packages stores an unseen value), no function gains or loses
  exhaustiveness and no other label moves.
- `transform effects-envelope`: byte-identical but for two refusals' `detail` text, which names the proven labels
  (`proven global.write` is `proven global.read.setting.precision, global.write.setting.precision`;
  `proven global.read, global.write` is `proven global.read.setting.ini, global.write.setting.ini`). No edit is added or removed. `throws-envelope`,
  `loop-to-array-map`, `phpdoc-honesty` and `phpdoc-to-native` are byte-identical on all ten packages.
- Wall time moves by nothing the load does not explain (a per-package pair of runs agrees to a second). The ledger counts
  do not move, so no reseed.
- Left: `ini_get_all`, `set_include_path`, `error_reporting($l)`, `bcscale` and `set_time_limit` (the residue slice),
  the cells' builtin readers (S6c to S6e) and the constructor rows are not coloured; a non-literal name keeps
  `global.read` and `global.write`, and moving those two to `global.read.setting` and `global.write.setting` (§2.2's sketch of a
  dynamic ini name) would reword every `ini_get($name)` finding and is a decision of its own. `ini_restore` of a name no
  cell owns is `global.write` where it was a `no-effect-row` gap, which the corpus never exercises.

### 3.12 Slice S6c: env (2026-10-08) — PENDING ratification

Landed as the third sub-slice of S6 of the ambient-settings run (#1000), after S6-core (§3.11) and before the encoding
cell. The design (the S6 section of #1000) sets the rule: the environment block is a cell of its own, `getenv` reads it
at every arity, `putenv` writes it, and `$_ENV` is not part of the cell. What S6c does:

- **Witnessed (php 8.5.11, NTS, the CLI; the rows of `setting_env_oracle.rs`).** `putenv("K=one")` then `getenv("K")`
  answers `'one'` (E1); a second `putenv` is seen by the next `getenv` (E1′, the read observes the write, so the read is of
  the block and not of a value cached at startup); `getenv()` with no argument lists the block and holds the entry
  (E3); `getenv("K", true)` answers the same entry (E4); `putenv("K")` without `=` removes it (`getenv` then `false`).
  `$_ENV["K"] = "nine"` leaves `getenv("K")` at `false` (E5): a superglobal write never reaches the block. And a
  `putenv` is not reflected in `$_ENV` (the `snapshot` row): `$_ENV` is a startup copy. **`$_ENV` therefore stays the
  superglobal and gets no row**, as the design says.
- **Rows (`effects.rs`).** `getenv` carries `global.read.setting.env` at every arity; `putenv` carries
  `global.write.setting.env`. Both are argument-blind (no gate, no narrowing), so the arity does not decide the label.
  The coarse rows they replace were `global.read` (`getenv`) and `global.write` (`putenv`, which shared the arm of
  `ini_set`, `date_default_timezone_set` and the rest). `ini_get`, `date_default_timezone_get` and the other coarse
  writers keep their rows. `apache_getenv` and `apache_setenv` have **no row**: they are SAPI-provided functions
  (`absence.rs`'s `apache_` prefix), which the catalog does not seed, so there is nothing to recolour and no row is
  invented. `$_ENV` has no row because it is not a function.
- **Registry.** `global.read.setting.env` and `global.write.setting.env` join the builtin table
  (`labels.rs`), and `SettingCell::Env` is registered like the other cells: every cell's pair is now registered, and the
  test that pinned `Env` out is replaced by one that pins all six. Registration is not inert (§3.11): `effect.unknown-label`
  stops firing on the two labels, and a docblock tag naming one now binds as an envelope. No ini name maps to `Env`, and
  the `ini_cell` table is untouched (S6-core's tests still pin that).
- **`setlocale` keeps its coarse read.** `setlocale('', …)` consults the environment block too (`LC_ALL` and the
  others), and the row still says `global.read` for that consultation. The S6-core comment said the environment read
  would narrow when the cell landed. It is **not narrowed here**: the locale verdicts of §3.9 and §3.11 pin
  `global.read` beside `global.write.setting.locale` (`locale_cell.rs`, `locale_readers.rs`, `setting_cells.rs`), and
  narrowing would move all of them for a read that an envelope admitting `global.read` still admits. It is a follow-up,
  reported in the slice's PR and not in this section's rule.
- **`remembered.rs` (ADR-0102), unchanged.** `getenv` is not on `ALLOWED`, and the allowlist stays by name. A pin test
  (`getenv_and_putenv_stay_off_the_allowlist`) records that: a setting read is never remembered in v1 (§7 of ADR-0102),
  and the env cell does not lift that posture; ADR-0102 may lift it later for the env cell, and that would be a change
  to that ADR's posture, not this slice's.
- **Discard.** `global.read.setting.env` is a child of `global.read`, and `no_effect`'s discardable set admits
  `global.read` by prefix. A probe of a statement-position `getenv('X');` reports `statement.no-effect` on the base and on
  the head, and the two `check` outputs are byte-identical.

**Tests.** `setting_env_oracle.rs` (new, `steins-catalog` integration tests, the S6c witness): the catalog's two rows
(`getenv` is the env read, `putenv` its write, neither gated), and the seven PHP rows above, asserted against the
output of `php` and skipped loudly without it (failing under `CI`). Its catalog half fails on the base; its PHP half is
a witness of the engine and passes on both, by design. `setting_cells.rs` (`steins-infer`):
`the_environment_block_is_what_getenv_reads_and_putenv_writes` (every arity, the variable name, `getenv(null)`, the
`putenv` forms, and a `$_ENV` read keeping no env label) fails on the base and passes here. `effects.rs`:
`the_env_cell_has_a_read_row_and_a_write_row`, and the locale-cell neighbour test without `getenv`/`putenv`.
`labels.rs` and `setting.rs` pin the registration of all six cells. `remembered.rs`: the allowlist pin.

**Measurement.** Base is the merge base `19b4e1d7` (a release binary built from a copy of that tree; `git archive`
omits `crates/` through `export-ignore`, so the copy was the worktree with this slice's tracked diff reversed), head is
this branch's release binary. Ten public packages, `check --profile strict --no-php --vendor-diagnostics --no-cache
--format json` (and the default profile, run too): **byte-identical** on every package under both profiles, and no
public envelope or docblock names the env labels, so no finding moves, appears or disappears. `effect-diff`: of 28,846
compared summaries (28,847 captured), **1,951 events on 858 functions** in six packages (composer 867 events, symfony/console
514, guzzle 260, phpunit 224, symfony/process 52, flysystem 34; none in the other four). Every event is a function that
calls `getenv` or `putenv`, which the design expects:

- `proven-added global.read.setting.env`: **750**; `proven-added global.write.setting.env`: **248**.
- `proven-removed-maybe global.read`: **726**; `proven-removed-maybe global.write`: **219**. The coarse label is no
  longer proven on these functions, because the call that carried it is now a narrower label.
- `proven-removed global.read`: **3**; `proven-removed global.write`: **5**. These are the functions whose only
  coarse label came from `getenv` or `putenv` (`GuzzleHttp\Handler\ProxyEnv::getNoProxy` and `::getenv`,
  `Symfony\Component\Console\CI\GithubActionReporter::isGithubActionEnvironment`, and five test set-up and tear-down
  methods that call `putenv`). Each of the eight has an env event in the same function.
- Pairing: no function loses a coarse label without gaining an env label. 27 functions gain an env label and keep a
  coarse label from a call the row does not recolour (`proven-added` only; not traced call by call).

No function gains or loses exhaustiveness, and no label other than `global.read`, `global.write` and the two env labels
moves. Every move is a narrowing to a child of the coarse label, so an envelope that admits the coarse label admits the
new one; an envelope that names the coarse label without its child was never a `putenv` or `getenv` envelope in the
corpus (none is). Wall time moves by nothing the load does not explain.

**Not measured.** The private corpus (not run, per the brief). The `apache_*` pair, which exists only in a SAPI that
defines it. A non-macOS libc: no row here depends on the C library's tables, because the environment block is the
process's own (unlike §3.9 and §3.10).

### 3.13 Slice S6d: encoding (2026-10-08) — PENDING ratification

Landed as the fourth sub-slice of S6 of the ambient-settings run (#1000), after S6c (§3.12). The design (the S6 section of #1000) sets the rule: a function that takes an `$encoding` reads the encoding cell where the argument is omitted or `null`, none where it is a literal name, and the `value-dependent-read` gap otherwise; `mb_internal_encoding()` and its kin read with no argument and write with one; the five ini names that also reset the mb-regex encoding move into the cell, with `mb_regex_encoding` and the `mb_ereg*` family. php-src (the `php-8.5.11` tag of the local checkout, which the checkout's master (8.6.0-dev) agrees with on every sink cited below except the one noted) and PHP 8.5.11 and 8.4.25 settled the places where that rule needs more than the argument, and the slice follows the engine in each.

- **The functions.** The generated parameter table (`param_facts_generated.rs`) has **70** rows with a parameter named `encoding` or `from_encoding`. **42** are gated readers: 11 `mb_*` of the *plain* class, 21 `mb_*` of the *substituting* class, `htmlspecialchars`, `htmlentities`, `html_entity_decode`, `get_html_translation_table`, and six `iconv_*` (`iconv_strlen`, `iconv_substr`, `iconv_strpos`, `iconv_strrpos`, `iconv_mime_decode`, `iconv_mime_decode_headers`). **4** are accessors (`mb_internal_encoding`, `mb_regex_encoding`, `mb_http_output`, `mb_detect_order`), and **24** are none of the cell's, each with a reason in `EXCLUDED` (the `gz*`, `deflate_init`, `inflate_init` and `zlib_encode` take a `ZLIB_ENCODING_*` constant, the `openssl_cms_*` a serialisation constant, `pg_set_client_encoding` and `pg_setclientencoding` a required connection charset, the four `tidy_*` the document's own encoding, `xml_parser_create*` and `xmlwriter_start_document` their own defaults, and `iconv`, `mb_convert_variables`, `mb_encoding_aliases`, `mb_preferred_mime_name` require every encoding they take). `iconv_set_encoding` is in that group too: it is a write row with no argument to omit. A test (`every_encoding_parameter_is_gated_an_accessor_or_excluded`) holds the partition, so a new mined function with such a parameter cannot arrive unexamined. Three accessors that the table does not list because their parameter has another name (`mb_language`, `mb_substitute_character`) and `mb_regex_set_options` and `iconv_get_encoding` complete the shapes, and six functions that compile under the mb-regex state carry the read with no gate.
- **Gate kinds (`setting_reads/encoding.rs`).** `SettingReadGate`'s kind `Encoding` holds four shapes: `Argument { position, class }` (the position is read off the generated table, not typed twice), `Accessor`, `RegexOptions`, `IconvType`. `GateArg` gains `Null` (a `null` literal; the locale readers' `null` stays `NotText`), and `SettingReadGate::writes` says whether a call may still write the cell, which drops the write label of an accessor given nothing to set. The deciding arguments reach the gate through `ConstArgs::literals`, a field appended after `patterns` (trace payload; no schema bump, as S4 and S5), recorded for a call whose spelling starts `mb_`, `iconv`, `html` or `get_html`, at any position up to the fifth.
- **Witnessed (`setting_encoding_oracle.rs`, 8.5.11 and 8.4.25).** An omitted or `null` encoding follows `ini_set('default_charset', …)` through every class (N1, N4, N8, N12; `mb_strlen("ä")` is 1 then 2). A literal name does not for the plain class (N2, N3b) and HTML (N9) and iconv (N13). Four more observations are the reason for the classes:
  - **The substitution character is the cell's, and a literal name does not stop it.** After `mb_substitute_character(0x41)`, `mb_strtoupper("a\xFFb", 'UTF-8')`, `mb_substr("a\xFFb", 1, 1, 'UTF-8')` and `mb_convert_encoding("a\xFFb", 'UTF-16BE', 'UTF-8')` all move (N4c, N4d, N4f), because they rebuild the string through `mb_convert_buf_init` or `php_unicode_convert_case` with `MBSTRG(current_filter_illegal_substchar)` and the illegal mode, which `mb_substitute_character()` and `mbstring.substitute_character` write and which §3.12 had already placed in this cell. A sweep of 44 functions over 12 encodings and 8 subjects, one subject at a time under five substitution settings, found 21 of the 44 probes move (also `mb_str_split` under `UTF-16BE`, `mb_stristr`, `mb_strrchr`, `mb_strrichr` and `mb_strstr` under `SJIS`, `EUC-JP` and `UTF-16BE`, `mb_ltrim`, `mb_trim`, `mb_scrub`, `mb_strimwidth`, `mb_lcfirst`, `mb_convert_kana`, `mb_encode_numericentity` and `mb_decode_numericentity`); `mb_rtrim` and `mb_str_pad` did not move on those inputs but call `mb_get_substr`, the same helper as `mb_ltrim` and `mb_substr`, which is the reason they are in the class (php-src, not the sweep). The functions that never reach such a helper (`mb_strlen`, `mb_strwidth`, `mb_strpos`, `mb_strrpos`, `mb_stripos`, `mb_strripos` and `mb_substr_count`, whose conversions take the constant `MBFL_OUTPUTFILTER_ILLEGAL_MODE_BADUTF8`, and `mb_strcut`, `mb_check_encoding`, `mb_chr`, `mb_ord`) are the plain class.
  - **The HTML functions return before they look at the charset** on an empty subject (`htmlspecialchars('')`, `htmlentities('')`) and on one with no `&` (`html_entity_decode('abc')`) (N9c, N10c); and an empty charset names the default (N9b). `iconv_strrpos` returns `false` on an empty needle first (N13b).
  - **Runtime `ini_set('mbstring.detect_order')` does not reach `mb_detect_order()`** on 8.5.11 or 8.4.25 (`mb_detect_order()` stays `ASCII,UTF-8` after `ini_set('mbstring.detect_order', 'ASCII')`): on the 8.5 branch `OnUpdate_mbstring_detect_order` writes `detect_order_list`, and the readers use `current_detect_order_list`, which only `mb_detect_order($list)` and the next request's startup set (master, 8.6.0-dev, copies it on a runtime update). S6c's mapping of the name to the cell therefore over-approximates on the supported minors; it is left as it is, because an over-approximated write is the sound direction, and the oracle's read row (N6d) sets the order through the accessor.
  - **The mb-regex state is read on every call.** `mb_ereg('^.b$', "\xC3\xA4b")` flips after `ini_set('default_charset', 'ISO-8859-1')` (R2; `_php_mb_ini_mbstring_internal_encoding_set` resets the regex encoding) and after `mb_regex_encoding('ISO-8859-1')` (R1b), and `mb_regex_set_options('x')` changes what a later `mb_ereg('a b', 'ab')` matches (R8b), so the cell holds the options and the syntax as well as the encoding.
- **Rules (`setting_reads/encoding.rs`).** *Plain* class: omitted or `null` reads; a literal name does not; any other argument is the gap. *Substituting* class: omitted or `null` reads; a literal name is **undecided** (the gap), not "no read", because an invalid subject reads the substitution character. *HTML*: the subject is a deciding argument (`htmlspecialchars` and `htmlentities`: an empty literal subject reads nothing; `html_entity_decode`: a literal with no `&` reads nothing), `''` as the charset is the default, and a subject the call does not show with a default charset is the gap, not a proven read. *iconv*: `''`, `char`, `locale` and any name containing `//` (`//TRANSLIT`, `//IGNORE`, in any case) are undecided (the C library takes the locale's own charset for the first three, and glibc's transliteration table is the locale's), and `iconv_strrpos` has its needle as a deciding argument. *Accessors*: `Omitted` or `Null` reads and does not write; a string, an integer or any other non-`null` literal writes and does not read; an argument the call does not show leaves the read undecided and keeps the write. `mb_regex_set_options` reads on every call (it returns the previous options) and writes when given a string; `iconv_get_encoding` reads for `all`, `input_encoding`, `output_encoding` and `internal_encoding` and not for any other type. A named or spread argument list, or a reader handed over as a callback, is the gap, as every gate's is (§3.9).
- **Rows (`effects.rs`).** The 42 gated names carry `global.read.setting.encoding` (`mb_convert_encoding` and `mb_scrub` also `global.write.setting.encoding`: `php_mb_convert_encoding_ex` adds to `MBSTRG(illegalchars)`, the counter `mb_get_info('illegal_chars')` and the zero-argument `mb_check_encoding()` read, so the counter is part of the cell and the write stays whatever the encoding shows; `mb_chr` restores the counter, `mb_output_handler` and `mb_convert_variables`, the only other writers in php-8.5.11, are not coloured; rows W1 to W3), as does `iconv_get_encoding`, `mb_ereg`, `mb_eregi`, `mb_ereg_replace`, `mb_eregi_replace`, `mb_ereg_match` and `mb_split` (the last six ungated); `mb_internal_encoding`, `mb_regex_encoding`, `mb_http_output`, `mb_detect_order`, `mb_language`, `mb_substitute_character` and `mb_regex_set_options` carry the read and the write, and `iconv_set_encoding` the write. `mb_regex_encoding` was `global.write` (S1's coarse row) and is narrowed. `mb_ereg` and `mb_eregi` also gain an `out_params` row for `$matches`, so a row that names them does not leave the by-reference write unnamed. **Not coloured:** `mb_detect_encoding` (it reads the detect order, the strict-detection entry and, for the literal `auto`, both), `mb_get_info`, `mb_http_input`, `mb_encode_mimeheader`, `mb_convert_variables`, the stateful `mb_ereg_search*` family (a search string and position live in the interpreter, outside the cell) and `mb_ereg_replace_callback` (runs user code) stay `no-effect-row`, with the same gap as before.
- **Ini names (`setting.rs`).** `default_charset`, `internal_encoding`, `input_encoding`, `output_encoding` and `mbstring.internal_encoding` map to the encoding cell, so `ini_get` of one is its read, `ini_set` and `ini_alter` its read and write, and `ini_restore` its write (S6-core had left them on the coarse row because the mb-regex state belonged to no cell). `mbstring.regex_retry_limit` and `mbstring.regex_stack_limit` map to the cell too (review round 1): `_php_mb_onig_search` reads both on every search, and a result changes silently under them (`mb_ereg('(a+)+c|x', str_repeat('a', 18) . 'bx')` is `true`, and `false` after `ini_set('mbstring.regex_retry_limit', '1000')`; row L1). `mbstring.encoding_translation` and `mbstring.http_output_conv_mimetypes` stay unmapped (they feed an output handler): C7 shows the first does not move any reader.
- **A row does not make an unknown call look exhaustive.** An unrowed builtin is a `no-effect-row` gap per call (`effect_unrowed`), and a rowed one carries its whole effect in the row; the slice rows only functions whose whole effect is the cell (the ones above), and `a_new_row_leaves_an_unknown_call_beside_it_open` pins that a body holding `mb_strlen('a')` and an unknown call keeps its gap beside the proven read. The reach rule applies to the `string` parameters of every newly rowed name as it does to any rowed one (an object argument's `__toString`).
- **No fold change.** None of the 42 names is on the fold allowlist (`fold.rs`'s exclusion test names `mb_strlen`, `mb_strtolower`, `mb_substr`), so the fold seam needs no refusal.

**Tests.** `setting_encoding_oracle.rs` (new, `steins-catalog` integration tests): 81 rows against `php`, each a probe run bare and after a setup that writes the cell, asserting soundness (a probe that moved was not called "no read") and precision (a proven read moved) and the expected movement, plus the shape of the table; skipped loudly without `php`, `mbstring` or `iconv`, failing under `CI`. It runs on 8.5.11 and on 8.4.25 (the CI matrix's two minors; every function a row probes exists on both). Eleven of the rows are the plain class's literal name on an invalid subject after `mb_substitute_character(0x41)` and stand still (the witness of each "no read" verdict), and twelve are the substituting class's, which move. `setting_reads/encoding.rs`: the partition test, the positions against the generated table, the rows, and one test per class and shape. `setting_encoding.rs` (new, `steins-infer`): 12 tests of the call shapes through the effect lane (every class with the encoding omitted, `null`, a literal, a variable, a named or spread list, a callback; the accessors; the regex functions; the five ini names; the envelope; propagation to a caller; an unknown call beside a new row). `smoke.rs`: `ConstArgs::literals`. `persist.rs`: the round trip. All 12 `setting_encoding.rs` tests and three of `setting_cells.rs`'s fail on the base (checked: the base tree with the head's two test files copied in: 15 failures, 8 passes); two existing tests that used `mb_strlen` as their example of an unrowed builtin (`effect_diff.rs`, `plugin_channel.rs`) and one in `site.rs` and `throw_knowledge.rs` now use `mb_detect_encoding`.

**Measurement.** Base is the merge base `1212c0f3` (a worktree at that commit, release binary), head this branch's release binary (review round 1 re-ran the A/B against it; the first run was against the pre-rebase base `b4db7fb6`); the binaries differ. Ten public packages, `check --profile strict --no-php --vendor-diagnostics --no-cache --format json` and the default profile: **byte-identical** on every package under both profiles, and the exit codes agree. No public envelope covers an encoding reader, so no finding appears or disappears. `effect-diff`: of 28,846 compared summaries, **72 events on 61 functions** in five packages (monolog 31, phpunit 23, composer 8, Carbon 5, symfony/console 5): `proven-added global.read.setting.encoding` on **30**, `proven-added global.write.setting.encoding` on **41** (every one a caller, direct or through a callee, of `mb_convert_encoding` or `mb_scrub`: 4 direct, e.g. `Utils::detectAndCleanUtf8` and `Xml::convertToUtf8`, 37 through a callee), and `coverage-completed` on 1. Of the 30 reads, **9** call an `mb_*` function with the encoding omitted in their own body (`Utils::substr`, `CarbonInterval::createFromFormat`, `CarbonPeriod::toString`, `Xml::convertToUtf8`, …) and **21** carry it from a callee. The one exhaustive body is `StringContains::matches`, whose only multibyte call is `mb_stripos($h, $n, 0, 'UTF-8')`. No label other than the two encoding labels moves, none is removed. The census of calls (a grep proxy over the ten packages' files, tests included: 93 of the 103 `mb_*`, `iconv*` and HTML call sites are of gated names): 70 omit the encoding (46 substituting and 14 plain, which are proven reads, and 10 HTML, which are a proven read or the gap by their subject), 17 name a literal (6 plain, no read; 7 substituting, the gap; 4 HTML, no read), 6 pass a variable (the gap), and 9 calls of `mb_detect_encoding` and one of `iconv` stay unrowed. `transform effects-envelope`: the plan (2 edits) is **unchanged**; **42** functions that carry a label of the cell and are not exhaustive are newly refused (`effects-not-exhaustive`, e.g. `Utils::substr`, `Xml::prepareString`), 18 existing refusals now list the encoding labels among the proven ones, and 12 class-wide `@phpstan-all-methods-pure` candidates are no longer candidates (`FluentdFormatter`, `HtmlFormatter`, `NormalizerFormatter`, `SqsHandler`, `Xml`, …: each has a method that reads or writes the cell). `throws-envelope`, `loop-to-array-map`, `phpdoc-honesty` and `phpdoc-to-native` are byte-identical.

**Not measured.** The private corpus (not run, per the brief). A glibc libc in the oracle: the `iconv` rows compare `UTF-8` with `ISO-8859-1`, which both libraries answer identically, but the macOS libiconv is what ran, and it does not move for `//TRANSLIT`. The reviewer's glibc 2.31 witness (`iconv_mime_decode('=?UTF-8?B?w6nigqzjgYI=?=', 0, 'ASCII//TRANSLIT')` is `?EUR?` under `C` and `eEUR?` under `C.UTF-8`, `iconv_mime_decode_headers` alike) is why a charset containing `//` is undecided; rows T1 to T3 assert only that the catalog does not call that read absent, not that it moves here. The locale-charset names (`''`, `char`, `locale`) are undecided by rule, not witnessed. PHP 8.1 to 8.3: the oracle ran on 8.5.11 and 8.4.25 only, and the php-src reading is of the 8.5 branch and of a master checkout (8.6.0-dev), not of the 8.4 sources; the claims that matter for a verdict (the plain class, the substitution rows, the HTML and iconv early returns) are the witnessed ones. The substitution character's `long` and `entity` modes (the sweep varied `none`, an integer, `long`, `entity` and `?`; the rows use the integer). `mb_detect_encoding`, which the slice leaves uncoloured, for want of a witness of its three inputs together.

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
from this ADR on its row (S4 certifies it, §3.9). The rest of the list that reads the locale
is coloured in S4 (§3.9): the queue is spent except `htmlspecialchars` and the `mb_*` family, which read the
encoding cell.

#991 is resolved as: `sprintf('%f', $x)` is `{global.read.setting.locale}`
and not exhaustive-pure; `sprintf('%d-%s', 1, 'a')` is `{}` (S7-engine reads
the literal, §3.8); a dynamic format keeps the read; `vsprintf` and `vprintf`
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
  later slices buy and must measure. S4 measured its half (§3.9): the proxy overstated the
  release, since the public `ctype_*` calls are `ctype_digit` and `ctype_xdigit`, which stay as they were,
  and two bodies become exhaustive in all. S5 measured the preg half (§3.10): the proxy overstated the
  release again, 241 summaries gained the read and none became exhaustive.

Gates a slice must pass, against its base: `check` default and strict
byte-identical on the ten packages (no public envelope covers a printf site);
the effect baseline diff naming exactly the printf bodies; `transform
effects-envelope` tags: a function whose only effect was the locale read now
writes `@phpstan-impure global.read.setting.locale` where it wrote nothing
or a class-level pure tag (D2); `transform throws-envelope` byte-identical.

Gates S3 added with the calibration (§3.2): the private project's default-profile
`effect.envelope-exceeded` equals S1's count (+0), because only a proven read is a
default-floor finding; its strict `effect.maybe-envelope-exceeded` gains findings that each
name `value-dependent-read`; and the public packages' default profile findings are identical.

## 7. Consequences

- The registry grows four entries (five with S3's `precision`) and the spec's label table with them;
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
- S4 (§3.9) spends the refusal queue of §4 apart from the encoding cell: the locale readers are rows, each
  but `basename` decided by its call through `setting_read_gate`, and `number_format` is certified. The rows follow
  `PINNED_PHP`; a floor below 8.2 reads `ucfirst` too, which the catalog has no version axis to say, and the data
  sorts under `SORT_STRING | SORT_FLAG_CASE`, which §3.9 leaves undecided for that reason.
- Deferred, with the evidence that forced the deferral recorded:
  - the preg family's locale tables (S5, landed, §3.10): the biggest queued reader and the
    one with a lexical escape (`/u`); it needed the pattern-literal read and a
    measurement before the fold allowlist's `preg_match` learned a refusal, and the
    measurement released no exhaustiveness (a pattern held in a local is still the gap);
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
