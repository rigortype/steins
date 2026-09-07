# A PHPDoc-vs-value contract checker, independent of any analyzer: feasibility spike and design sketch

Date: 2026-09-07. Spike environment: PHP 8.5.10, `phpstan/phpdoc-parser`
2.3.5 and nothing else. The script is
[`docs/research/phpdoc-runtime-check/spike.php`](../research/phpdoc-runtime-check/spike.php);
its output is reproduced verbatim below and is deterministic (seed 7).
Companion to [the TSTyche survey](20260907-tstyche-type-testing-survey.md),
which took the static half of the same question.

## 0. Framing

The direction this note evaluates, as set on 2026-09-07:

- Detect **mismatches between PHPDoc and the values that actually flow**, at
  runtime, in tests.
- **Independent of every analyzer's runtime** — not built on PHPStan's `Type`
  classes, Steins' engine, or Mago. Whether a type *spelling* is well-formed
  is the analyzer's job; this checker only *interprets* spellings.
- Built on `phpstan/phpdoc-parser` and accepting a **superset** of what the
  analyzers accept, so that a docblock written for any of them (including
  Steins' own vocabulary, ADR-0030 / ADR-0089) can be checked.
- Two presentations: **observation** (values that flowed during a real call)
  and **property-based testing** (`@param` as the generator, `@return` and
  `@throws` as the property).
- **Its own PBT engine**, not a layer over an existing one, and integration
  with PHPUnit and Pest.

Nothing here is a decision; it is the evidence the decision can be drawn
from.

## 1. What the spike established

About 400 lines of PHP: an interpreter from phpdoc-parser's `TypeNode` to a
trinary verdict, a contract reader over `ReflectionFunction` + `PhpDocParser`,
a generator from `TypeNode`, and a shrinker.

### 1.1 Type × value — the observation primitive

```
list<non-empty-string> vs ['a','b']                      yes
list<non-empty-string> vs ['a','']                       no          $[1]: '' is not non-empty-string
array{id: int, name?: string} vs ['id'=>1]               yes
array{id: int} vs ['id'=>1,'x'=>2] (sealed)              no          $: unexpected keys 'x'
array{id: int, ...<string, bool>} vs +x=>true            yes
int<0, max> vs -1                                        no          $: -1 outside int<0, 9223372036854775807>
class-string<Throwable> vs RuntimeException              yes
key-of<array{a: int, b: string}> vs 'b'                  yes
uncased-string (Steins vocabulary) vs '123'              yes
literal-string vs 'x'                                    unknowable  $: 'literal-string' has no runtime interpretation
ArrayObject<int, string> vs new ArrayObject()            unknowable  $: type arguments of ArrayObject are erased at runtime
callable(int): string vs 'strlen'                        unknowable  $: callable signature not checkable at runtime
```

`uncased-string` is parsed by phpdoc-parser as an identifier (syntactically a
class name) and interpreted by name from a registry; that is the whole
mechanism behind "superset".

### 1.2 Observation — a real call checked against `@param` / `@return`

```
kind(1)                  @return   yes          // @return ($k is int ? 'int' : 'string'), decided from the argument value
kindBuggy(1)             @return   no           return [then]: 'string' !== 'int'
idBuggy(new ArrayObject) @return   no           return: stdClass is not an instance of ArrayObject   // @template T bound from the argument
```

Two constructs that are hard statically are trivial here: a conditional
return type is decided because the parameter's value is in hand, and a
template parameter is bound from the argument's runtime kind. The Steins
probe in the companion note answered `unknown` for the same conditional
type; the two instruments are complementary, not competing.

### 1.3 Property — `@param` generates, `@return` / `@throws` judge

```
✓  kind:      200 runs, @return held for every generated input
✗  kindBuggy: property violated after 1 run(s) (seed 7)
     counterexample: $k = 0
     return [then]: 'string' !== 'int'  (returned "string")
✗  maxOf:     property violated after 1 run(s) (seed 7)
     counterexample: $xs = [0]
     return: 0 is not positive-int  (returned 0)          // @param non-empty-list<int>, @return positive-int
✓  label:     200 runs                                    // generated from array{name: non-empty-string, tags?: list<lowercase-string>}
✗  invert:    property violated after 7 run(s) (seed 7)
     counterexample: $pct = 0
     threw LogicException (zero) not declared in @throws
```

Three things the spike found that the design has to carry:

- A parameter without a `@param` tag needs the **native declaration as its
  envelope**; without that fallback nothing is generated and the call fails
  on arity. The same fallback is natural in observation mode.
- Shrinking must only try candidates that **still satisfy the `@param`
  type**, or `non-empty-list<int>` shrinks to `[]` and reports a false
  counterexample.
- With generation size proportional to the run index, the minimal
  counterexample (`[0]`) tends to appear before shrinking is needed.

## 2. Prior art

| | type parser | value check | generation | hook | note |
| --- | --- | --- | --- | --- | --- |
| symfony/type-info | phpdoc-parser | `Type::accepts()` (collections fully traversed; array shapes since 7.3, object shapes 8.1) | — | — | no template binding, conditional types, `@throws`, or trinary verdict |
| CuyZ/Valinor | its own | at mapping time (`Type::accepts`) | — | — | conditional types documented as unsupported; `@valinor-*` overrides |
| azjezz/psl `Type` | PHP API, no string parser | `assert` / `matches` / `coerce` | — | — | does not read docblocks |
| TypeLang parser | its own (a unified PHPStan/Psalm grammar, PHP ≥ 8.4) | — | — | — | the "superset grammar" precedent, with its own AST |
| Eris 1.1.0 (2026-03) | — | — | generators, shrinking, PHPUnit 10–13 | — | the PHP QuickCheck port |
| nikic/php-fuzzer | — | — | coverage-guided | include-interceptor instrumentation | the instrumentation precedent |
| Ruby `rbs test` | RBS | runtime | — | method prepend, `RBS_TEST_SAMPLE_SIZE` | the direct ancestor of observation mode |
| sorbet-runtime | `sig` | runtime (generics erased) | — | method wrapping | one signature, static and runtime |
| Python typeguard | annotations | runtime (first-item sampling by default) | — | import hook rewriting the AST | pure user-land instrumentation |
| Hypothesis `from_type`, typia `random<T>` | types | — | from the type | — | the "type as generator" precedent |

The nearest PHP neighbour, symfony/type-info, has `accepts()` as an auxiliary
and none of the right-hand columns. "Observation plus generation, analyzer
independent, with an explicit Unknowable" is unoccupied.

## 3. Design sketch

### 3.1 The verdict is trinary

`Yes | No | Unknowable`, with a path and a reason. Unknowable is not an error
and not a pass; it is reported and counted. It arises from `literal-string`,
type arguments of anything but native arrays, `callable(...)` signatures,
`T[K]`, `static` / `$this` outside a class context, and the contents of a
`Generator`. Never mixing Unknowable into Yes is this design's version of
TSTyche's `rejectAnyType`, and of the zero-FP posture of ADR-0002.

### 3.2 Layers

1. **Interpreter** — pure, stateless: `TypeNode × value → Verdict`. Names
   resolve through a registry (scalar refinements, classes and enums,
   derived operators such as `key-of<…>`, `int<lo, hi>`, `class-string<T>`).
   An unknown identifier is Unknowable; `never` / `void` are No. A dialect
   setting orders `@phpstan-*`, `@psalm-*` and any tool-specific prefix.
2. **Contract** — `Reflection` plus `PhpDocParser`: `@param`, `@return`,
   `@throws`, `@template`, `@var`, `@param-out`, with template binding from
   values (observation) or from generated values (property), and the native
   declaration as the fallback envelope.
3. **Observe** — where values come from; three backends over the same two
   layers: an explicit API and test assertions; test-time instrumentation
   that inserts `assert()` calls at function entry and exit through
   `nikic/include-interceptor` and `nikic/php-parser`, inert outside
   `zend.assertions=1`; and `ext-opentelemetry`'s `hook()` (zend_observer;
   pre/post callbacks receive parameters, return value and exception; a PECL
   extension).
4. **Property** — the engine of §3.3.

### 3.3 The property engine is its own

Reasons not to build on an existing PBT library: shrinking has to be
**type-preserving** (a candidate is admissible only if the `@param` type
still accepts it — §1.3), generation has to know about Unknowable (a type
that cannot be generated is a reported outcome, not an exception in the
middle of a run), template binding has to be shared between generation and
checking, and a second generator vocabulary beside the PHPDoc one is exactly
what the design avoids. The dependency surface stays `phpstan/phpdoc-parser`
plus PHP ≥ 8.2 (`Random\Randomizer`).

- **Randomness**: `Random\Randomizer` over `Random\Engine\Mt19937`, one
  engine per run seeded from `base seed + run index`; every failure prints
  its seed, and the seed replays the run. The spike is deterministic this
  way.
- **Size schedule**: the generation size grows with the run index, so small
  inputs are tried first.
- **Generators from `TypeNode`**: scalars and refinements, literals and
  unions (uniform arm choice), nullable, `int<lo, hi>`, lists and arrays with
  key and value types, sealed and unsealed shapes with optional keys,
  `key-of` over literal shapes, enums (a case), classes with a no-argument
  constructor, and `@template T of X` bound to a concrete type drawn from
  `X`. Object arguments in general are the hard part (Hypothesis needs
  `builds()` for the same reason): constructor `@param`s recursively, plus
  user-registered factories.
- **Shrinking**: greedy over structural candidates (integers toward zero,
  strings and lists shorter, one element shrunk at a time), filtered by the
  `@param` type.
- **Report**: `counterexample: $xs = [0]`, the failing path and reason, the
  seed, the pre-shrink input, and the Unknowable count for the run.

### 3.4 PHPUnit and Pest

- **PHPUnit**: a trait for `TestCase` providing `forAll(callable)` /
  `property(callable)` plus assertions `assertMatchesType(string, mixed)` and
  `assertContract(callable, ...args)` (observation); failures are
  `AssertionFailedError`s carrying counterexample and seed; an optional
  `PHPUnit\Runner\Extension\Extension` registered from `phpunit.xml`
  subscribes to test-finished events to print the Unknowable summary and the
  seed of the run. PHPUnit ≥ 10 for the event API.
- **Pest**: a plugin following the template — an `Autoload.php` that
  registers `expect()->extend('toMatchType', …)` and
  `expect()->extend('toSatisfyContract', …)`, namespaced `forAll()` /
  `property()` functions reaching the current test through `test()`, and the
  same trait through `Pest\Plugin::uses(...)`. A seed option on the command
  line comes through the plugin's argument handling.

## 4. Open decisions and traps

- **Sampling.** Full traversal of a large array is slow; `rbs test` samples
  100 elements by default, typeguard checks the first. Proposal: full
  traversal up to a bound, then sampling with the verdict marked partially
  Unknowable.
- **`Traversable`.** A `Generator` is never consumed; an `IteratorAggregate`
  is not guaranteed re-entrant. Default Unknowable, traverse on opt-in.
- **Erased generics.** `Collection<int, Foo>` is checkable only to
  `instanceof Collection` (as in sorbet-runtime); traversable ones can be
  narrowed by elements (as symfony does), subject to the consumption rule.
- **Conflicting template bindings.** Two arguments binding `T` to `int` and
  `string`: No, or Unknowable? A least upper bound weakens the `@return T`
  check. Undecided.
- **`@throws` semantics.** PHPStan reads `@throws` as "may throw"; an
  undeclared exception is not a violation there. Treating it as a
  counterexample is stricter than PHPStan and should be switchable.
- **Side effects.** Property mode really executes the function. It cannot be
  applied blindly to code touching a database, files or the network.
  Steins' effect facts (ADR-0018) are a natural permission signal: only a
  function proven pure gets automatic property runs. This is the most
  Steins-specific point of contact.
- **Instrumentation coexistence.** The include-interceptor backend competes
  with pcov / xdebug for the stream wrapper; php-fuzzer solved this with its
  own wrapper.
- **PHP versions.** Being runtime, the checker naturally reports "the truth
  on this PHP"; TSTyche's `--target` maps to a CI matrix and nothing else.
- **Correctness of the interpreter itself.** Analyzer `assertType` fixtures
  assert static types and cannot be reused directly. Beyond table-driven
  tests, a differential oracle is available: for an expression the analyzer
  types as `T`, the runtime value must satisfy `T` — the check phpstan-src
  declined to add inside its own test case (phpstan/phpstan discussion
  #6673), run from the outside.

## 5. Relation to Steins

- **The vocabulary complement.** `annotate` and `transform` write Steins'
  own type vocabulary into docblocks, and PHPStan parses only part of it. A
  checker whose registry knows that vocabulary gives such a project a way to
  verify those annotations against real values in its own test suite, with
  no dependency on any analyzer's spelling compatibility.
- **A safety net under `transform`.** Before and after an array-shape → DTO
  transform, the old shape contract can be observed against the values that
  flow.
- **Not nsrt.** nsrt compares analyzer against analyzer; this compares
  docblock against execution.
- **Unknowable is the same posture as zero-FP.**

## 6. Next steps (none taken)

1. Minimal scope: Interpreter + Contract + the explicit API + PHPUnit
   assertions + the property engine. Instrumentation backends are the
   second stage. Dependencies: `phpstan/phpdoc-parser` only; PHP ≥ 8.2.
2. Fix the boundary of "superset" first: the core is what phpdoc-parser
   parses, the vocabulary is a registry, the dialect is configuration.
3. A name and a repository of its own; it is not a Steins subcommand.
