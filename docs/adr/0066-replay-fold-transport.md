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
