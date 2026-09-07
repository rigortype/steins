# TSTyche as a reference for type testing: what it does, and what PHPStan and Steins lack

Survey date: 2026-09-07. Sources: a local checkout of
[TSTyche](https://tstyche.org/) 7.2.4 (HEAD `f608f708`), its documentation,
a lightning talk on it at Gotanda.ts #1 (2026-09-07,
[slides](https://qiita.com/tomoasleep/slides/9a52c56a39d0f2d9748d) and the
[typed-i18n gist](https://gist.github.com/tomoasleep/0773c3ebb1c2bcf812ac80587471de8f)
they use), phpstan-src `9fb9e6346`, and Steins master `df4a2e8`. The Steins
probes below ran on a debug binary built 2026-09-01; a rebuild of master was
blocked by the toolchain pin on the surveying machine, and the two answers the
note leans on (`unknown` for a conditional return type and for `1 + 1`) are
both tracked as open work (issue #260), so they are not stale-build artefacts.

The question the survey answers: TSTyche is a *type* test runner for
TypeScript — assertions about what a type resolves to, run without executing
the test file. PHPDoc types are checked by no runtime, so the only way to see
that a generic or conditional type does not collapse to `*ERROR*` or `never`
has been to run the analyzer and read the result. Does TSTyche's design say
anything about turning that into a test? A follow-up note,
[the PHPDoc-vs-value contract checker](20260907-phpdoc-runtime-contract-checker.md),
takes the *runtime* half of the same question.

## 1. What TSTyche actually is (read from the source)

**Test files are never executed.** The published `tstyche` module exports a
no-op `Proxy` for `describe`/`test`/`expect` (`source/index.ts:1-14`); the
only file the runner ever `import()`s is a `// @tstyche template` file
(`source/runner/FileRunner.ts:92-93`). Assertions are found by walking the
`CallExpression`s of the test file, keyed on the `import … from "tstyche"`
statement (`source/collect/CollectService.ts:27-129`,
`source/collect/IdentifierLookup.ts:18`). Test names must be string literals
for the same reason.

**One language service per TypeScript version, in-process.** The runner wraps
`ts.server.ProjectService` rather than `tsc`
(`source/project/ProjectService.ts:59-67`), creates one per `--target`
version and runs files sequentially (`source/runner/Runner.ts:100-117`). Each
test file is checked three times: the original text, the text with directives
erased, and the text after the ability rewrite described below.

**Matcher semantics.**

| matcher | implementation |
| --- | --- |
| `.toBe<T>()` | a bespoke 628-line structural comparison (`source/structure/Structure.ts`); TypeScript's `isTypeIdenticalTo` was removed in #643. Fresh literals normalised, single-member unions collapsed, unions order-insensitive, intersections order-sensitive, `NoInfer` compared as a substitution type, recursion guard for self-referential generics (#791, 7.2.4) |
| `.toBeAssignableTo/From<T>()` | one `typeChecker.isTypeAssignableTo` call with the arguments swapped |
| `.toBeCallableWith(…)` and the other ability matchers | the test file's text is rewritten into real TypeScript (`expect(f).type.toBeCallableWith(a, b)` becomes `f (a, b)`), re-checked, and the assertion passes iff no *new* diagnostic appears (`source/layers/AbilityLayer.ts`, `Layers.ts:25-64`) |
| `.toRaiseError(…)` | consumes the semantic diagnostics that fell inside the `expect()` range, matched positionally and count-exact; **deprecated in 7.0** (#705) in favour of `@ts-expect-error` plus `checkSuppressedErrors` |
| `.not` | negates every matcher |

**Collapsed types are rejected by default.** `rejectAnyType` and
`rejectNeverType` (both default `true`) inspect the first argument on *both*
sides of the assertion with `getTypeAtLocation(node).flags & TypeFlags.Any`
(`source/expect/ExpectService.ts:71-76`, `source/reject/Reject.ts:29-57`).
The only escape is syntactic: the node must literally be the `any` / `never`
keyword, so `type Any = any; expect<Any>()` is still rejected. The diagnostic
reads `The 'any' type was rejected because the 'rejectAnyType' option is
enabled. If this check is necessary, pass 'any' as the type argument
explicitly.` This is the feature that answers the question above: an
assertion whose subject collapsed fails no matter what was asserted.

**The rest.** `checkSuppressedErrors` erases every `@ts-expect-error`
directive, re-checks, maps each resurfaced diagnostic back to its directive
line and requires a message fragment (`...` wildcard, exactly one error per
directive, `!` to opt out). Directives: `// @tstyche if { target: ">=5.7" }`
(file- or node-level, silently skipped outside the range),
`// @tstyche fixme` (expected to fail; passing is an error),
`// @tstyche template` (the file exports a string of generated tests).
Multiple TypeScript versions come from the npm registry into a store
(`~/Library/TSTyche/typescript@X`, hand-written tar reader, SRI-verified),
`--target '>=5.6'` is expanded to concrete minors at config time, and the
chosen `lib/typescript.js` is dynamically imported. Zero runtime
dependencies; `typescript` is an optional peer; Node ≥ 22; TypeScript 7
(tsgo) unsupported until it has a programmatic API.

**The talk's claims.** About 300 stars in 2026-09, adopted by Effect.ts; a
`Flatten<D>` typed-i18n dictionary as the example, with `.toBe` on flattened
keys and `.not.toBeCallableWith` on bad keys and missing or extra parameters;
0.4–0.8 s on a laptop; the author's takeaways were the BDD-style readability,
the clarity of failure logs, and that `.not` cases are awkward to express as
ordinary type assertions.

## 2. PHPStan today, measured

`PHPStan\Testing\assertType`, `assertNativeType`, `assertSuperType`
(added 2025-06-19, 2.1.18; unused by the nsrt fixtures) and
`assertVariableCertainty` live in `src/Testing/functions.php`. The rule that
consumes them, `FileAssertRule`, is `#[AutowiredService]`, and the autowiring
extension tags every `Rule` implementer (`AutowiredAttributeServicesExtension.php:84-90`),
so it is **active in every analysis**. phpstan.org's page for `phpstan.type`
says only that a user "will not encounter this during normal usage"; the
function works in any analysed file. A scratch file at level 9:

```
kinds.tst.php:23:Expected type never, actual: 'a'|'b' [identifier=phpstan.type]
kinds.tst.php:25:PHPDoc tag @var for variable $bad contains unresolvable type. [identifier=varTag.unresolvableType]
kinds.tst.php:27:Expected type mixed, actual: *ERROR* [identifier=phpstan.type]
kinds.tst.php:30:PHPDoc tag @return contains unresolvable type. [identifier=return.unresolvableType]
kinds.tst.php:31:Expected type int, actual: *ERROR* [identifier=phpstan.type]
kinds.tst.php:33:Dumped type: 'int' [identifier=phpstan.dumpType]
```

What is missing, against the list in §1:

- **Comparison is string identity** on `describe(VerbosityLevel::precise())`
  (`FileAssertRule.php:79-91`). A spelling change in the renderer means
  rewriting every expectation (phpstan/phpstan discussion #6614).
- **No default that rejects a collapsed type.** `assertType('*ERROR*', …)`
  passes; the nsrt corpus itself asserts `*ERROR*` in 75 files and `*NEVER*`
  in 98 of 1,626, deliberately. The `*.unresolvableType` identifiers catch
  only syntactically unresolvable PHPDoc, not a generic that resolved to
  `mixed` or a conditional that collapsed to `never`.
- **No `not`**, no callable-with. "This call is rejected" is written as
  `@phpstan-ignore argument.type` under `reportUnmatchedIgnoredErrors`, the
  same construction as `@ts-expect-error` plus `checkSuppressedErrors`, but
  keyed on the identifier rather than a message fragment.
- **No runner.** `TypeInferenceTestCase` exists for extension authors and
  turns each assertion into a PHPUnit data-provider row (slow at scale,
  phpstan/phpstan #10757). There is no named test, no `.only`, no version
  matrix.
- **No type lane.** A type expression cannot be the subject; it has to be
  bound to a value through `@var` or a function's `@return`. That is the
  "run it without types and look" workflow in its entirety.

Psalm has `@psalm-check-type $x = T` (subtype allowed),
`@psalm-check-type-exact` and `@psalm-trace` as annotations, and likewise no
runner. The only user-land precedent found for `assertType` outside
phpstan-src's own tests is a 2024 blog post testing ReactPHP Promise v3's
template types.

## 3. Steins today, measured

ADR-0053 gives Steins the dump lane (`debug.type`, `debug.phpdoc-type`,
`debug.var-dump`) with the `(asserted)` marker for Asserted-stratum facts,
and its point 9 already designs `PHPStan\Testing\assertType` as "a comparator
plugged into the same name-keyed recognizer", deferred to its own issue;
point 12 refuses Steins-native spellings of the dump call while leaving the
door open "with a Steins-only capability the PHPStan spelling cannot name".
`crates/steins-infer/src/assert_harness.rs` installs a harness-only sink for
the nsrt run; in an ordinary `check` an `assertType` call is a call like any
other, and in a project without phpstan in `vendor` it reports
`call.undefined-function`. The nsrt harness (`xtask/src/nsrt.rs`) judges by
relation, not by string: both sides go through `steins_contract::lower_str`
and `normalize::subsumes` in both directions, yielding
`match / equal / subsumed / differ / unsupported`, with the int→float veto
and the `// lint < 8.3` PHP-version gate.

Probes with `dumpType` / `dumpPhpDocType` (2026-09-01 debug build):

```
lanes.php:15  dumped type: 'a'                          // keys(): value lane — the body returns 'a'
lanes.php:16  dumped phpdoc type: 'a'|'b' (asserted)    // keys(): contract lane — key-of<array{a: int, b: string}>
lanes.php:17  dumped type: unknown                      // kind(1): conditional return type, not modelled
lanes.php:18  dumped phpdoc type: no declared contract  // same
probe.php:16  dumped type: 3                            // strlen('abc'): folded
probe.php:17  dumped type: unknown                      // 1 + 1: no operator-value node yet (#260)
```

PHPStan answers `'a'|'b'` for the same `keys()`. Two lessons:

1. A library author asserting on a declared type wants the **contract lane**;
   the value lane `subsumes` it. A TSTyche-style `toBe` belongs on the
   contract lane, and value-lane precision is a separate matcher.
2. A PHPDoc construct Steins does not model lowers to `unknown` /
   `no declared contract`. With a "fail on `unknown` by default" posture —
   TSTyche's `rejectAnyType` — the assertion becomes a detector for exactly
   the construct that did not survive lowering.

## 4. Correspondence

| TSTyche | PHPStan | Steins today |
| --- | --- | --- |
| `.toBe<T>()` (structural identity) | `assertType` (rendered-string identity) | nsrt `is_proven_equal` (subsumes both ways, spelling-independent) |
| `.toBeAssignableTo<T>()` | `assertSuperType` (`isSuperTypeOf()->yes()`) | `normalize::subsumes`, one direction |
| `.toBeAssignableFrom<T>()` | — | the same relation, reversed |
| `.not` | — | — |
| ability matchers (count new diagnostics) | `@phpstan-ignore` + unmatched-ignore reporting | — (a closure body walked and its diagnostics counted would be the same shape) |
| `rejectAnyType` / `rejectNeverType` | — (`*ERROR*` can be asserted) | `unknown` is a sentinel the nsrt relation refuses to grade; no user-facing default |
| `expect<T>()` type lane | — (docblock binding) | `dumpPhpDocType`; `lower_str` evaluates a type string with no walk |
| `--target '>=5.6'` | — | the sidecar's PHP minor; nsrt recognises `// lint` gates |
| `// @tstyche if { target }` | `// lint < 8.3` (nsrt convention) | `lint_gate` in the harness |
| `checkSuppressedErrors` | `reportUnmatchedIgnoredErrors` (identifier only) | the three suppression channels of ADR-0023 |
| `fixme`, `template` | — | — |
| test files are never executed | same (analysed only) | same (walked only) |

## 5. What transfers

Three ideas, in the order they matter:

1. **Reject a collapsed subject by default.** An assertion whose subject is
   `*ERROR*` / `never` (PHPStan) or `unknown` / an Asserted-stratum claim
   (Steins) fails unless the expectation names that outcome literally.
2. **A type lane.** Let a type string be the subject. Steins can do this with
   `lower_str` and the acceptance relation alone, no walk involved; PHPStan
   would need a small rule.
3. **Ability matchers over "expect an error".** Say what a call cannot do;
   the implementation counts diagnostics inside a closure body. TSTyche
   deprecated its positional error matcher for this reason.

The runner ergonomics (names, `.only`, reporters, a version matrix) are
worth having but are the cheap part in either ecosystem.

**Candidate slices for Steins** (non-normative; they inherit ADR-0053
point 9 and would need an amendment to that ADR, since it places the
comparator "beside the conformance apparatus, not in the check pipeline"):

- *Slice 1 — recognise `assertType` / `assertSuperType` in `check`.* Emit a
  `Layer::Debug` id judged by the nsrt `classify` relation, never by string
  equality, so PHPStan-spelled expectations pass as `equal`. Defaults:
  `unknown` fails whatever was expected; an Asserted-stratum fact does not
  satisfy an expectation unless opted in (ADR-0052 §5); `subsumed` fails
  `assertType` and passes `assertSuperType`. Reserve `PHPStan\Testing\` as
  compat vocabulary the way the dump pair is, so `call.undefined-function`
  stays silent without phpstan in `vendor`. One trap to close in the same
  slice: an `assertType($x, …)` call currently puts `$x` into a loop body's
  write set (measured in the nsrt harness), and a test surface that perturbs
  the subject it observes cannot be trusted.
- *Slice 2 — the type lane.* A pair of type strings judged by `lower_str`
  and `subsumes` in both directions. This is the direct answer to "does this
  derived type operator (ADR-0089) or conditional type survive lowering?",
  it needs no sidecar, and it runs in the wasm playground.
- *Slice 3 — a runner.* `*.tst.php` discovery, `describe`/`test`/`skip`/
  `todo` read from the CST and never executed, a list/summary reporter, and
  a `// @steins if { php: ">=8.3" }` gate reusing `lint_gate`. A PHP-version
  matrix stays a CI concern; the harness's refusal of a `--php-version`
  override holds. Steins-specific expectations (value precision `'a'`
  against `'a'|'b'`, the `(asserted)` marker) currently live only in Rust
  tests; a `.tst.php` corpus in the repository would give them a home, as
  TSTyche's own `typetests/` does.

**A thin runner for the PHPStan ecosystem** is also cheap and lives outside
Steins: discover `*.tst.php`, run `phpstan analyse --error-format json` over
them, map `phpstan.type` / `phpstan.superType` to failed assertions, and ship
three rules PHPStan lacks (`assertNotSuperType`; `assertNotCallable` over a
closure body; a rule that fails `assertType` when the actual type describes
as `*ERROR*` or `*NEVER*` unless the expectation says so). `phpstan.phar` is
one file per release, so TSTyche's store and `--target` range translate
directly. It cannot validate Steins-authored docblocks beyond the subset
PHPStan parses (ADR-0030, ADR-0089).

## 6. What does not transfer

- TSTyche itself: it is bound to the TypeScript checker's API.
- Its structural `toBe`: Steins has the acceptance relation; TSTyche wrote
  its own because `isTypeIdenticalTo` was unusable, and PHP has no such
  problem.
- Executing test files. PHP has no type-level expression syntax, so a type
  lane means type *strings* or docblock binding, but the "analyse, never
  run" model is the same.
- Positional error matching as the primary API — TSTyche retired it.
- PHPStan-style string comparison; the spelling problem is already solved
  in the nsrt relation.
