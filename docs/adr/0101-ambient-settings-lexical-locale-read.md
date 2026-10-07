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
D6 `setlocale($c, '0')` narrows to the read in S4 with the other call-site narrowings (landed, §3.9).

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
with slice S3 (§3.8). The cell roster below names the later children (`timezone`, `env`,
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
  The row is the upper bound and the call site decides it (`locale_read_gate`,
  `site/locale.rs`).
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
  and two bodies become exhaustive in all.

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
  but `basename` decided by its call through `locale_read_gate`, and `number_format` is certified. The rows follow
  `PINNED_PHP`; a floor below 8.2 reads `ucfirst` too, which the catalog has no version axis to say, and the data
  sorts under `SORT_STRING | SORT_FLAG_CASE`, which §3.9 leaves undecided for that reason.
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
