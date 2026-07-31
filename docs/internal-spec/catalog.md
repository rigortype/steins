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

**Deliberate exclusions**, even where frequent:

- `mb_*` — encoding-dependent.
- anything affected by `setlocale`, the current timezone, or
  `mb_regex_encoding`-class settings — the value is not portable without
  ADR-0008's opt-in pseudo-constant configuration, which is not implemented.
- `nondet` builtins (`time`, `rand`, `microtime`) — excluded by definition.

## `effect_labels(name)` — effect coloring

Maps a builtin to its effect labels, or `None` for uncatalogued (which widens to
unknown-effect: exhaustiveness taint, no finding). A coloured entry wins;
otherwise a foldable builtin is catalogued with the **empty** effect set.

Coverage is frequency-seeded (`docs/notes/20260722-builtin-frequency.md`) plus
the gaps identified in `docs/research/phpsrc-mining/effects_gaps.md`:
randomness, time, filesystem read/write, output, header mutation, signals,
System-V IPC, global/ini state, and the composite `session_start`.

Recorded imprecisions, stated rather than hidden:

- `fopen` stays at the parent `io.fs` label — its read/write split is
  mode-string-dependent and this slice does not inspect it.
- `print_r` / `var_export` are coloured `output` even though they are pure in
  return-mode (`$return = true`); the arg-blind upper bound is the safe choice.
- `sleep` / `usleep` are `io` — an observable timing side effect, closest to the
  `io` root among the initial labels.
- `exit` / `die` are **language constructs**, not functions; they never reach
  this table and are detected structurally.

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

Membership today is one family — `PDO::query`/`exec`/`prepare` and
`PDOStatement::execute`/`fetch`/`fetchAll`, all `io.db` (issue #67). That is the
first producer of a label the registry had carried since ADR-0018 with nothing to
emit it. `prepare` takes the same coarse colour as the rest: whether it is a
round trip to the server depends on PDO's emulated-prepares setting, which is
runtime configuration the catalog cannot read, so the row takes the upper bound.

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
transitively. A name absent from the table is an unknown external →
`Unknown`, never `No`.

**Builtin enums are deliberately omitted**: the mining data for their implicit
interfaces and backing is incomplete, and an incomplete row would produce a
wrong `No`.

## `builtin_exception_parent(name)` / `builtin_throws(name)`

The standard SPL/engine exception tree, keyed by global simple name
(case-insensitive, no namespace). Project classes chain into it through their
`extends` once their own chain leaves the project index. A name absent here and
not a project class has an **unknown** parent — the caller keeps the chain result
at `Maybe`, never `No` (ADR-0040's FP-safe side).

`builtin_throws` gives the throw classes a builtin can raise.

## `invocation_shape(name)` — higher-order builtins

The callback parameter index, immediate-vs-deferred invocation, and the
callback's argument source. The table and its irregularities are documented in
[`closures.md`](../type-specification/closures.md). A function absent from the
table is not treated as a higher-order invoker — its callback argument stays an
opaque taint.

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

## `declared_envelope(name)` / `declared_envelope_changed_at(name)` — the Asserted return floor

**Consumed** (ADR-0069, issue #73). 919 rows of `(lowercased builtin name,
canonical envelope spelling)` — `"bool"`, `"int"`, `"float"`, `"string"` and their
`?T` forms — generated by `cargo xtask gen-catalog` from
`docs/research/phpstan-mining/declared_envelopes.toml`. That TOML is itself
generated, by `cargo xtask mine-function-map`, and is the source of record.

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

**Version discipline** is A11-shaped, via `declared_envelope_changed_at`: the
functionMap delta files are the change oracle, and a name whose declared return type
moved at minor *m* is admitted only for a project target lying wholly at or above
*m*. An undeclared target admits (the row is Asserted anyway). The two tables are
**disjoint at this pin** — every version-sensitive name returns an array or a list,
so none carries an admitted envelope — and the gate is wired anyway, because a later
pin can change that.

What is **deliberately not here**, counted rather than hidden (see the TOML's
`[counts]`): 6,658 `Class::method` rows, 388 shaped arrays/lists and 1,119 multi-base
`T|false` unions. Those are the rows where functionMap genuinely exceeds what
reflection can say, and ADR-0069 §5 hands them to a contracts-grade slice that would
seed through the full `lower_str` lowering rather than `envelope_fact`. Also absent
by construction: 33 rows the pinned engine's own declaration escaped (excluded and
listed verbatim in the TOML) and 2,099 names the pinned engine does not know as
functions.

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
- **Flag inspection** — `json_decode`/`json_encode` throw `JsonException` only
  under `JSON_THROW_ON_ERROR`; without flag inspection those rows stay
  uncatalogued (widen) rather than manufacture a throw. The keys are present in
  the source, awaiting the machinery.
- **Plugin-registered ecosystem labels and signatures** (ADR-0012, ADR-0039).
