# Not Implemented

This document exists so no other document has to be vague. Everything here is
either **designed with no code**, or **known imprecision** that costs true
positives. Nothing here costs *false* positives: an unknown widens to silence,
which is the whole shape of the zero-FP bar.

Sequencing and exit criteria live in [`docs/ROADMAP.md`](../ROADMAP.md); this is
the semantic inventory.

## Designed, no code

### Type and inference machinery

| Surface | ADR | Note |
| --- | --- | --- |
| Narrowing N5/N6 — property-chain guards, static-prop channel, structured loops | 0052 | Deferred out of v0.1.0 by owner decision; designed in full in ADR-0052 §7–8. |
| Template scope transfer | 0051 | Templates as functions, render sites as call sites. Out of v0.1.0 scope by owner decision; promoted only if dogfooding demands it. |
| `template-type<Subject, Owner, 'TName'>` resolution | 0032 | **Resolves on the declared side** (issue #361): a spelled parameterization of the owner, a one-level inheritance edge to it, or the template name itself is rewritten into the type it names before lowering, and judged exactly as that type is. **A `@return` whose subject is a class-level template of the receiver's class resolves too** (issue #362), off the generics carry the receiver object holds at the call site — two lookups, one level each, seeding the arms a hand-written `@return` would seed. **A subject naming the declaration's own function- or method-level `@template` resolves too** (issue #363), bound from the carry of the argument that flowed into a top-level `@param Owner<…, T, …>` (or, for `@param T $p`, from the argument's proven value) — and since #361 rewrites `template-type<Box<T>, Box, 'T'>` to `T`, the same read serves both spellings. The spelling was made recognized vocabulary first (issue #360), which is also why its owner argument is exempt from `untyped.generics` — a class-reference position, not a missing type argument. What still floors to `Opaque`: a receiver that carries nothing to read (a `$this`, static, non-exact or **declared** receiver, or one whose value carry an earlier method call swept — a registered divergence, entry 13); a **union or intersection subject**, where PHPStan unions over the subject's class names; a subject reaching the owner only through a **generic intermediate**, which is substitution rather than lookup (ADR-0032's amendment: one level, no recursion); every argument spelling outside #363's binding rule (a nested or nullable `@param`, a named or spread argument list, a by-ref or variadic parameter, two occurrences that disagree, a **bounded** template — which reads its bound instead); and the resolution does not run over `@var`/property docblocks at all, which keep the #360 floor. |
| Derived type operators past `key-of` / `value-of` | 0089, 0090 | **The `lower_generic` roster has landed** (#473): `non-nullable<T>`, `return-type<F>`, `parameters-of<F>`, `exclude-from<T, U>` and `extract-from<T, U>` project into an existing `ContractTy` and are judged, spelled and round-tripped as the type they project to. With them, the three rules ADR-0089 fixes for the whole family: **kebab-case** naming (a hyphenated spelling is not a legal PHP identifier, so nothing can shadow it — the lowercase spelling the family was proposed in could not have done this, since PHP class names are case-insensitive); **projection over representation** (no new `ContractTy` variant, so the operator spelling does not survive lowering and `annotate` never emits one); and an **arity-blind `Opaque` floor** through `DERIVED_OPERATORS`, which also closed a live wrong-`No` — `key-of<int, int>` used to lower to `Class("key-of")` and answer a definite `No` for every non-object value. **Deferred**: `constructor-parameters-of<C>` (#474), the one operator whose operand resolves against machinery that already exists, held back because it reads the class registry and so lowers as a pre-lowering rewrite at the seam `Cx::resolve_template_types` runs at rather than in `lower_generic` — `template-type`'s declared-side precedent. And the six **shape modifiers** (ADR-0090, #475), one pair per part of ADR-0062's shape fact: `partial-of` / `required-of` on the presence axis, `unsealed-of` / `sealed-of` on the seal axis, `pick-of` / `omit-of` on the field set. All six require a `ContractTy::Shape` operand — the rule that keeps `sealed-of<array<string, int>>` from sealing a field-less `MapOf` into `array{}` and answering a confident `No` for every non-empty array it was written to describe; `pick-of` / `omit-of` additionally require a *sealed* one, since an unsealed tail re-admits the key they claim to remove. `isList` is recomputed by `ShapeFact::normalize` rather than carried, so a modified `list{…}` generally spells back as `array{…}` under divergence entry 2. **Refused, not deferred**: `Record` (denotes `array<K, V>` exactly; ADR-0030's entry 6 is the measured precedent for refusing a re-spelling), `Readonly` (PHP arrays are value types, so it denotes the same set), `InstanceType` (`class-string`'s bound is dropped at lowering by design, issue #236, so the operand carries nothing to read — and the container idiom is issue #363's argument carry), `NoInfer` (ADR-0032: no solver to steer), `ThisType` / `ThisParameterType` / `OmitThisParameter` (no `this` parameter in a PHP callable type), `Awaited` (no promise in PHP core), and the four casing intrinsics (the refined-string grid holds the predicates, issue #240; the sidecar folds the transforms). A test pins that these keep the ordinary class reading, so the absence is not read later as an oversight. **What the shape modifiers wait on**: an operand that is a *name*. Written inline, every modifier is longer than the type it projects to, and Steins resolves no type alias at all — `@phpstan-type` is a silence obstacle, not an expansion (ADR-0049 A14, issue #195). ADR-0090 §7 makes issue #472 (alias resolution) the prerequisite. **Left open**: whether an operator that provably states nothing — `omit-of` with a key the shape lacks, `non-nullable<null>` yielding `never` — should raise a `phpdoc.*` id at the contracts floor. One id or neither, in its own slice with its own fp-gate evidence. |
| The hyphen reservation, and the unknown-vocabulary id | 0091 | **Designed, no code.** A phpdoc type identifier containing `-` is vocabulary and never a class: no namespace resolution, no shadowing, and never `ContractTy::Class`. Today only half of that holds — `is_shadowable_pseudo_type` short-circuits on the hyphen, but `lower_identifier`'s catch-all still lowers an unrecognized name to `Class(norm)`, whose acceptance leg answers a definite `No` for every non-object value. So `@param non-empy-string $s` (one letter) is a contract that rejects every string, and `positive-integer` rejects every int. `KNOWN_UNENFORCED` and `DERIVED_OPERATORS` close this one spelling at a time, which makes the safety a maintenance property; the rule makes it structural. **Not a Steins-only defect**: on the conformance suite's namespaced `int-range` fixture, 5 of 16 analyzer configurations reject a call the fixture marks valid, having resolved the keyword into a class that cannot exist. Across the 69 namespaced fixtures using a hyphenated keyword, 84 over-rejections in 48 fixture-instances across 7 tools carry the fingerprint — a floor, since Qodana exhibits the defect without printing a qualified name. Phan is the control (it implements `int-range`, passes, and its over-rejections elsewhere drop out); PHPStan already behaves as ADR-0091 proposes, reporting `parameter.unresolvableType` rather than manufacturing a contract. **Why the reservation is airtight**: a hyphenated identifier cannot be a class (PHP's compiler rejects it — zero hyphenated class-likes in the seeded catalog, zero in 6,670 corpus PHP files), cannot be a `@template` name and cannot be a `@phpstan-type` alias (the tag scanner's `is_ident_byte` excludes `-` while the type lexer's `is_ident_cont` includes it, so a tag-position name stops at the hyphen). All three of the possibilities that force silence on an unrecognized identifier are therefore closed, which is what makes an unrecognized *hyphenated* one a **provable** docblock defect rather than an undecidable one. **Two slices, deliberately not bundled**: ADR-0091 §3 is the rule and strictly removes wrong answers, needing no calibration; §6's `phpdoc.*` id for unrecognized vocabulary is a judgement call whose one FP source is other tools' spellings Steins does not model, so its surface floor is measured against the fp-gate rather than assumed, and is not `default`. Under the rule `KNOWN_UNENFORCED` stops being a safety valve and becomes that id's allowlist, and `DERIVED_OPERATORS` is subsumed for flooring. `unset` is **not** an instance: it carries no hyphen and is safe because PHP reserves the word (ADR-0087 §2.2) — a sibling reason, same shape. **Owner ruling 2026-08-24** (ADR-0091 §4.1): a user-defined type alias may be named `foo_bar` (PHPStan/Psalm compatible) and may **not** be named `foo-bar` — rejected outright rather than silently truncated at the hyphen, which is what the tag scanner does today and is a divergence from the phpdoc-parser oracle that #472 must register. The space is reserved, not frozen: a plugin registers utility types into it as a second registration kind on the existing `steins-plugin.json` manifest (ADR-0039/0068), which is what keeps §6's allowlist open. That makes the diagnostic **plugin-set dependent** — dropping a plugin introduces findings on the docblocks that used its vocabulary, a baseline that moves with configuration rather than code, which ADR-0022 has to be told about. |
| Callable signatures beyond the closure-variance arm | 0033 | A declared `callable(P): R` is checked against a *closure argument*; nothing else consumes it. |
| Arrays *of* resources, resource **parameters**, open/closed state | 0056 §8.7 | The resource leaf itself has **landed** (ADR-0056 §8): 19 stub-mined producers seed a `resource`(`\|false`) arm lane, the ordinary `=== false` subtraction narrows it, and a scalar/class parameter handed the result is a mode-independent finding. What is left: `stream_socket_pair`'s `array` of resources (a `ShapeFact` holds `Fact`s and no `Fact` is a resource), the *consuming* direction (`fwrite($notAResource, …)`, which needs the builtin parameter surface), and `fclose($h); fread($h, …)` — a real bug, but a dataflow analysis rather than a type one. `is_resource()` is not a narrowing predicate yet, for the same producer-vs-filter reason (§8.4). |
| A **function-scope** `T\|unset` that reports | 0087 §5.6 | The **vocabulary** (issue #395), the **top-level semantics** (issue #396) and the **positions** (issue #397) have all landed: `unset` lowers to its own leaf instead of a class named `unset`, is non-shadowable, round-trips through the speller, contributes no value — `\DateTime\|unset` accepts and refuses exactly what `\DateTime` does, in every position — and a **top-level inline** `@var T\|unset $x` seeds the name as possibly-unbound, reported by `phpdoc.maybe-undefined` (`Layer::Contract`, `Floor::Contracts`) on any read `isset`/`empty`/`??`/`??=`/an assignment/the defaulting idiom has not discharged. Every other position is **inert** by ruling, not by omission (ADR-0087 §5). What is deferred is one leg: an inline `@var T\|unset $x` inside a **function, method or closure** never emits the id, although `include`-inside-a-function is a real idiom and §8.3's positional dam rule is the machinery it would need. The premise does not transfer — a script scope has no proof of absence, which is what makes the declaration the only premise there, while a function scope *does*, so the declared and the proven claims would have to be ordered before the id could speak. Until then a function-scope local keeps `variable.undefined` / `variable.maybe-undefined` unchanged. One residue of the landed slice (ADR-0087 §8): where two docblocks declare the same name the read attributes to the later one. |
| Value-provenance labels | 0038 | Reserved as the general mechanism in place of taint analysis. |
| Ecosystem packs — PSL, Serde, Valinor, PSR | 0044, 0045 | Dependent shapes, witness refs, mapper returns as runtime truth. The mapper-boundary types are exactly where legacy modernization needs truth. |
| Plugin contract | 0012, 0039, 0068 | **Partial.** The manifest channel has **landed**: a `type: steins-plugin` Composer package's `steins-plugin.json` registers effect labels (vendor-root checked) and colors plain functions, whose labels enter the *declared* lane with the taint kept. Deferred: everything the sidecar half was for — synthetic declarations, pattern subscriptions, booting the real framework (the `plugin` JSON-RPC method is still the stub returning `widen`), method colorings, value-provenance registrations, response caching by environment fingerprint, and the ADR-0044/0045 packs. |
| Per-package vendor budgets | 0015 | Descent into `vendor/` bodies is implemented (diagnostics off); the budget cap that would bound it, naming its cutoff per the Certainty discipline, has no code. Vendor propagation runs uncapped today. |

### Diagnostics and CLI

| Surface | ADR | Note |
| --- | --- | --- |
| `call.too-many-arguments` | 0049 §6 | Internal targets only — userland too-many runs clean and is never a finding. Waits on the sidecar reflect slice. The only registered id with no emitter. |
| Scoped policy — `[paths.sets]`, `[[policy]]` | 0023 | Designed in full, including semantic `where` matchers. The pipeline stage exists as a no-op with a seam. |
| `doctor` (full report) | 0054 | The **minimal** `doctor` (ADR-0054 C3 scope — index-bound posture report, runs no emitter) has **landed**, and so has `--format json` and part of the richer ADR-0054 C4 audit: Catalog (builtin catalog pin vs. analysis version, hierarchy/foldable table sizes) and Registry totality (emittable/pending id partition) sections. Deferred: the dump-site count (waits on the unlanded D3/D4 recognizer) and `contract_touches_class`'s project-wide count (needs a second, index-only entry point the checker does not expose yet). |
| `check --fix` fix-its | 0010 | Autofix as a first-class diagnostic payload has **landed**: a finding may carry a `Fix` (a title plus byte-span `FixEdit`s), `--format json` shows it as an additive key, and `check --fix` pours a run's fixes into one atomic plan that writes only past the ADR-0034 dual-verification post-check — a refusal is named and nothing touches disk. One family ships: deleting a committed `\PHPStan\dumpType()` / `\PHPStan\dumpPhpDocType()` statement (`debug.type`, `debug.phpdoc-type`), the remedy ADR-0053 names. Deferred: every further fix family. `debug.var-dump` carries no fix by decision, not by deferral — deleting legal working PHP is the author's call. |
| `lsp` | 0048, roadmap M6 | Position queries are *constrained* today (replay over retention, canonical entry states, no global-ordering dependence) but not built. The flagship capability is type-directed member completion. |
| `mcp` | 0010, roadmap M7 | The agent-driven dry-run → diff → approve → apply loop has **landed** as `steins mcp`: an MCP server on stdio with four tools (`list_transforms`, `plan_transform`, `apply_plan`, `check`), plan and apply deliberately separate, and a plan handle scoped to the serving process. Deferred: an `annotate` tool, MCP resources and prompts, and a tool that applies a finding's `fix` payload (the payload is returned; the agent applies it). Also **landed** (#491, #534): `check` answers from the published generation, warm ≡ cold asserted at the MCP surface, so the resident process gets ADR-0092's warm path there. `plan_transform` and `apply_plan` still re-analyze from scratch on every call — deliberately, since their post-check verifies the plan against hypothetical edited text no generation holds (the dirty-buffer lane, ADR-0092 §6, issue #492). |
| `init` / config generators | 0020 | **Refused**, not deferred — zero-config is the banner. |

### Runtime knowledge

| Surface | Note |
| --- | --- |
| Extension-class reflection, past the first slice | The sidecar's `reflect()` **does** resolve extension classes against the project's own PHP (#269): a class an installed extension provides carries its methods, constants, properties and inheritance edges. What stays out: a class the runtime cannot reflect — an unloaded extension, `--no-php`, no `php` on `PATH` — is `Unknown`-silent exactly as before, and no absence-family finding is ever premised on a reflected declaration (it resolves; it does not convict). |
| The full effect catalog | What ships is a frequency-seeded starter set; ADR-0014's php-src stub sourcing is not built. |
| Computed folding purity | Folding permission is a hand-picked allowlist, not a derived property. |
| Locale/timezone pseudo-constants | The ADR-0008 opt-in that would let `mb_*` and locale-sensitive functions fold. |

## Known imprecision

Places where Steins is quieter than it could be.

**Byte strings** (ADR-0080): a PHP string value carries its bytes, so equality,
array-key identity and offset absence are exact for a literal like `"\xC0"`.
What such a value does **not** do is fold: the sidecar wire is JSON and cannot
carry arbitrary bytes, so a non-UTF-8 argument is not sent and every
`PORTABLE` builtin over it (`strlen`, `substr`, `strrev`, `str_pad`, `md5`,
`base64_encode`, …) falls back to its declared return envelope instead of a
constant. Restoring the exact fold needs a lossless encoding on the ADR-0024
protocol and is ADR-0080 §3.1. The name lanes decline in the same direction —
a byte string never resolves as a class, function, method, effect label,
include path or preg pattern — and the phpdoc spelling lane widens rather than
inventing an escape the grammar does not have. Separately, *source files* are
still read UTF-8-lossily, so a file that is not itself valid UTF-8 collapses
before parsing (ADR-0080 §3.2), which also leaves the salsa backdating in §3.3
open.

**Builtin parameter types reach only what the engine can be asked and the
native relation can spell** (issue #423, ADR-0056 §9; the whole-surface gap of
issue #391 is closed). A builtin's parameters now have a type source — the
sidecar's `ReflectionFunction::getParameters()`, Verified — and
`type.argument-mismatch` plus the possibly pair judge a builtin argument by the
same relation they judge a project one, at the call-site file's `strict_types`,
with the internal-null coercive carve-out §9.3 measures. What is left is
bounded and stated:

- **No engine, no judgment.** `--no-php`, a sidecar that cannot spawn, the
  pre-boot playground and a replay table recorded before the field all answer
  `None`. That is the sound subset (ADR-0004) and not a gap to close: ADR-0069's
  note of 2026-08-17 explains why no static row may stand in.
- **Positions the native relation does not model.** `array`, `iterable`,
  `callable`, `object`, `resource` and a class-typed position all decline, so
  `array_map('f', 'notanarray')`, `str_replace([], 1, 1)` and
  `fwrite($notAResource, …)` stay silent. Every one of those declines exactly as
  the same spelling written on a *project* parameter does — the cap is
  `NativeType`'s member set, in one place, for both.
- **Named arguments to a builtin** (v1): name→position binding for an internal
  target is its own slice.
- **`mixed`, untyped, by-reference and variadic positions**, each for a reason
  §9.4 gives; these are silence by construction rather than unfinished work.

The argument *carrier* is a separate cap in the same place, and it has shrunk to
one shape: the possibly-grade pair reads `$v`, `f(g($x))`, `f($o->m())` and
`f($a['k'])` (issue #418) but not `f($o->prop)`, which has no condition-operand
variant of its own to narrow through and so cannot be shipped guarded.

The **return** seam's half of the same pair (issue #537) is capped narrower
still: it reads `return $v;` and nothing else, so `return g();`,
`return $o->m();` and `return $a['k'];` are silent there even though the
argument seam reads all three. Its object arms carry their own cap — a class
arm is judged only where the class can have no subclass (`final`, or an enum),
because the acceptance oracle decides an *exact* class and an extensible one
may have a subclass the return type accepts.

**Generic type-argument carry drops conservatively past a variable binding**
(issue #295, ADR-0032 stage 1). `$box = new MutableBox(1); f($box);` now
judges the full `MutableBox<int>`, not just the class half — the direct-`new`
argument position (already landed) and the variable-binding carry (v0.1.5)
together cover assignment. The arguments are dropped again wherever they
could have gone stale: any method call on the object drops them, and so does
passing the object to any function whose signature so much as mentions the
parameter, whether or not that function's body actually observes or mutates
the type argument. Only a function that never mentions the parameter in its
own text keeps them, which is provable from the function's text alone. This
costs true positives only — a live argument dropped early is a widen, never a
wrong answer.

**The value IR carries what it can spell, and a call it cannot spell is
silence** (ADR-0027; ADR-0075 §3 as amended by issue #386). A method or static
call in value position is an `ArgValue::MethodCall` and resolves as its
assignment form does — `takesString($b->unwrap())`, `dumpType($b->get())`,
`Foo::m(1)`, `(new C(1))->m()`. What still lowers to `ArgValue::Other` there,
each because the carrier has no way to say it: a **dynamic** receiver or method
name (`$o->$m()`, `$obj[0]->m()`, `$var::m()`), a receiver deeper than one
property hop (`$a->b->c->m()` — depth 1 is a `Receiver::Prop`, which is
carried and then declines as a dispatch target by ADR-0052 §7), a **spread**
argument list at the call (`$o->m(...$args)`, and the same for a plain
function call), a method **first-class callable** (`$o->m(...)`, which is a
value and not a call), a `static::` static class (late static binding), and a
`clone`/property/offset expression in receiver position. A **nullsafe** call is
carried but never rebound, in any position (§3.1) — the receiver may be `null`
and the summary does not say so. None of these costs a false positive: an
un-carried call is a value nobody claims to know.

**`class-string<T>` carries no bound** (issue #236 landed the bare form; the
parameterized one is issue #10). `class-string`, `interface-string`,
`trait-string` and `enum-string` are judged as a value refinement — they refute
`''`, `'0'` and `'123'`, and satisfy `string`/`non-empty-string`/
`non-falsy-string` — but a written `class-string<Foo>` drops to plain
`class-string`. That widens rather than misstates (every `class-string<Foo>` is
a `class-string`), so a bound the annotation states costs a true positive, never
a false one. Whether a concrete identifier names a real class is never asserted
in either form: that needs the class table, and the refinement is decidable in
the refuting direction only.

**Control flow** ([narrowing.md](narrowing.md)):

- Loops are `Opaque` — write/read-set invalidation only, no loop-carried facts
  (ADR-0052 N6, deferred out of v0.1.0 by owner decision).
- `try`/`catch`/`finally` is `Opaque` for value flow (catch *matching* works).
- Reachability is decided **structurally only** (ADR-0078 §5, issue #199). Every
  statement carries a `BodyEnd` — `Terminates` / `FallsThrough` / `Unknown` —
  computed from the CST, and `body_end` folds a statement list to the same
  verdict. What it does not do is feed *value* flow: a construct that early-
  returns on every branch is now provably terminal, but a fact about a variable
  the dead tail never reads is still carried as if that tail ran. The judgment's
  own silences are `try`/`catch`/`finally` (excluded whole — `finally` overwrites
  the exit point), `goto`/labels, a `switch` with case-to-case fall-through, and
  a provably-infinite loop containing a `break` whose target is unresolved.
  A second, orthogonal question — `body_has_terminator`, does the body exit the
  function anywhere — splits a falling-through body into the unconditional and
  the conditional class. The `type.return-missing` pair is the only consumer
  today; the level-4 dead-code family is the deferred one, and reads `Unknown`
  the opposite way round.
- Static properties are not a fact lane; property chains (`$a->b->c`) are a
  `Barrier` (ADR-0052 N5, same owner deferral).
- `??` refines an *array offset* in guard position (ADR-0062 S5); over any other
  operand it yields a value fact only.
- Array shapes carry key presence, optionality and list-ness, and the
  `isset`/`array_key_exists`/`empty`/`??` family narrows them (ADR-0062) — but a
  write at a key Steins cannot prove widens the whole shape rather than refining
  it, and the value side of `in_array`/`array_search` declines to project through
  a shape at all (its answer is a multi-base union the value domain cannot spell).
- `array_slice` projects through a shape (ADR-0062 Amendment B), but only as far
  as the element union, the key class and list-ness carry: it claims no size
  bound, and it never projects *positionally* from a declared shape — a key set
  has no runtime order (§2). An order-witnessed array is where the exact slice
  comes from, and only there.
- An array literal with an unproven element seeds a `Fact::Shape` rather than
  dropping the fact (ADR-0062 Amendment C), so its keys, entry count and sealing
  survive what its values do not. The *order-dependent projections* consume the
  order witness now (issue #328): `array_keys` of a literal-seeded shape is the
  sequence (`list{'a', 'b'}`), and `array_values`/`array_reverse`/`array_slice`
  execute over it — while a *declared* shape still takes the key-set widening,
  which is the answer a key set with no runtime order deserves. `array_key_exists`
  and `isset` in **value** position are `bool`/unknown against any array fact,
  declared or witnessed — they are implemented as guards, and the value transfer
  is unwritten.

**`settype`'s cast grid states only cells a probe measured** (issue #595). The
statement-position write is real, and the cells it declines are the honest
remainder rather than unfinished work in most cases: `'object'` writes a
`stdClass`, which the value domain has no member for; an array cast to
`'string'` is the `E_WARNING` cell PHP fills with the literal `'Array'`; and a
float's decimal spelling is `precision`-ini dependent, so the value answer is
declined and even the abstract one is only `uppercase-string&non-empty-string`
— never `numeric-string`, since `is_numeric('NAN')` is `false`. Two cells are
genuinely deferred: a **non-numeric string's** integer value (`(int)'12abc'` is
`12`, a leading-numeric-prefix rule this slice does not author, so the answer
widens to `int`), and an **out-of-range float value's** truncation
(`(int)1.0E+30` is the hardware's `5076964154930102272`, not the saturation the
numeric-string path takes). Every declined cell leaves the by-ref invalidation
standing, so the name is forgotten exactly as it was before the row existed.
The vocabulary the row introduced (`WrittenWhen::CallReturns` plus the
statement-position seed) is what `array_splice` and the other by-ref writers in
the same bucket need, and none of them carries a witness yet.

**Objects** ([object-model.md](object-model.md)):

- `__get`/`__set` are not modeled; `__call` is an absence-proof obstacle.
- Traits are an obstacle, not a modeled method source.
- `@method`/`@property`/`@mixin` are absence-proof obstacles too, not member
  sources: `$obj->scopeActive()` still resolves to nothing.
- A `Member` fact on a `final` class is not treated as exactness in v1.
- `Closure::bind`/`bindTo` rebinding drops the binding.

**Propagation**:

- Binding descent is capped at 8 frames (`MAX_BINDING_DEPTH`), plus on-stack
  recursion detection. Past the cap: silence.

**Docblock tags read as obstacles only** (ADR-0049 A14): `@method`,
`@property`, `@property-read`, `@property-write`, `@mixin`, `@phpstan-type`
aliases, `@phpstan-import-type`. Steins recognizes each tag's presence and its
subject — the method name, the property name, the `@mixin` target — and records
one `(class-like, kind, subject)` obstacle per tag site. A class-like carrying
any of them anywhere in its resolved reach (parents, interfaces, `@mixin`
targets transitively) is not enumerable, so the absence family is silent on it,
exactly as for `__call`. Reading them as **member sources** — resolving
`$model->scopeActive()` or `$model->created_at` to a type — remains deferred,
as does the subject-granular discharge channel that would re-enable the
absence proof for a class-like's *undeclared* remainder (ADR-0039's to design).
Their types are never parsed: only the subject is.

## Engine and performance

**Cross-run persistence and the warm path have landed** (ADR-0092, the
issue-#493 series). A run seals its sources, builds per-Composer-package
artifacts — symbol shards, declared contracts, per-file trace IR,
per-declaration own-rows and per-file walk blocks — and publishes them
atomically; the next run reuses every package and every file whose content
fingerprint is unmoved, decodes a lowered tree only where a walk reaches
it, walks only the files an edit could reach, and answers everything else
from persisted diagnostics. Fold results persist as one generation-level
table keyed by engine identity (§4, over ADR-0066's replay seam).
`project_index` shards **per package** rather than per symbol — ADR-0092
§3 replaced ADR-0009's interning plan — with every global table recomputed
per generation from the shards, because PHP's autoloading is not a module
system and a symbol added in one package can move a name's answer in
another.

What is not done, and what it costs:

- **The last scale criterion is unmet.** On the ten pinned corpus packages
  (6,670 files) a cold run is 7.70s and a rebuild that walks nothing is
  1.41s, which straight-lines to roughly 6s at the ~30k-file scale
  ROADMAP M5 names, over its ≤2s target. Everything an edit reaches is
  proportional to the edit; what is left — capture, and the merge and
  fixpoint work that survives — scales with the universe.
- **Nothing prunes old generations** (#529). A publish never removes what
  it replaced, so a store grows by roughly one artifact set per
  invocation — five edits to one file of nikic/PHP-Parser leave five
  generations and 26 MB against 1.26 MB of source. Artifact sharing does
  not save it: sharing needs an *unchanged* package, and the ordinary
  first-party shape is one package. `doctor` makes the size visible; the
  bound does not exist yet, and sweeping is entangled with the same
  concurrency question #491 has to answer.
- **The analysis itself is single-threaded.** Parallelism was re-scoped by
  measurement (#490) from the generation build to `check_units`' per-file
  loop, which is where the remaining walk cost is; the walk threads
  `&mut dyn Folder` and `&mut Vec<Diagnostic>` throughout, so the
  conversion is one sidecar per worker and per-file diagnostic sinks
  merged at the end.
- **The affected set is sound-conservative, and two of its legs are
  coarse.** A file is walked when it changed, when its footprint meets a
  name whose resolution could have moved, when it reaches a changed file
  within the descent depth, or when a whole-universe verdict moved. The
  call-graph leg over-approximates where a walk's answer is not available,
  so a core-file edit still walks more than an edit to a leaf (14 files of
  341 on nikic/PHP-Parser, against 2 for a leaf); tightening further is
  gated on measurement, not appetite.
- **Artifacts cost about 4.3x the source they describe** after the binary
  codec (#504 took them from 13.7x). The residue is the lowering genuinely
  being larger than its text — spans, resolved names, per-node vectors —
  rather than encoding overhead, so the next lever is per-package string
  dedup, not another codec.
- **The perf harness exists** (`cargo xtask perf`, with `--warm`,
  `--edits` and `--paranoid`) and carries the warm ≡ cold oracle. Its
  `--paranoid` mode walks every file and grades each would-be skip against
  the fresh walk — the instrument that earns every tightening of the
  affected set. Its limit is worth stating: it proves the answer, not the
  reasoning, so a missed dependency whose findings happen to agree passes.
- **Whole-corpus numbers taken before 2026-08-26 are suspect.** Until
  #524, the file walk followed symlinks out of the analyzed tree and the
  CLI and the harness disagreed about how far — the harness saw 220,110
  files where there were 6,670 — which invalidated one measurement badly
  enough to need a public retraction (#523). One collector now serves
  both, a directory link is followed only into the named roots, and
  `doctor` names what it skipped.

## Deliberate refusals

Not gaps. Recorded here so a reader does not file them as such: numeric
strictness levels, worst-case `maybe`-reporting, message-regex suppression,
benevolent-union semantics, a call-site template *solver*, a
`TypeCombinator`/`TypeUtils` layer, lint and format rules, Rector integration,
tool-specific docblock tags beyond `@phpstan-*`/`@psalm-*`, `init`, and a
PHP-version emulation matrix. Each is anchored in an ADR; see
[overview.md](overview.md) and `docs/ROADMAP.md`'s "Won't build".

The template refusal is worth stating precisely, because part of what a solver
buys is now delivered by other means. What Steins **does not** do: generate
constraints from every occurrence of a template variable, unify them, or
propagate a solution back through the signature — no unification, no fixpoint,
no reverse flow into an argument. What it **does** do (ADR-0032's 2026-08-15
amendments, issues #362/#363): a single positional *read* of the generics carry
a receiver or an argument already holds, one level, top-level positions only,
all-or-nothing across *every* occurrence of a name (an occurrence the read
cannot perform contests it, rather than being skipped) — and always underneath
the body summary, which wins wherever it speaks. That is tier 1's "T is whatever
flowed in" made legible at the call site, not a second inference engine beside
it.
