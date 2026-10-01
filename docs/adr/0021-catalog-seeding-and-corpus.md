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
out-parameter row and can autoload. They are recorded here, not fixed.
