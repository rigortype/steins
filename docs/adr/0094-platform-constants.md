# Platform constants: a generated table, a union by default, a pin by opt-in, and 64-bit integers

**Status: proposed (2026-09-10), PENDING ratification.** Drafted from the
owner's grilled design session of 2026-09-10 over issue #598, in
post-hoc-ratification mode (ADR-0077 precedent). This ADR gives a bare
global constant a value, and rules on the three questions #598 raised:
the trust boundary of the mechanism, the treatment of host-dependent
constants, and the scope of project-defined constants. It is the
Steins-side companion of the integer-width question the owner filed
upstream as [phpstan/phpstan#14948](https://github.com/phpstan/phpstan/issues/14948).

## 1. Context: a name that carries no value, by construction

`ArgValue::GlobalConst` is carried by the lowering
(`crates/steins-syntax/src/lower_expr.rs`) and is unproven by
construction — issue #168's ruling — so `PHP_INT_MAX`, `PHP_EOL`,
`E_ALL` and `JSON_THROW_ON_ERROR` all dump `unknown`. The 2026-08-08
gaps note measured 777 nsrt rows corpus-wide whose expression names a
bare constant, and the 2026-09-02 recount counts 60 direct engine-constant
rows, 37 user-constant rows, and 26 `filter_var` rows that read a
const-valued local the resolver already takes (PR #622). Every transfer
rule whose deciding argument is a constant — `filter_var`, `curl_getinfo`,
`preg_*` flags — matches on the *name* today and works around the
missing value (`crates/steins-infer/src/transfers.rs`,
`crates/steins-infer/src/out_params.rs`).

Two things make this a design question rather than a slice. The obvious
mechanism — the sidecar answering `constant('PHP_INT_MAX')` — extends
the fold seam's recognizer surface, which ADR-0060/ADR-0066 fence: the
analyzed source names a symbol and the engine evaluates it. And a
constant, more than a function result, is likely to encode the
*analysis* host rather than the *deployment* target: `PHP_EOL`,
`DIRECTORY_SEPARATOR`, `PHP_OS_FAMILY`, `PHP_VERSION` and `PHP_INT_SIZE`
all vary by machine, and a library cannot assume any one of them.

## 2. Decision: a generated table, evaluated at build time and never at analysis time

Engine constants are answered from a **generated, committed table**,
produced by an xtask from `get_defined_constants(true)` and checked in
the way the `curl_getinfo` option table (#594) and the functionMap
declared-return table (ADR-0069) are. No sidecar call evaluates a
constant during analysis. The analyzed source therefore never names a
symbol the engine executes on its behalf; the fold seam's recognizer
surface is unchanged.

Scope of the table:

- **The extension set is the function catalog's.** The table covers the
  same extensions the declared-return catalog mines (ADR-0014 /
  ADR-0069), so "the extensions Steins knows" is one set for functions
  and constants. A constant of an extension the sidecar reports unloaded
  keeps its value — an unloaded extension makes the constant *undefined*
  at runtime, which is the absence family's business, not a different
  value; `doctor`'s Registry posture may list the table's extensions the
  runtime lacks.
- **Rows carry a PHP-minor range, read off the engines' own presence.**
  `MYSQLI_SET_CHARSET_DIR` left in 8.4; `LIBXML_NO_XXE` arrived in 8.4
  and left in 8.5; `JSON_*` and
  `CURLOPT_*` members arrive per minor. Each row carries `since` and an
  optional `until`, the generator runs over the PHP minors the corpus
  harness already scopes, and the lookup respects the project's
  `PhpTarget` (`crates/steins-db/src/layout.rs`; read from
  `require.php` / `config.platform.php` in
  `crates/steins-db/src/composer.rs`) the way `floor_target_admits`
  already gates the declared-return floor
  (`crates/steins-infer/src/builtin_returns.rs`). A constant outside
  the target's range answers nothing.

  **Amendment (owner ruling 2026-09-11, issue #718).** The range comes
  from the same engines the value diff compares, one per minor, and not
  from a php-src branch scan: a scan reading `.stub.php` `const`s and
  `REGISTER_*_CONSTANT` calls is blind to macro token-pasting, needed a
  per-extension coverage floor to keep its blind spots from minting wrong
  gates, and left 2,551 of 2,566 rows rangeless. Presence is judged only
  against the engines that **loaded the name's extension** — a build
  without `brotli` is not a minor without `BROTLI_*`. A name the older
  engines have and the top one does not gets a **value-less row**: `until`
  and nothing else. The value resolver ignores it, and the absence family
  reads it — at a target whose floor is above `until`, no minor the
  project supports has the name, and `constant.undefined` may say so over
  an analysis host that still has it. This does not reopen §5: the table
  still never says a constant *is* defined, only that it stopped being.
- **A row's value is diffed across those minors, not mined from one of
  them.** "The same value on every host that has the constant" is the
  whole reason a row may be seeded `Verified`, and one engine cannot
  check it. The generator mines every engine it is given
  (`mine-constants --php PATH`, repeatable) and refuses any name they
  disagree about *inside the range the row would claim*; the refusal is
  recorded by name with what each engine said. An engine below the row's
  `since`, or one whose build lacks the name, takes no part — that is
  absence, which is this section's version gate, not a disagreement
  about a value. Families whose numbers are a C library's rather than
  PHP's (`glob.h`, ICU's `UErrorCode`, libpq's enums) are refused by
  family, on the `tokenizer` argument: the name is stable while the
  value is not, and a name-by-name roster silently admits the next
  member the library adds.

## 3. Decision: the default is the union of what the constant can be; a pin is opt-in

A library cannot assume its deployment host, so the **default answer for
a host-dependent constant is the union of the values it can take**, which
is a sound upper bound on every host. An application that knows its host
opts in to a pin.

The classes, and their default spellings:

| class | constants | default |
| --- | --- | --- |
| spec-fixed | `E_*`, `SORT_*`, `JSON_*`, `PREG_*`, `M_PI` and the rest of the value-fixed roster | the literal |
| integer width | `PHP_INT_MAX`, `PHP_INT_MIN`, `PHP_INT_SIZE`, `PHP_FLOAT_*` | the **64-bit** literal (§3.1) |
| closed host sets | `PHP_EOL` → `"\n"\|"\r\n"`; `DIRECTORY_SEPARATOR` → `'/'\|'\\'`; `PATH_SEPARATOR` → `':'\|';'`; `PHP_OS_FAMILY` → `'Windows'\|'BSD'\|'Darwin'\|'Solaris'\|'Linux'\|'Unknown'` | the closed union, as php-src closes it |
| open host sets | `PHP_OS` and any constant whose value set php-src does not close | `non-empty-string` |
| engine version | `PHP_VERSION`, `PHP_MAJOR_VERSION`, `PHP_MINOR_VERSION`, `PHP_VERSION_ID`, `PHP_RELEASE_VERSION` | derived from `PhpTarget`: `PHP_MAJOR_VERSION` → the major when floor and ceiling agree, `PHP_VERSION_ID` → the `int<floor, ceiling>` range the target spans, `PHP_VERSION` → `non-empty-string`; with no target, `int` / `non-empty-string` |

The pin lives in `steins.toml`'s existing `[runtime]` section — the
section ADR-0037 §2 reserves for "boot-truth facts the checker cannot
observe from source", which is exactly what a deployment host is:

```toml
[runtime]
os = "linux"   # one of the PHP_OS_FAMILY values, lowercased
```

`os` pins `PHP_OS_FAMILY`, `PHP_EOL`, `DIRECTORY_SEPARATOR` and
`PATH_SEPARATOR` together — pinning one and leaving the others as a
union would let `if (PHP_OS_FAMILY === 'Windows')` stay alive while
`PHP_EOL` inside it is already `"\n"`, the self-inflicted disagreement
phpstan#14948 §4 describes. `PHP_OS` stays `non-empty-string` under a
pin; php-src does not close its set per family. `doctor` prints the
posture with its source, alongside `warning-handler` and
`final-keyword`.

### 3.1 Integer width is 64-bit, always

Steins does not model 32-bit PHP, and it does not carry a half-model
either. `PHP_INT_MAX` is `9223372036854775807`, `PHP_INT_SIZE` is `8`,
`Base::Int` is documented as 64-bit
(`crates/steins-domain/src/value.rs`), `IntRange` clamps at i64, and
overflow to float is i64 overflow. There is no `4|8` union and no
configuration that would let a user spell the contradiction
phpstan#14948 §1 and §3 document (`PHP_INT_MAX + 1` inferring an `int`
no build produces; a union no runtime check can narrow away). A project
that requires the `php-64bit` virtual package has asked for exactly this
and gets it with no configuration; a project that has not is assumed
64-bit too, and `doctor` states the assumption in one line:
`assume 64-bit int; 32-bit targets unsupported`.

### 3.2 Strata

- A spec-fixed literal, the 64-bit width, and a default union are
  **Verified**: each is true of every host the project can run on.
- A value fixed by a `[runtime] os` pin is **Asserted**: it is the
  user's claim about the deployment host, on the same footing as an
  `@param` claim, and it premises no proof-layer finding.

## 4. Decision: project constants bind from their own file, and cross-file waits

A `const NAME = <literal>;` or a `define('NAME', <literal>)` at file
scope binds its value for reads **in the same file**, from the
declaration the walk already parsed; no sidecar, no generation input.
Non-literal initializers, conditional `define`s and `define` under a
guard decline. Cross-file project constants make a constant's value an
input to every file that reads it, which meets ADR-0092's package
artifacts and name delta; that is a separate slice with its own issue,
and this ADR does not size it.

## 5. Consequences

- #598 becomes three bounded pieces: the generator and table (§2), the
  default/pin resolver with its `doctor` line (§3), and the same-file
  user-constant binding (§4). The `filter_var` const-local rows and
  every transfer that matches on a constant *name* can switch to the
  value once §2 lands.
- The absence family is untouched, in **both** directions: existence is
  a boot-surface fact, the table never says a constant is defined, and
  it is not consulted when one is reported absent either. §2's
  value-less rows were drafted with a reading in the other direction —
  a candidate whose `until` is below the project's floor skipping the
  boot-surface leg — and it is not in the code, because it cannot change
  a correct outcome. `absence_family_available`
  (`crates/steins-infer/src/fold.rs`, issue #28) already declines every
  absence claim made from a runtime outside the declared `PhpTarget`, so
  a runtime that reaches the boot-surface leg is one the project
  supports; if the floor is above the departure, every supported runtime
  is past it and reports the name absent unaided. The only outcome such
  a clause could change is one where the row's `until` is *wrong*, and
  there it manufactures a `constant.undefined` no engine agrees with —
  which is what a boundary between two differently-packaged builds
  produces (§2). The `until` is therefore **recorded and not read**: it
  is what issue #718 asked the table to know, evidence for a reader and
  for whatever tool comes next, and no lane's oracle.
- A host-dependent constant's default union is deliberately wider than
  any one host. A nsrt row asserting the analysis host's literal (the
  upstream fixtures assert `"\n"`) will read `subsumed`, not `match`;
  that is the honest verdict and the harness records it as such.
- The upstream question stays open in phpstan#14948; if PHPStan adopts
  a pin (`phpIntSize` / `assume64BitInt`), the divergence registry
  should record that Steins has no union to opt out of.

## 6. Related

- #598 (ruled), #168 (`GlobalConst` carries no value), #594 (the
  `curl_getinfo` table precedent), #622 (`filter_var` const-local
  resolver), #628 (class constants, a different lane).
- ADR-0014 / ADR-0069 (generated catalogs and their lineage), ADR-0037 §2
  (`[runtime]` as boot truth), ADR-0060 / ADR-0066 (the fold seam's
  trust boundary), ADR-0092 (why cross-file constants wait).
- phpstan/phpstan#14948, phpstan/phpstan-src#6030.
