# The fold surface reaches the browser by request replay, over a transport the policy does not know

Issue #64 brings the real engine to the browser: php-wasm is php-src compiled
to WebAssembly, so the fold is still answered by php-src and only the
compilation target changed (ADR-0004 intact). This ADR records the Rust-side
shape that makes that possible — the S1 slice — and the two soundness gates the
php-wasm spike forced into it. Status: PENDING ratification.

## 1. Context: a synchronous walk and an asynchronous engine

`Folder::fold` is called mid-walk and returns a value. php-wasm's JS API is
asynchronous. There is no way to await inside a `&mut self` method of a
synchronous analysis, and ADR-0065 deliberately left the analysis core free of
any async runtime.

Three shapes were available. Two are rejected in issue #64 and are not
relitigated here:

- **SharedArrayBuffer + Atomics sync bridge.** Viable on Cloudflare Pages
  (`_headers` can set COOP/COEP, unlike GitHub Pages), but `COEP: require-corp`
  drags every esm.sh import-map load into CORP/CORS requirements — a moving part
  the replay loop simply does not have.
- **A Workers-side sidecar.** Script-size caps and CPU-time limits fit php-wasm
  badly, and it turns a static playground into infrastructure with cost and
  abuse surface.

## 2. Decision: a request-replay fixpoint

The analysis runs to completion **without** asking the engine anything. It
answers each fold/reflect/env question from a supplied table, and records the
questions the table could not answer. The caller answers those, adds them to the
table, and runs again.

```
table = {}
loop:
  envelope = check_replay(source, table)
  if envelope.pending is empty: done
  for key in envelope.pending: table[key] = answer(key)
```

**Termination** is by the answered set strictly growing: every iteration either
finishes or converts at least one pending request into a table entry, and the
question set a given source can generate is finite. The loop is nevertheless
**capped** on the caller's side, because a defective answerer (one that returns a
result the parser rejects, so the same key is asked again) would otherwise spin.
The cap lives in JS, not in Rust: Rust is single-shot per call, which is what
keeps the wasm side free of both a loop and a policy about how long to try.

Cost is not a concern: a snippet analysis is milliseconds, and the flagship
converges in **two** iterations (one to learn the environment, one to fold).

**Decline on miss; never fabricate.** An unanswered request returns exactly what
a dead sidecar returns — `None` for `env`/`reflect`, `Widen` for `fold`. So an
iteration with non-empty `pending` is a *NoFold-grade* run: sound, less precise,
and discarded by the loop rather than shown. This is the one property that makes
the intermediate iterations safe to compute at all. It is also why cap
exhaustion degrades to silence and never to a partial lie — the acceptance
criterion issue #64 states.

### The table key

The key is the JSON-RPC request object **minus its `id`**, serialized:

```
{"method":"fold","params":{"function":"str_repeat","args":["Hello, World! ",2]}}
```

`id` is framing, not identity — two folds of the same call differ in `id` and
ask the same question, so keying on the whole request would never hit.
Everything else *is* identity. The key is also the interchange format: it is
handed out as a pending entry and taken back as a table key, and it parses as
`{"method", "params"}`, which is everything an answerer needs. The answer stored
under it is the raw `result` object `steins_handle` returns for exactly that
method and params — the same dispatch function the native sidecar embeds, so
there is no second PHP-side semantics to keep in step.

## 3. Decision: split the fold seam into transport and policy

The replay transport is only safe if it cannot *mean* anything different from
the process transport. So the seam is split:

- **`FoldEngine`** is the transport: `env`, `reflect`, `fold`. Three questions,
  no judgment. `ProcessEngine` (native only) talks to a resident `php` child;
  `TableEngine` answers from the table and records misses.
- **`EngineFolder<E>`** is the policy: every memo, the ADR-0049 A9 monkey-patch
  veto, the issue-#28 target/runtime agreement, the ADR-0056 §2 admission
  sequence, the integer-width gate of §4 below. It is generic over the
  transport and **exists exactly once**.

`SidecarFolder` is now an alias for `EngineFolder<ProcessEngine>` and keeps its
exact public surface; `TableFolder` is `EngineFolder<TableEngine>`.

This is not tidiness. Issue #63 was a bug in precisely this seam — a second
caller reached the fold policy by a second path and silently analyzed each
project under another project's declared PHP target, and the corpus counts swung
between 536 and 483 on unchanged code for two sessions of triage. A browser
folder that carried its own copy of the gate sequence would be the same class of
defect, with no corpus gate to catch it. Sharing the policy makes drift
impossible rather than unlikely.

The wire format is split the same way and for the same reason: `steins-sidecar`
gains a `wire` module (types, `*_params` constructors, `parse_*_result` parsers)
that compiles on every target, and the `php`-spawning half moves behind
`cfg(not(target_arch = "wasm32"))`. The ADR-0065 property that no `std::process`
enters the wasm dependency graph is preserved — it is now enforced one crate
lower down, so `steins-infer`'s dependency on the sidecar can be unconditional.

### Replayability (ADR-0048)

The walk is a pure function of (CST, canonical entry state, query answers, fold
memo) with no reliance on global ordering. `TableFolder` adds nothing that could
break that: it consults no clock, no process, no filesystem, no ambient state. A
replay run is a pure function of its table, so the same source and table produce
the same findings and the same pending list, on any target.

## 4. Decision: a curated fact is pinned to a machine, not only to a version

The php-wasm spike found that **php-wasm 0.1.0 is PHP 8.5.2** — `PINNED_PHP`,
the minor every existing version gate admits — built **32-bit**:
`PHP_INT_SIZE = 4`, `PHP_INT_MAX = 2147483647`. Empirically, on that build:

| call | 64-bit | php-wasm (32-bit) |
| --- | --- | --- |
| `ip2long('255.255.255.255')` | `4294967295` | `-1` |
| `crc32('x')` | positive | negative |
| `1 << 40` | `1099511627776` | `0` |
| `hexdec('FFFFFFFFF')` | `int` | `float` |
| `abs(-2147483648)` | `int` | `float` |
| `strtotime('2040-01-01')` | a timestamp | `false` |

None of these fail. They return *silently wrong values*, which a fold would
carry straight into a proof. A minor is not a machine, so the version gate alone
is unsound on this transport.

Therefore:

1. `env` reports `PHP_INT_SIZE`; `EnvInfo::int_size` carries it, absent = unknown.
2. **The fold lane requires a provably 64-bit engine.** Unknown width declines.
   The gate is asked before the engine is dispatched to.
3. **ADR-0056 Gate 2 (curated-row admission) requires it too.** A curated row is
   verified against the 64-bit engine at the pinned minor, and a narrow engine at
   the *same* minor can violate it.
4. **Reflected envelopes and absence claims are unaffected.** A declared return
   type is a platform-independent claim and the 32-bit build reports the same
   ones; existence is not arithmetic. So a 32-bit engine still seeds envelopes,
   still witnesses absence, and simply does not fold and does not get curated
   refinements.

(2) is deliberately coarse and deliberately default-deny: `strtolower` cannot
care about `PHP_INT_MAX`, and a later slice may admit a *curated width-safe
subset* of the foldable allowlist. Until such a subset is verified against a
32-bit engine, the whole lane declines rather than guesses which builtins are
width-blind. That relaxation is tracked separately; it is not this slice.

## 5. Decision: the runner's diagnostics leave by the error log

`ini_set('display_errors', 'stderr')` is honored only by the cli and cgi SAPIs.
Under an `embed` SAPI — which is what php-wasm is — it is accepted, round-trips
through `ini_get`, and does nothing, so a notice lands mid-NDJSON on stdout and
corrupts the response line the protocol depends on. The runner now routes
through `log_errors` + `error_log = 'php://stderr'`, which works on both. The
only difference on a native run is a `PHP Warning: ` prefix on a stream the
parent discards, and the protocol fixtures are byte-identical.

## 6. The ABI

Two exports, additive; the existing four are untouched and stay NoFold, so
engine-not-loaded behavior in the browser is byte-identical to ADR-0065's.

```
sw_check_replay(src_ptr, src_len, prof_ptr, prof_len, table_ptr, table_len) -> i32
sw_annotate_replay(src_ptr, src_len, table_ptr, table_len) -> i32
```

`table` is a UTF-8 JSON object of key → raw `result` (`{}` is valid and is how a
loop starts; a malformed table takes the existing `error_envelope` path). The
envelope gains `"pending"`, **always present**, empty exactly when the run is
complete — so a caller never has to distinguish "finished" from "an older module
that does not report pending". Both replay entries and both plain entries share
one analysis body, so the pipeline pins (the profile ladder,
`warning_handler_abort = true`) cannot fork.

A fresh `TableFolder` per call, by construction: the ABI takes the table by
value and drops the folder before returning, so a stale decline can never
outlive the answer that fixes it.

## 7. Consequences

- The differential fixpoint oracle
  (`crates/steins-infer/tests/replay_fold.rs`) is the acceptance pin: a replay
  run driven to its fixpoint by a real `Sidecar` answering each pending request
  verbatim produces exactly the findings and annotations a direct
  `SidecarFolder` run produces. If a future change gives the replay path its own
  semantics, that test fails.
- `Sidecar::call_raw` exists so an answerer can hand a `(method, params)` pair
  to the engine without re-deriving it from a typed call. It is what makes the
  oracle test the same dispatch rather than a parallel one.
- The playground gains the whole sidecar-gated surface once S2 lands the JS
  loop: reflected return envelopes, the absence family, the boot-surface label.
  Version and width gates degrade exactly as designed — a browser snippet has no
  composer.json, and php-wasm's 32-bit build declines the fold lane until the
  width-safe subset exists.
- The pending list is a public interchange format. Changing the key shape breaks
  a caller's table, so it is pinned by hardcoded key strings in the `steins-wasm`
  tests and in `apps/playground/smoke.mjs`.

## Amendment (2026-07-31): the width-safe fold subset (issue #64 S1.5)

§4 declined the **whole** fold lane on any engine that is not provably 64-bit, and
said so deliberately: "a later slice may admit a *curated width-safe subset* of the
foldable allowlist… until such a subset is verified against a 32-bit engine, the
whole lane declines rather than guesses which builtins are width-blind." This is
that slice. The subset is now verified and the lane is relaxed to it. Nothing else
in §4 changes — in particular **ADR-0056 Gate 2 keeps its `int_size == 8` leg**, and
`int_size == None` still declines everything.

### The rule

A fold is admitted on a `PHP_INT_SIZE == 4` engine when **both** legs hold:

1. `steins_catalog::width_safe(name)` — the callee is on the verified subset.
2. **The argument range guard**: every integer occurring anywhere in the arguments
   lies within `[-(2^31 - 1), 2^31 - 1]`, counted recursively through array
   literals and over explicit integer **keys** as well as values.

Neither leg means anything alone: the catalog's verdict is stated *for exactly the
tuples the range guard admits*. Anything else — a refused name, an out-of-range
integer, an unreported width, a width nobody has probed — declines exactly as
before, with the same shape (`None` / `Widen`). Nothing fabricates.

The guard's lower bound is `-(2^31 - 1)` and **not** `-2^31`. `PHP_INT_MIN` on a
32-bit engine is the one integer whose magnitude that machine cannot represent, so
it is the seed of every boundary flip: `abs(-2147483648)` promotes to float there
and stays `int` on a 64-bit engine. Excluding it means no admitted integer has an
out-of-range magnitude, which makes the `abs`-shaped flip structurally unreachable
rather than merely unobserved. The cost is one value per call site.

Keys are guarded because a key is not decoration. `count([3000000000 => 'a', 'b'])`
has no out-of-range *value*, and yet the key is what PHP's next-int rule reads to
decide whether the array has one element or two.

### The classification criterion

A name is width-safe iff, for every argument tuple the range guard admits, the
32-bit engine either returns the **identical value and type tag** the 64-bit engine
returns, or **declines** (throws, or widens).

The decline clause is deliberate and it is the sound direction: a decline is
precisely what the blanket §4 gate does for every name today, so the browser loses
precision there and can never gain a wrong literal. Requiring bit-equality
*including* declines would refuse `str_repeat` — `str_repeat("", "3000000000")` is
`""` on a 64-bit engine and a `TypeError` on a 32-bit one, because an oversized
numeric string landing on an `int` parameter is coerced by the *engine's* width —
and `str_repeat` is the builtin in this issue's own flagship acceptance criterion.
The hazard §4 names is the *silent* divergence ("None of these fail. They return
silently wrong values"), and that is what the criterion excludes.

Two divergence directions are therefore refusals, and both were checked for:

- **silent** — both engines return a value and the values or their type tags
  differ. Unsound: a wrong literal enters a proof.
- **reverse** — the 32-bit engine returns a value where the 64-bit engine declines.
  Also unsound: the browser would show a value the real 64-bit runtime never
  produces. **Zero rows observed**, on any name.

### The evidence

310 adversarial `(name, args)` tuples, every one of them passing the range guard,
run through the **same** `steins_handle` dispatch core on both machines — 64-bit
`php` 8.5.8 over the runner's own NDJSON protocol, 32-bit php-wasm 0.1.0
(PHP 8.5.2, `PHP_INT_SIZE = 4`, `sapi = embed`) over the ADR §5 patched prologue.
Same dispatch function on both sides, so a difference is the machine and not a
second semantics. Both builds report `precision = 14` and
`serialize_precision = -1`, which is what makes the float-rendering names agree.

The probe families: boundary integers `±(2^31 - 1)`, oversized numeric strings
(`"3000000000"`, `"9223372036854775807"`, `"9007199254740993"`), oversized and
denormal floats, negative inputs, `-0.0`, explicit integer array keys at
`PHP_INT_MAX`, length- and position-derived results, base-converting arguments, and
every format specifier `sprintf` has for an integer.

### The 22-name disposition

| name | verdict | probes (silent/reverse/decline) | one-line reason |
| --- | --- | --- | --- |
| `strtolower` | safe | 16 (0/0/0) | string in, string out; an in-range coerced integer has one decimal spelling |
| `strtoupper` | safe | 14 (0/0/0) | as `strtolower` |
| `ucfirst` | safe | 10 (0/0/0) | as `strtolower` |
| `lcfirst` | safe | 10 (0/0/0) | as `strtolower` |
| `trim` | safe | 11 (0/0/0) | byte transform of the subject; charlist is a string |
| `ltrim` | safe | 8 (0/0/0) | as `trim` |
| `rtrim` | safe | 8 (0/0/0) | as `trim` |
| `strrev` | safe | 9 (0/0/0) | as `trim` |
| `substr` | safe | 22 (0/0/7) | int params, but an in-range offset/length clamps identically; the 7 declines are `TypeError` on an oversized numeric-string or float offset |
| `str_replace` | safe | 12 (0/0/0) | no int parameter is passed (`$count` is by-ref and absent) |
| `str_repeat` | safe | 16 (0/0/2) | the repeated bytes do not depend on the width; the 2 declines are `TypeError` on an oversized count |
| `implode` | safe | 13 (0/0/1) | each element renders under the same `precision`; the decline is an unassignable next-int key |
| `sprintf` | **REFUSED** | 19 (9/0/0) | `%b`/`%x`/`%o`/`%u` render the machine word — `sprintf("%x", -1)` is `"ffffffffffffffff"` vs `"ffffffff"` — and `%d` re-imports `intval`'s saturation |
| `strlen` | safe | 12 (0/0/0) | result bounded by the subject, which is bounded by the engine's own memory |
| `abs` | **REFUSED** | 16 (6/0/0) | the **type tag** flips: `abs("3000000000")` is `int` vs `float`; a numeric string re-enters as an integer by the engine's width, past the range guard |
| `intdiv` | safe | 17 (0/0/3) | `\|intdiv(a, b)\| <= \|a\|`, so an in-range pair yields an in-range result; the 3 declines are `TypeError`/`ArithmeticError` |
| `intval` | **REFUSED** | 17 (10/0/0) | saturation and wraparound by definition: `intval("3000000000")` is `3000000000` vs `2147483647`; `intval(4.2e9)` is `4200000000` vs `-94967296` |
| `floatval` | safe | 15 (0/0/0) | returns an IEEE double, 64-bit on both machines |
| `strval` | safe | 18 (0/0/0) | renders under the same `precision = 14`; an in-range integer has one spelling |
| `boolval` | safe | 17 (0/0/0) | returns a bool; truthiness is not arithmetic |
| `in_array` | safe | 21 (0/0/0) | returns a bool from php-src's own `zendi_smart_strcmp`, whose overflow guard makes two oversized numeric strings compare as *strings* on both machines |
| `count` | safe | 9 (0/0/1) | element count, bounded by the fold seam's 256-entry array budget; the decline is an unassignable next-int key |

19 safe, 3 refused. The three refusals are exactly the builtins whose *job* is to
render or produce an integer in the machine's own width — which is the result the
subset was supposed to isolate.

`sprintf` could in principle be sub-classified by format string (`%s` is as safe as
`strval`). That is deliberately not attempted: the safe/unsafe line would live
inside a string literal, which is the wrong place for a soundness gate.

### Why curated rows are not relaxed with it

A fold is a claim about **one argument tuple**, and a range guard can bound a tuple.
A curated return-fact row is a claim about a builtin's **whole return domain**,
verified against the 64-bit engine at `PINNED_PHP`; there is no per-call tuple to
bound it with, so there is nothing here for it to be the analogue of. `strlen` is on
the width-safe fold subset and its curated `int<0, max>` row still declines at
`int_size == 4` — that contrast is pinned by a test.

### A runner fatal found en route

Rebuilding an array literal runs PHP's own key rules, and those rules can **throw**:
`[PHP_INT_MAX => 'a', 'b']` raises "Cannot add element to the array as the next
element is already occupied". `steins_decode_args` ran outside `steins_fold`'s
`try`, so that Error escaped as an **uncaught fatal** — it took the resident runner
down mid-NDJSON and with it every later request in the run, on the native 64-bit
sidecar as much as under php-wasm. The decode now has its own catch and widens
("undecodable argument"), which is a fact about the argument rather than a result of
the folded call, and which honours the runner's standing contract that any misuse
widens.

It surfaced here because the threshold is the engine's own `PHP_INT_MAX`: at
`int_size == 4` it drops from 2^63-1, which no source realistically writes, to
2147483647 — inside the range guard, and a key a human plausibly types.

## Amendment (2026-07-31): the boot surface travels as data (issue #64 S3)

§7 promised the playground "the boot-surface label" once the JS loop landed, and
§4 promised a lane that degrades. Both arrived — and the page went on saying
`SOUND_SUBSET_NOTICE` ("no PHP sidecar — findings that require executing PHP are
omitted") with php-src running inside it. That is stale in the *safe* direction,
which is exactly the failure mode issue #61 names: a visitor cannot tell a
deliberate subset boundary from a missing feature, and now could not tell it from
a boundary that had already moved.

The fix is the one this ADR already made for the notice: **the description
travels as data, and the prose lives in the UI.**

### The `boot` object

Both replay envelopes gain a `boot` object beside `pending`:

```json
"boot": {
  "label": "PHP 8.5.2 (25 extensions)",
  "php_version": "8.5.2",
  "int_size": 4,
  "fold_lane": "width_safe_subset",
  "fold_total": 22,
  "fold_safe": 19,
  "curated_rows": false,
  "absence_family": true,
  "refused_folds": ["abs", "intval", "sprintf"]
}
```

`fold_lane` is `full` | `width_safe_subset` | `declined`. `refused_folds` is
present only on the middle lane — on the other two the lane already says the
whole story — and it is the **catalog complement** (`foldable ∧ !width_safe`),
never a second list. The plain `sw_check` / `sw_annotate` envelopes are
unchanged and carry no `boot` key at all: engine-off behaviour is byte-identical
to ADR-0065's, which is an acceptance criterion and now an assertion.

### Why it is computed by the policy and not by the frontend

`EngineFolder::surface_summary` reads the same helpers the gates read —
`boot_surface_label`, `engine_int_size`, `fold_lane_at_width`,
`curated_rows_admitted`, `absence_family_available` — and `fold_lane_at_width`
was extracted *out of* `fold_admitted_at_width` rather than written beside it, so
the description branches on the same three cases the gate does. `curated_rows`
is likewise the ADR-0056 Gate 2 predicate itself, lifted out of
`compute_builtin_return_fact`.

The alternative — a frontend that knows php-wasm is 32-bit and hardcodes
"19/22", "abs, intval, sprintf" — is the same class of defect as issue #63: a
second copy of a policy, reached by a second path, drifting silently. Here it
would drift into *claiming a boundary the analysis does not apply*, which is the
one thing a page whose whole subject is soundness cannot afford. Relaxing the
width gate, or refusing a 23rd builtin, moves the page in the same commit.

### The extra round trip, and why it is wanted

`surface_summary` is taken **before** `take_pending`, so an unanswered `env` is
recorded as a miss like any other. A snippet that asks the engine nothing —
`$a = 1;` — therefore reports `env` as pending and converges on the next
iteration with the boot object filled in. A converged run always carries a
complete description, so the UI reads one without a null check per field, and
the cost is one round trip per session (the table is session-global and
monotone).

### Consequences

- The engine bar reads `PHP 8.5.2 (php-wasm, 32-bit) — folding 19/22 width-safe
  builtins, reflection & existence live`, and a `<details>` panel lists both
  sides of the line: what php-src answered, and what widens (the three refused
  names with the `sprintf("%x", -1)` divergence, the ±2147483647 argument guard
  including array keys, curated rows off with `strlen()` as `int`).
- The seeded sample gains one line, `$greeting = str_repeat("Hello, " . "World"
  . "! ", 2);`, which has no margin fact without the engine and folds to
  `"Hello, World! Hello, World! "` with it. The rung ladder is unchanged: 1
  finding at `default`, 2 at `contracts`, 3 at `strict`, engine on or off.
- `apps/playground/smoke-replay.mjs` pins the flagship end to end over the real
  php-wasm — `dumpType(greet(2, "World"))` inlining to `'Hello, World! Hello,
  World! '` — plus issue #61's own two table rows in the margin, `abs(-3)`
  widening to `dumped type: unknown` because the name is width-refused, and the
  `boot` object agreeing with the engine's own boot probe.
