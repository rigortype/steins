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

## Amendment (2026-08-01): the allowlist grows by twenty-four names (issue #78)

The S1.5 amendment above verified a width-safe subset of a **22-name** allowlist.
This is that allowlist's own growth path walked once: every candidate = an ADR-0008
purity/determinism argument + a 32/64-bit differential probe verdict + a
`WIDTH_SAFE`/`WIDTH_REFUSED` row. **No mechanism moved.** The fold lane, the range
guard, the replay loop, the `#73` declared-return floor and the boot object picked
the names up because the tables are what they read.

### The same instrument, one more round

**351 further adversarial `(name, args)` tuples** (running total **661**), every one
passing the range guard, through the **same** `steins_handle` dispatch core on both
machines — 64-bit `php` 8.5.8 over the runner's NDJSON protocol, 32-bit php-wasm
0.1.0 (PHP 8.5.2, `PHP_INT_SIZE = 4`) over the §5 patched prologue. Both builds
again report `precision = 14`, `serialize_precision = -1`, `memory_limit = 256M`.

New probe families this round: engine-*minted* binary strings (`base64_decode("gA==")`,
`urldecode("%80")` — the fold wire is JSON, so a raw `0x80` cannot travel as an
argument, and the interesting case is where the callee produces one), out-of-alphabet
string arithmetic, both `strtr` arities including array-key ordering, array *subjects*
that make a scalar-shaped builtin return an array, and the base-conversion pair in
both directions.

One probe is **deliberately absent** and says so: `str_pad("abc", "3000000000")`. Its
`int` parameter is a target *width*, so unlike `str_repeat`'s count it cannot be
neutralised with an empty subject — on the 64-bit engine it is a three-gigabyte
allocation, a PHP fatal, and the resident runner dies mid-NDJSON taking the rest of the
run with it. The identical coercion path is probed from the negative side
(`str_pad("abc", "-3000000000")`), which produces the same `TypeError`-vs-answer
decline at zero bytes.

### The candidate disposition (21 probed, 18 admitted safe, 1 width-refused, 2 declined)

| name | verdict | probes (silent/reverse/decline) | one-line reason |
| --- | --- | --- | --- |
| `ucwords` | safe | 14 (0/0/0) | byte transform; ASCII-only since PHP 8.2's locale-independent case conversion, delimiters form included |
| `strtr` | safe | 20 (0/0/0) | both arities; the 2-arg array form is longest-key-first by PHP's own rule, order-independent on both engines |
| `preg_quote` | safe | 12 (0/0/0) | escaping table is a constant of the build, not of the word size |
| `addslashes` | safe | 10 (0/0/0) | as `preg_quote` |
| `urlencode` | safe | 12 (0/0/0) | percent-encoding is a byte table |
| `urldecode` | safe | 12 (0/0/0) | as `urlencode`; `%80` mints a non-UTF-8 string and the runner widens it identically on both |
| `rawurlencode` | safe | 11 (0/0/0) | as `urlencode` |
| `rawurldecode` | safe | 11 (0/0/0) | as `urldecode` |
| `base64_encode` | safe | 10 (0/0/0) | fixed alphabet |
| `base64_decode` | safe | 15 (0/0/0) | including the strict second argument: `base64_decode("!!!", true)` is `false` on both, `""` non-strict on both |
| `str_increment` | safe | 18 (0/0/0) | 8.3+, present on both builds; the digits live in the string — `str_increment("9223372036854775807")` is `"9223372036854775808"` on each |
| `str_decrement` | safe | 17 (0/0/0) | as `str_increment`; `ValueError` on `'0'`, `'a'`, `'A'`, empty and non-alphanumeric, identically |
| `str_pad` | safe | 16 (0/0/1) | an in-range target width clamps against the subject identically; the decline is `TypeError` on an oversized numeric-string width |
| `substr_replace` | safe | 17 (0/0/2) | scalar subject; in-range offset/length clamp identically, the 2 declines are `TypeError` on oversized numeric-string offsets |
| `str_starts_with` | safe | 10 (0/0/0) | returns a bool from a byte comparison |
| `str_contains` | safe | 10 (0/0/0) | as `str_starts_with` |
| `str_ends_with` | safe | 10 (0/0/0) | as `str_starts_with` |
| `gettype` | safe | 12 (0/0/0) | one word from a fixed vocabulary |
| `version_compare` | **WIDTH-REFUSED** | 19 (6/0/0) | php-src compares each numeric run of a canonicalized version through a C `long`, so two oversized runs both saturate and compare **equal** on 32-bit: `version_compare("2147483647","2147483648")` = `-1` / `0` |
| `strcmp` | **NOT ADMITTED** | 18 (0/0/0) | zero width divergence — the refusal is ADR-0008's, not the gate's: see below |
| `strcasecmp` | **NOT ADMITTED** | 18 (0/0/0) | as `strcmp` |

`version_compare` is the round's surprise and the reason the instrument exists. It
reads as pure string work, its documented return is `-1|0|1` (or a bool), and its
arguments are **strings** — so the range guard has no integer to reject and could
never have caught it. Only the differential did:

```
version_compare("2147483647", "2147483648")   64: -1   32: 0
version_compare("3000000000", "4000000000")   64: -1   32: 0
version_compare("1.3000000000","1.4000000000") 64: -1  32: 0
version_compare("9223372036854775807","9223372036854775806") 64: 1  32: 0
```

The three-argument (bool) form runs the same comparison, so it is refused with the
two-argument form rather than split.

### `strcmp`/`strcasecmp`: a refusal that is not a width verdict

Both names probed **clean** — 36 tuples, zero silent, zero reverse, zero decline, the
two engines agreeing to the byte. They are still not admitted, and the distinction
matters enough to write down: a `WIDTH_REFUSED` row is still `foldable`, so a name that
fails ADR-0008's determinism bar cannot be parked there. It has to be absent from both
tables.

The bar it fails: PHP's contract for these functions is the **sign**. The value is
`memcmp`'s, which C leaves implementation-defined, and both builds pass it straight
through — `strcmp("A", "a")` is `-32`, `strcmp("zzz", "a")` is `25`, `strcasecmp("ß",
"SS")` is `80`. Folding would pin a literal the language does not promise, on a
quantity two agreeing samples do not make portable. A sign-normalised admission was
considered and rejected: it would have the catalog report `-1` where the engine
returns `-32`, which is forking semantics — the one thing the fold seam must never do,
since its entire premise (ADR-0004) is that the engine is the oracle. Declining costs
nothing a two-literal `strcmp` call was going to buy.

### The must-not list, with its evidence

Also probed, so the exclusions cite something. Two of these are width rows; the rest
are ADR-0008 refusals and therefore **off the allowlist entirely**, pinned absent by
`steins_catalog`'s `impure_and_locale_sensitive_are_excluded`:

| name | disposition | evidence |
| --- | --- | --- |
| `dechex` | **WIDTH-REFUSED** | `dechex(-1)` = `"ffffffffffffffff"` / `"ffffffff"`; `dechex(-2147483647)` = `"ffffffff80000001"` / `"80000001"` — an **in-range** argument suffices |
| `decbin` | **WIDTH-REFUSED** | `decbin(-1)` = 64 ones / 32 ones |
| `decoct` | **WIDTH-REFUSED** | `decoct(-1)` = `"1777777777777777777777"` / `"37777777777"` |
| `bindec` | **WIDTH-REFUSED** | the **type tag** flips: `bindec("11111111111111111111111111111111")` = `int(4294967295)` / `float(4294967295)` |
| `hexdec` | **WIDTH-REFUSED** | `hexdec("FFFFFFFF")` = `int` / `float`; `hexdec("FFFFFFFFF")` = `int(68719476735)` / `float(68719476735)` |
| `strtotime` | off the allowlist | `nondet.time`, and timezone-coupled: `strtotime("2020-01-01")` = `1577804400` / `1577836800`, the engines' timezone offset exactly |
| `date` | off the allowlist | already colored `nondet.time`; `date("Y-m-d")` = `"2026-08-01"` / `"2026-07-31"` between the two clocks |
| `idate` | off the allowlist | timezone-coupled **even with an explicit timestamp**: `idate("Y", 0)` is `1970` under `UTC` and `1969` under `Pacific/Kiritimati` |
| `mb_*` | off the allowlist | encoding-coupled (`mbstring.internal_encoding`), and settled a second way: php-wasm 0.1.0 has **no mbstring**, so all 11 `mb_*` probes answered `widen: unknown function` there |
| `number_format` | off the allowlist | held out with the `mb_*` family by the issue. Recorded honestly: 5 probes, no width divergence, and the historical locale coupling of float rendering is **gone** at `PINNED_PHP` (`de_DE.UTF-8` and `C` render `number_format(1234.5678, 2)` identically; `precision` does not move it). It stays out on the conservative side and may be admitted later on its own evidence, not smuggled in on this slice's |
| `bin2hex` | off the allowlist | carries a standing refused row in the ADR-0056 return-fact table (the empty-in/empty-out trap, `return_facts.toml`). That row is about a different table and is **not relitigated**; the width probe found no divergence and the name simply does not enter here |

### The 46-name disposition, and what moved with it

`WIDTH_SAFE` is now **37**, `WIDTH_REFUSED` **9**, the allowlist **46**. The counts in
the playground's engine bar and boundary panel came from
`width_safe_names()`/`width_refused_names()` before this slice and still do — the page
went from "19/22" to "37/46" and named six more refusals without a line of JS
changing, which is the property issue #64 S3 built the boot object for. What did need
editing is the places that *pin* the old numbers on purpose, and only those:
`steins-catalog`'s partition test, `steins-wasm`'s boot-object test, and the two
playground smokes.

### Consequences

- Two of the new rows are load-bearing beyond their own fold. `str_contains` and its
  siblings fold to real booleans, which the narrowing lane can act on where a declared
  `bool` envelope cannot; and `strtr`'s 2-argument form lit up the array-literal seam
  (issue #39) without the seam changing, exactly as `in_array`/`count`/`implode` did.
- `version_compare` folding on the CLI and declining in the browser is the first
  width-refused row a reader is likely to *meet* — `abs`/`intval`/`sprintf` look like
  integer functions, and this one does not. The boundary panel names it.
- The 32-bit half of every verdict is testable only through the replay table
  (`replay_fold.rs`) and the real php-wasm smoke; the machine that runs the Rust suite
  is 64-bit, and the probe harness — not the test suite — is where the differential
  lives. That is unchanged from S1.5 and remains the standing cost of the arrangement.

## Amendment (2026-08-15): the five deferred fold names are measured (issue #354)

ADR-0028's 2026-08-14 amendment opened the seam to array *results* and shipped two
waves behind it, deferring five names it had already argued were admissible:
`array_unique` (37 corpus uses), `range` (24), `preg_split` (20), `str_split` (14),
`array_fill` (8). The deferral was pinned, not implicit — `steins_catalog`'s partition
test asserted `width_class(name) == None` for exactly those five. This slice moves
that pin, and no mechanism moves with it: the fold gate, the range guard, the replay
loop and the boot object pick the names up because the tables are what they read.

### The same instrument, sharpened twice

**209 adversarial `(name, args)` tuples**, every one passing the range guard, through
the **same** `steins_handle` dispatch core on both machines — 64-bit `php` 8.5.9 over
the runner's NDJSON protocol, 32-bit php-wasm 0.1.0 (PHP 8.5.2, `PHP_INT_SIZE = 4`)
over the §5 patched prologue. Both builds report `precision = 14`,
`serialize_precision = -1`, `pcre.backtrack_limit = 1000000`,
`pcre.recursion_limit = 100000`. They differ in their PCRE: **10.47 with
`pcre.jit = 1`** against **10.44 with no JIT**. That difference turns out to decide a
row.

Two properties of the harness had to change before any of it counted, and both are
mistakes that produce a *false clean*:

1. **Compare the response bytes, not parsed JSON.** Array elements cross the seam
   bare — `steins_encode_array` carries no per-element type tag — so an `int` on one
   engine and a `float` on the other are distinguished only by
   `JSON_PRESERVE_ZERO_FRACTION`'s `3000000000` versus `3000000000.0`, which any JSON
   parse erases. Every earlier round compared scalars, where the envelope's own `type`
   field carries the tag; the array results ADR-0028 admitted are the first place this
   could hide, and the first probe run under the byte comparison found a divergence
   the parsed comparison had reported clean.
2. **A float argument cannot be written as a JavaScript number.** `3000000000.0`
   round-trips through `JSON.stringify` as `3000000000` and reaches the runner as an
   **int** — an argument the range guard refuses, so the tuple is not a probe at all.
   Float arguments travel as raw JSON tokens, and the harness now applies
   `fold_arg_fits_i32`'s rule itself and refuses to count an inadmissible tuple. Ten
   tuples were caught this way.

`str_replace` and `substr_replace`'s array forms — the wave-0 rows ADR-0028 admitted
with no re-verification, on the argument that the array form is identical on both
engines — were re-probed **bytewise** for the same reason. Seven tuples, unchanged.

### The five-name disposition (209 probed, 3 admitted safe, 2 refused)

| name | verdict | probes (silent/reverse/decline) | one-line reason |
| --- | --- | --- | --- |
| `range` | **WIDTH-REFUSED** | 52 (7/0/0) | its bounds are declared `string|int|float`, so the engine's own width types a numeric string and the **element type tag** flips inside the result: `range("3000000000", "3000000000")` is `[int(3000000000)]` / `[float(3000000000.0)]` |
| `preg_split` | **WIDTH-REFUSED** | 64 (5/0/2) | the two PCRE builds disagree, and it is the **JIT**, not the version: `preg_split('/(*LIMIT_MATCH=1)a/', "aaa")` splits on the JIT-enabled build and is `false` on the interpreter |
| `array_unique` | safe | 39 (0/0/1) | compares string casts under `SORT_STRING` without retyping what it keeps; the one decline is the runner refusing an argument whose next-int key has nowhere to go at `PHP_INT_MAX` |
| `str_split` | safe | 24 (0/0/3) | `int` `$length` in, strings and `0..n` keys out; the three declines are `TypeError` on an oversized numeric-string or float length |
| `array_fill` | safe | 30 (0/0/4) | `int` parameters that never coerce a *value*; the four declines are the narrow engine having no key after its own `PHP_INT_MAX`, and a `TypeError` on an oversized `$start_index` |

The dividing line is one sentence: **`range` is the only one of the five with a
`string|int|float` parameter**, and a numeric string on such a parameter is typed by
the machine, not by the argument. `str_split`, `array_fill` and `preg_split` take
plain `int` parameters, where the same oversized argument is a `TypeError` on the
narrow engine — a decline, which is sound. 52 probes found no int-argument and no
float-argument route to `range`'s flip; it is reachable from strings only, and the
flip starts exactly one past the narrow `PHP_INT_MAX` (`"2147483647"` agrees,
`"2147483648"` does not).

### `preg_split`: a refused row whose divergence is not the width

This is the first row in either table refused for something other than the integer
machine, and the honest thing is to say so in the row rather than let the class name
imply otherwise. The witness family is the inline limit verbs, which PCRE2's
interpreter honours and its JIT does not:

| probe | 64-bit (PCRE 10.47, JIT on) | 32-bit (PCRE 10.44, no JIT) |
| --- | --- | --- |
| `preg_split('/(*LIMIT_MATCH=1)a/', "aaa")` | `['', '', '', '']` | `false` |
| `preg_split('/(*LIMIT_MATCH=1),/', "a,b")` | `['a', 'b']` | `false` |
| `preg_split('/(*LIMIT_RECURSION=1)(?:a)+/', "aaa")` | `['', '']` | `false` |
| `preg_split('/(*LIMIT_DEPTH=1)(?:a)+/', "aaa")` | `['', '']` | `false` |
| `preg_split('/(*LIMIT_MATCH=1)(*NO_JIT)a/', "aaa")` | `false` | `false` |

The last row is what identifies the cause: disabling the JIT in the pattern makes the
two engines agree. Everything else probed clean across 59 further tuples — Unicode
property and script classes, `\R`, `\X`, lookbehind, recursion, catastrophic
backtracking at seven lengths, UTF-8 mode on multibyte subjects, `PREG_SPLIT_*` flags
including the integer offsets of `OFFSET_CAPTURE` — so this is not a claim that the
two PCREs are broadly different. It is one recorded divergence, which is what a
refused row needs.

**The `preg_refusal_memo` question, decided.** ADR-0078's pattern lane asks the engine
whether a literal pattern compiles and memoizes the refusal. A folded `preg_split`
does not ride that memo, is not gated by it, and is not refused for it:

- **In the browser** the two seams no longer meet, because the refusal above keeps
  `preg_split` from folding there at all. That is the whole of the collision issue
  #354 asked about, and it is closed by the width verdict rather than by new wiring.
- **On a native run** both seams ride the *project's own* PCRE, which is the only
  engine whose answer is right for the project's own runtime (ADR-0004). They answer
  different questions: `preg_split('/[/', $s)` genuinely **is** `false` on that engine,
  which is a value fact, while the lane's diagnostic is about the pattern being
  broken. Gating the fold on the memo would make the value seam re-report the pattern
  seam's finding as an absence, which is the "two seams answering the same question"
  the issue warned against — in the other direction.

### `array_unique` and `precision`: admitted, with the escalation named

`array_unique`'s default `SORT_STRING` compares string casts, and a float's cast is
`precision`-dependent, so the ini decides **how many elements survive**. Measured, not
assumed: at `precision = 14` `array_unique([0.1, 0.1000000000000001])` keeps one
element, at 17 it keeps two.

That is the same ini `strval` and `implode` have folded under since the first round —
`strval(0.1)` is `'0.1'` at 14 and `'0.10000000000000001'` at 17 — so refusing
`array_unique` for it while those two fold would set the bar in two places at once.
It is admitted, and what is new is written down rather than smoothed over: the
exposure moves from *how a float is spelled* to *how long the array is*. Both engines
report `precision = 14`, which is the condition every float-rendering row here already
carries. Closing the seam properly is ADR-0008's opt-in pseudo-constant configuration,
and it is a decision about `strval`, `implode` and `array_unique` together.

### `array_fill` and the result budget, end to end

`array_fill(0, 1000000, 'x')` is the legitimate call with an illegitimate reply, and
the first name on the allowlist where a single integer literal reaches it — `explode`
needs a 257-piece string. The runner charges the 256-entry budget **after** the call
and **before** encoding, so the reply is declined, never truncated, and the dump falls
to a type. Pinned at both levels: `steins-sidecar/tests/protocol.rs` for the runner's
`'array result over entry budget'`, and
`an_over_budget_array_fill_widens_rather_than_truncating` for the analyzer, with 256
folding whole beside 257 declining.

### The 53-name disposition, and what moved with it

`WIDTH_SAFE` is now **40**, `WIDTH_REFUSED` **11**, `WIDTH_UNVERIFIED` unchanged at
**2**, the allowlist **53**. Unlike wave 1, this slice moves the boundary in both
directions at once: the browser folds three names it did not, and names two more
refusals it did not. The pages that report those counts derive them, so only the
places that *pin* the numbers on purpose were edited — `steins-catalog`'s partition
test, `steins-wasm`'s boot-object test, and the two playground smokes.

`WIDTH_UNVERIFIED` did not grow, which is the class working as defined: a probed name
lands in the class its evidence chooses and never passes through the one that claims
nothing. Its two rows, `array_merge` and `explode`, still have zero probes behind
them, which remains the correct number until someone runs a probe **set** at them.

### Consequences

- The byte-comparison correction is the durable part. Any future width verdict on an
  array-returning name is only as good as a comparison that can see element type tags,
  and the parsed-JSON comparison silently cannot. It is worth re-reading that
  paragraph before the next round rather than rediscovering it.
- `preg_split` establishes that a `WIDTH_REFUSED` row can be refused for a build
  option rather than the word size. The class's mechanism — folds on a provably
  64-bit engine, declines elsewhere — fits that case exactly, and the row carries its
  own reason, so nothing is lost by not inventing a fourth class for it.
- Three of the five now fold in the browser, which is the first time the width gate
  and the array-result path meet on a narrow machine;
  `the_issue_354_verdicts_split_the_lane_on_a_32_bit_engine` is where that is pinned.

## Amendment (2026-08-15): the alias rows (`join`, `chop`, `sizeof`, `doubleval`)

The issue #354 coverage survey — 238 builtins taken from the extensions in
phpstan-src's `src/Type/Php` that declare support for one, each measured against
what Steins answers today (`docs/notes/20260815-phpstan-type-php-coverage.md`) —
turned up four names that are **PHP's own second spellings** of names already on
this list: `join`/`implode`, `chop`/`rtrim`, `sizeof`/`count`,
`doubleval`/`floatval`. One C handler, two names. `foldable` matches a spelling,
so all four widened.

### Why they are listed rather than resolved

An alias table (`alias(name) -> Option<&str>`, consulted by `width_class`) was
the obvious shape and is deliberately not what landed. A row on this list claims
a width, and the discipline is that a claim is earned by probing; resolving one
name's verdict onto another would make the *first* alias table entry a claim
nothing measured. Four literal rows cost four lines and keep every row's
evidence its own. The table becomes worth building when the alias count is
large enough that transcription is the greater risk — a threshold four names do
not meet.

### The evidence

Each alias was probed with **its target's own recorded probe family**, run
against the alias spelling, on both machines. 45 tuples. Two independent claims
come out of the same run, because each tuple produced four replies
(`{target, alias} × {64-bit, 32-bit}`):

- **the pairing** — `alias@64 == target@64` and `alias@32 == target@32`,
  byte-identical. **Zero breaks in 45 tuples.**
- **the width** — `alias@64 == alias@32`, the ordinary `WIDTH_SAFE` claim,
  earned directly on the alias spelling rather than inherited.

| alias | target | probes (silent/reverse/decline) | the target's recorded row |
| --- | --- | --- | --- |
| `join` | `implode` | 13 (0/0/1) | 13 (0/0/1) |
| `chop` | `rtrim` | 8 (0/0/0) | 8 (0/0/0) |
| `sizeof` | `count` | 9 (0/0/1) | 9 (0/0/1) |
| `doubleval` | `floatval` | 15 (0/0/0) | 15 (0/0/0) |

The counts reproduce the targets' rows exactly, including both declines (the
unassignable next-int key `implode` and `count` both refuse). That is what "one
handler" predicts, and it is the reason these four needed no probe *design*:
the family was already written.

### No fifth pair

Every internal function's arginfo was compared against the 53 allowlisted names,
and the twins were read by hand. Signature identity alone is a weak filter —
`string $string: string` matches 27 functions — but it is a complete *upper*
bound, and the only true aliases among them are the four above. Three further
pairs alias names that are **not** admitted (`key_exists`/`array_key_exists`,
`is_integer`/`is_long`/`is_int`, `is_double`/`is_float`); they enter with their
targets or not at all, which the catalog's alias test pins in the negative.

`WIDTH_SAFE` is now **44**, `WIDTH_REFUSED` **11**, `WIDTH_UNVERIFIED` **2**, the
allowlist **57**. The boundary moved in the safe direction only: the browser
folds four more names and refuses nothing new.
