# Catalog seeding is demand-driven; the initial FP-gate corpus

**Catalog (ADR-0014) seeding order**: type signatures are generated
mechanically from php-src stubs in bulk; **effect coloring follows measured
demand** — builtin call frequency counted over the FP-gate corpus, colored
top-down. Uncolored functions widen to unknown-effect (a miss, never an FP),
the only seeding order compatible with the zero-FP bar. The by-ref
out-parameter survey (79 functions) seeds conditional purity; the
pseudo-constant settings table (ADR-0008) comes from the same notes.
Extension modules are added on demand, driven by what the sidecar's
`ReflectionExtension` audit reports actually loaded.

**Initial FP-gate corpus (ADR-0013 concretized)** — balanced across
io-heavy, pure-computation, and metaprogramming-heavy code:

- composer/composer — scale, real-world complexity
- phpunit/phpunit — reflection and metaprogramming stress
- guzzlehttp/guzzle — `io.net.http`, PSR-7
- monolog/monolog — `output`/`io.fs`, handler abstractions (envelope
  carriers in the wild)
- symfony/console, symfony/process — component culture, `io.process`
- league/flysystem — `io.fs` behind interfaces
- nikic/php-parser — large pure-computation library (the `Pure` side)
- nesbot/carbon, cakephp/chronos — `nondet.time` heartland; exercises
  pseudo-constant timezone settings
- the private monorepo, via the ADR-0013 injection point

## Amendment (2026-10-01): catalogued pure is certified, not "foldable" — PENDING ratification

Issue #851. **Status: PENDING ratification.** Designed autonomously under the
owner's standing delegation.

### Context

`effect_labels` answered a name with no coloured row from the folding
allowlist alone, so "catalogued pure" meant "on the fold allowlist". Every
other builtin was uncatalogued and left the body that called it `…?`, however
plainly it did nothing. `function f($c) { return is_int($c); }` was `…?`, and
so was every PHPUnit exception: their base constructor calls `is_int()` and
`array_keys()`.

The allowlist answers a different question: is this name safe and worth
executing in the sidecar? Purity is one of its preconditions (ADR-0008's
folding gate), not its definition. Type predicates and array readers are pure
and are left out of the allowlist on purpose, since narrowing and shape
projection already answer them.

### Measurement

Over the ten public corpora (34,222 bodies, 25,009 of them `…?`), each
uncatalogued builtin was ranked by how many bodies it **alone** keeps `…?`,
meaning that cataloguing that one name would make the body exhaustive. The top
of the ranking: `is_string` 80, `trigger_error` 45, `assert` 43, `fclose` 20,
`is_array` 20, `array_map` 19, `array_key_exists` 14, `array_keys` 14,
`defined` 10, `strcmp` 9, `is_int` 8, `array_values` 8, `function_exists` 7.
Single names undercount a family: the type questions alone release 166
bodies, the array readers alone 48, and the two together 305, because one
body often calls both. The ranking picked the families and the evidence below
picked their members. Most of the rest of the top forty is I/O, error
handling, autoloading, or a `string` parameter.

### Decision

1. **Purity is not foldability.** A builtin is catalogued pure when it is
   foldable, as before, or when it is on the closed certified list beside the
   coloured rows (`CERTIFIED_PURE` in `steins-catalog`). The allowlist is not
   widened to get there, and a certified name does not become foldable.
2. **What certifies a row.** At `PINNED_PHP`, php-src must show that for every
   argument its parameter types accept, the call:
   - runs no userland. That rules out a `callable` parameter; a `string`
     parameter, since coercive typing converts an object argument through its
     `__toString`; a loose comparison or string cast of an argument, which
     reaches the same method; a class lookup by name, which autoloads; and a
     read of an object's property table, which initializes a lazy object;
   - takes no reference;
   - reads only its arguments: no ini setting, locale, clock, environment,
     superglobal, engine symbol table or error state;
   - performs no I/O;
   - throws nothing beyond the `TypeError` its parameter types already state.
     The colour row is also the predicate several passes ask as "does the
     catalog know this name", and the throws pass reads a known name with no
     throw row as throwless.

   `php -r` witnesses settle argument-dependent behaviour. PHPStan's
   `hasSideEffects => false` corroborates and is never the source of record:
   it says the same of `current`, `key`, `get_class` and every form of
   `array_keys`, each refused below.
3. **A diagnostic is not an effect.** An `E_WARNING` or `E_DEPRECATED` raised on
   bad input runs an installed error handler, and that handler's effects
   belong to its registration (`set_error_handler` is `global.write`). The
   diagnostics the language's own operators raise, such as an undefined key or
   an array-to-string conversion, have no effect origin for the same reason,
   and the fold allowlist's rows already hold the rule: `preg_match` warns on a
   bad pattern and `strlen(null)` is deprecated, yet both are pure. No earlier
   ADR stated it for the effect lane. ADR-0096 §3 holds a narrower bar for
   `statement.no-effect`, where a diagnostic is a reason to keep the statement,
   and that bar stands. Its refusal list gains the three certified names that
   diagnose on a literal their types admit: `array_flip`, `array_key_exists`
   and `key_exists`.
4. **A row may hold at one arity only.** `array_keys($a)` copies keys, but
   `array_keys($a, $v)` compares `$v` loosely with every element, which runs an
   object's `__toString` or a lazy object's initializer. Its argument-blind
   row, the upper bound over every form, stays uncatalogued, and
   `pure_at_arity` certifies the one-argument call. The effects pass reads the arity from the call's
   positional argument list, so a named or spread list, or a use as a callback,
   keeps the `…?`.
5. **Widening the list is a separate, measured act.** Each family enters with
   a ranking that motivates it and php-src evidence for every member. It never
   enters because PHPStan or another catalog lists it.

### The certified families

- **Type questions:** `is_string`, `is_int` (with `is_integer` and `is_long`),
  `is_float` (with `is_double`), `is_bool`, `is_array`, `is_null`, `is_object`,
  `is_scalar`, `is_numeric`, `is_iterable`, `is_countable`, `is_resource` and
  `get_debug_type`. In `ext/standard/type.c` each reads the zval's type tag.
  `is_numeric` parses a string with the engine's locale-independent
  `zend_strtod` and raises no diagnostic. `is_iterable` and `is_countable` test
  the class's interfaces and handlers without calling `getIterator()` or
  `count()`. `is_resource` reads the resource's own closed state.
  `get_debug_type` names the class from the class entry.
- **Array readers:** `array_first`, `array_last`, `array_key_first`,
  `array_key_last`, `array_values`, `array_flip`, `array_reverse`,
  `array_slice`, `array_key_exists` (with `key_exists`), `array_is_list`, and
  `array_keys` at one argument. These are ADR-0070's certified readers and
  presence predicates. Each takes its container as `array`, so no object
  reaches it as the container, and each copies, reorders or tests keys and
  values without comparing or converting a value. `array_flip` warns on a value
  that is neither an `int` nor a `string` and skips it. `array_key_exists`
  deprecates a `null` or fractional key and throws `TypeError` on an array or
  object key.

### Excluded, with the reason

- `current` and `key` accept an object and read its property table. On a lazy
  object that runs the initializer, and the call is deprecated as well.
- `get_class()` without an argument throws `Error` outside a class, and no
  throw row records it.
- `is_callable`, `is_a`, `is_subclass_of`, `get_parent_class`, `class_exists`
  and `interface_exists` autoload a class named by a string.
  `defined` and `function_exists` read engine symbol tables that a later
  declaration changes.
- `array_search` compares values loosely and `array_combine` casts keys to
  strings, so an object value runs `__toString`.
- The string family (`strcmp`, `ord`, `dirname`, `bin2hex`, …) declares
  `string` parameters. In a file with `strict_types=1` an object is a
  `TypeError` there instead of a `__toString` call, so a narrowing on the
  calling file's mode could certify the family later. The ranking puts that at
  70 more bodies.
- `spl_object_id` and `spl_object_hash` answer object identity, which ADR-0008
  counts as `nondet`.
- `trigger_error`, `assert`, `restore_error_handler`, `error_reporting`,
  `fclose`, `getcwd` and the like run handlers, read ini or perform I/O.

### Consequences

Measured over the ten corpora against the PR #850 base: 305 bodies become
exhaustive and none stops being exhaustive. `transform effects-envelope` writes
895 tags where it wrote 816. All 79 new tags are former `effects-not-exhaustive`
refusals, and no tag is lost. 50 are in PHPUnit, 46 of them class tags, most
on exceptions under `PHPUnit\Framework\Exception`. `check --profile strict` reports
the same 8,967 findings, and `transform throws-envelope` writes the same plan.
No pure row adds a label, so `effect.envelope-exceeded` cannot newly fire
through one. `statement.no-effect` can now fire on a literal call such as
`is_int(1);`, and the ten corpora contain none.

A certified name is a known builtin to every pass that asks, which is the
coupling issue #375 found. `function_exists('array_first')` folds to present,
and a project's own global polyfill of a certified name is ambiguous with the
builtin, so a call to it is `…?` where it was an edge to the polyfill. Both
answers hold at `PINNED_PHP`. The catalog has no version axis, so a name newer
than the project's PHP (`array_first` and `array_last` are 8.5) folds to
present there too, as the allowlist's `str_increment` (8.3) already does.

The certification also surfaces holes the fold allowlist already had and this
list does not repair. `strlen($o)` and the allowlist's other `string`-parameter
rows run `__toString` under coercive typing, as ADR-0096 noted. `in_array`
compares loosely, and `count($countable)` runs a userland `count()`.
`is_callable` reaches the effects pass as a known builtin through its
out-parameter row and can autoload. They are recorded here, not fixed; the
second amendment of this date closes them.

## Amendment (2026-10-01, second): a builtin call is pure only where its arguments reach no user code — PENDING ratification

Issue #856. **Status: PENDING ratification.** Designed autonomously under the
owner's standing delegation.

### Context

The first amendment of this date certified names argument-blind and recorded
the fold allowlist's holes: `strlen($o)` runs `__toString` under coercive
typing, `in_array` compares loosely, `count($countable)` runs a userland
`count()`, `is_callable('Foo::bar')` autoloads. The holes were wider than the
allowlist. Every catalogued function row answered the same whatever its call
was handed, coloured rows and the out-parameter-only rows (`sort`, `reset`,
`settype`, `preg_replace`) included, so `implode(',', $objects)`,
`json_encode($value)` and `usort($rows, $cmp)` with an unresolved comparator
read as pure too. A user method reached that way can write state, perform I/O
or throw, so the body's effect set was short: unsound, which the zero-FP
posture forbids.

### Decision

1. **A row says what a builtin does; the call site says what its arguments
   reach.** The catalog states, per parameter, the user code an argument can
   make the builtin run (`arg_reach`, one `ArgReach` per position): `Inert`;
   `Coerced`, a coercive `string` parameter converting an object through
   `__toString`; `Object`, the builtin converting, counting or reading an
   object itself (`strval`, `count`, a lazy object's initializer); `Nested`, the
   builtin also converting or comparing what an array holds (`implode`,
   `in_array`, `json_encode`); `Callback`; `Autoload`.
2. **The row is derived, not listed.** From the mined arginfo, by declared
   type: a scalar other than `string` is `Inert` (an object is a `TypeError`),
   `string` is `Coerced`, a class, interface, `object` or `iterable` is
   `Object`, `array` and `mixed` are `Nested`, a declared `callable` is
   `Callback`, a resource position is `Inert`, a union takes its strongest
   member, a variadic tail repeats its last position, and a position past a
   non-variadic list is an `ArgumentCountError` raised before anything runs. A
   curated list overrides the derivation where php-src does less (`count`
   never reads an array's elements, `intval` and `gettype` read a tag, the
   comparator sorts and the invokers hand values to the callback, an
   out-parameter is written unread) or where the type cannot say
   (`is_callable` autoloads, `preg_replace_callback_array` maps to callables).
   Each override and each reach kind is witnessed in both calling modes on PHP
   8.5.11. A certified name is `Inert` everywhere.
3. **The call-site rule.** A call to a catalogued builtin is pure only when
   every position it fills is ruled out: `Coerced` by an argument shown not to
   be an object, or by `declare(strict_types=1)` in the calling file, where
   the object is a `TypeError`; `Object` by an argument shown not to be an
   object; `Nested` by an argument shown to hold no object at any depth;
   `Callback` and `Autoload` never, at a plain call (a callback the effects
   pass resolves already takes ADR-0033's road). Otherwise the call keeps its
   row's labels and marks the body `…?`. A named or spread argument list
   reaches whatever strictness alone does not rule out. A builtin handed to
   another as a callback is called with arguments of its invoker's choosing,
   and in coercive mode whatever the file declares (`array_map('strlen',
   [$o])` runs `__toString` under `strict_types=1`), so any reaching position
   counts. An invoker's own non-callback arguments are held to the rule as a
   plain call's are. The rule covers every catalogued function: coloured,
   pure and out-parameter-only rows alike.
4. **What an argument is shown to hold** (`ArgShape`, on each call origin of
   the trace payload) is structural; the effects fixpoint runs before the walk
   whose narrowed facts could say more, so none are read.
   - By form: a scalar literal, a magic constant, a concatenation or
     interpolation, a comparison, `<=>`, a logical connective, `!`,
     `instanceof`, `isset`, `empty`, a cast to a scalar, a ternary or `??` of
     such, and an array literal of such hold no object. Any other array
     literal, and an `(array)` cast, is not an object.
   - A variable whose every binding the frame makes: a by-value parameter,
     holding its declared type on entry, or a local the frame neither imports
     nor captures, unset on entry. Either holds the meet of that with every
     write the frame makes to it: an assignment, a compound assignment, an
     element write, an increment, an `unset`. A `foreach` or `catch` binding, a
     destructuring of a value not shown object-free, or a frame with `global`,
     `static`, `$$v`, `extract`, `include`, a reference or a by-ref capture
     makes it unknown. A bare variable handed to a named call, a
     `$this->`/`self::`/`parent::`/`Foo::` method or a `new` is checked against
     the callee's by-reference flags: the catalog's by-value certification for
     a builtin, the project's parameter list for a function, method or
     constructor (a method's declaration binds every override's flags; a
     constructor answers only for an exact class), and by value for an engine
     Throwable's constructor, which reflection confirms for all 60. Handed to
     anything else, it counts as written.
   - `$this->name`: the declared type of the property, which a read always
     yields. A `__get` for an unset typed property is checked against the
     type too (PHP 8.5.11 throws `TypeError` for an object returned for an
     `array` property).
5. **The string family is certified under the same rule.**
   `certified_at_call_site` lists `strcmp`, `strncmp`, `strcasecmp`,
   `strncasecmp`, `strspn`, `strcspn`, `substr_count`, `ord`, `chr`,
   `bin2hex`, `hex2bin`, `dirname` and `unpack`: each php-src body at
   `PINNED_PHP` reads only its arguments, byte by byte or through
   `zend_tolower_ascii`, takes no reference and performs no I/O. They are not
   on `effect_labels`, so no other pass reads them as known builtins; the
   effects pass resolves an otherwise unresolved call against the list and
   answers pure only where the call site rules the `string` parameters out.
   Left out, each reading the locale or an ini setting: `basename` and
   `pathinfo` (`php_mblen`, `ascii_compatible_locale`), `strnatcmp` and
   `strnatcasecmp` (C `isdigit`, `toupper`), `substr_compare`
   (`zend_binary_strncasecmp_l`), `parse_url` (C `isalpha`), `escapeshellarg`
   (`php_mblen`), `strip_tags` (C `isspace`), `number_format`, the `ctype_*`
   family, `htmlspecialchars` (`default_charset`) and the `mb_*` family.
   `strtok` keeps its position in interpreter state.

### Measurement

The ten public corpora, 34,222 bodies, 24,704 of them `…?` at the #855 base.
A throwaway instrumented build recorded each call site's argument shapes and
the fixpoint was recomputed offline per proof source, with the soundness half
over the allowlist and out-parameter rows:

| Proofs admitted | Bodies newly `…?` |
|---|---|
| none | 891 |
| `strict_types=1` for `Coerced` | 696 |
| and an argument's form | 675 |
| and a parameter the frame never rebinds | 570 |
| and a call's declared return type | 564 |

Coloured rows added 13 more, and a call's declared return type was left out
for the 6 it bought. The cost was dominated by `sprintf` values, `count`,
`in_array`, `implode` and `str_replace`; locals and property reads were the
largest unproven argument kinds, and the write summary and typed properties
were then added for them. A broad string-family candidate list released 57
bodies in the same simulation, where argument-blind certification would have
released about 70; the thirteen names php-src supports release 37 below.

The landed build, against the base: 364 bodies become `…?` (267 functions and
methods, 97 closures) and 37 become exhaustive, all through the string
family, so `…?` bodies go from 24,704 to 25,031. No body's proven labels
change. `check --profile strict` is byte-identical (8,912 = 8,912), and so is
`transform throws-envelope`. `transform effects-envelope` writes 853 tags
where it wrote 895: 46 tags become `effects-not-exhaustive` refusals and 4
are new, from `bin2hex`, `dirname` and `substr_count` calls the rule now
certifies. The lost tags are, by the call that keeps them `…?`: a `sprintf`
value or an `implode` piece the scan cannot show object-free (28), `count`,
`in_array` and `explode` on unproven arguments (9), an unresolved callback
handed to `set_error_handler` or `register_shutdown_function` (2), and seven
single cases. PHPUnit keeps the exceptions under its base exception that
the first amendment of this date released: the constructor's `$code` is
written only with `0`, and `parent::__construct()` reaches an engine
Throwable, so both proofs hold.

### Consequences

No pure row adds a label, so `effect.envelope-exceeded`, `effect.liskov-widened`
and every other proven-lane finding are unchanged; exhaustiveness never
manufactures a finding (`provably_impure` reads labels only). What moves is
the `…?` marker, the effect baseline, and the tags `effects-envelope` writes.

Holes this rule does not close, recorded for follow-up:

- **Engine method and constructor rows.** `new DateTime($o)`, `$pdo->query($o)`
  and `new \RuntimeException($o)` coerce through `string` parameters too, and
  the method and constructor rows are not held to `arg_reach`.
- **The throws pass.** It reads a known builtin with no throw row as
  throwless, so a `__toString` that throws through `strlen($o)` is still
  missing from the throw set.
- **Operators and destructors.** `'a' . $o`, `(string) $o`, `$o == 'x'`, a
  property read through `__get`, an `ArrayAccess` offset and a destructor run
  by overwriting a variable are user code the effects pass does not model at
  all. This rule answers for the builtin's own conversion only.
- **Precision left on the table.** A literal `sprintf` format names which
  values render through `%s` (about 7% of its failing sites would pass), and
  `in_array(…, true)` compares strictly; neither is read yet.

### Note (2026-10-03): a literal printf format names the values it renders — PENDING ratification

Issue #860, catalog half (run 2, S7). **Status: PENDING ratification.** Designed
autonomously under the owner's standing delegation. This note covers the
literal printf format only; the call site that reads it, and the strict flag of
`in_array`/`array_search`, are the engine half and get their own note.

The row's `Object` at `sprintf`'s first value is the reading for a format the
call site cannot read. For a literal format, `steins_catalog::format_reach`
parses php-src's `php_formatted_print` grammar into one reach per value:
`Object` where a conversion naming the value is `%s`, `Inert` where every one
is numeric (`d u c o x X b e E f F g G h H`). Witnessed on PHP 8.5.11, an
object handed to a numeric conversion becomes a number with a warning and runs
no `__toString`. Positional `n$` is honoured and leaves the in-order counter
alone; a value several conversions name takes the strongest; a value no
conversion names is never read; too few values is an `ArgumentCountError` for
`sprintf` and a `ValueError` for `vsprintf`, which is argument checking and not
user code (ADR-0099 §3.3).

The parser returns `None`, leaving the call to the row, for any format it
cannot read as the engine does: an unknown or missing conversion, a padding
quote with nothing after it, an argument number of zero or past `INT_MAX`, a
width or precision past `INT_MAX`, a `*` width or precision (it consumes a
value as an integer and shifts which value a conversion names), a `%` behind
modifiers (it renders `%` and still consumes a slot), and a position past an
internal bound. A malformed format may run `__toString` for an earlier `%s`
before it fails, so the whole format is unreadable, never a prefix. The
verdict was checked against PHP over 960,000 generated runs (random formats,
zero to six tagged objects, `sprintf` and `vsprintf`): no value a run rendered
was parsed `Inert`, and where the run succeeded with enough values the
rendered set equalled the parsed `Object` set.

`printf_family(name)` names the layout: the format at position 0, the values
the positions after it (`sprintf`, `printf`) or one array (`vsprintf`,
`vprintf`), where the array is `Nested` when some conversion is `%s` and
`Inert` when none is. `fprintf` and `vfprintf` have no row of any kind (the
format sits at position 1) and are not named: a layout without a row would
read a call the effect lane still cannot place, and a stream writer's row is
its own issue.

`array_search` joined the call-site certified list (`certified_at_call_site`),
the names whose own effects are certified and whose call is pure only where the
call site rules out every reaching argument. Before, it answered
`no-effect-row` in the effect lane, which no call-site proof could ever
discharge (it is not on the fold allowlist, which is how `in_array` reached the
reach rule). `vsprintf` does not join: `%f`, `%g` and `%G` read `LC_NUMERIC`
(`%e` and `%E` do not), which contradicts Decision 2. `sprintf` has the same
hole through the fold allowlist, which predates this note; both are issue #991.
Its reach answer is unaffected, so `printf_family` and `format_reach` still
cover `vsprintf` and `vprintf`. Nothing reads the parser yet, so the only
effect is `array_search`'s gap kind: `no-effect-row` becomes `user-code-reach`
unless the arguments are already shown object-free. On the ten public corpus
packages `check` is byte-identical under `default` and `strict`, and no
function's labels, exhaustiveness or throw lane move; 20 of 28,846 summaries
change the kinds behind their `…?` (9 swap `no-effect-row` for
`user-code-reach`, 7 lose `no-effect-row` beside an existing `user-code-reach`,
4 gain `user-code-reach` beside a `no-effect-row` another call keeps), every
one an `array_search` call.

## Amendment (2026-10-02): the call-site rule holds at every site — ratified 2026-10-02

ADR-0099 (issue #865) takes the holes the second amendment of 2026-10-01
recorded. The call-site rule of that amendment's §3 holds at every site kind,
in both the effect and the throw lane: builtin functions, engine methods and
constructors (a per-method reach row, issue #858), and operator sites (issue
#859). Function resolution and both lanes ask one predicate, `knows`, whether
the catalog knows a spelling; a known name with no row on an axis is a
coverage gap on that axis. Being on the fold allowlist or a certified list
does not by itself make a name throwless (ADR-0099 §3.3). The literal-format
and strict-comparison refinements stay with issue #860.
