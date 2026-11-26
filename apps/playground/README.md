# Steins Playground

The browser playground (ADR-0065): a static page that runs the Steins analysis
core **fully in-browser** as a wasm module in a Web Worker. There is no backend
— the spec follows the rigor-playground frontend (rigor ADR-29) in its end
state, where the engine is a cached static asset.

While the engine loads — and whenever it is toggled off or fails — every run
here is the documented **sound subset** (ADR-0004): no PHP sidecar, so findings
that require executing PHP are omitted and nothing false is added. A builtin's
return type then comes from the catalog's mined declaration (ADR-0069),
rendered `(asserted)` — a claim, never a runtime answer. The banner at the
bottom of the page is the analysis envelope's `notice` field — the same
sentence `steins check --no-php` prints — rendered as data. With the engine
live that sentence is no longer the posture, so the banner states what the
engine did and points at the boundary panel; the envelope's `notice` text
itself is untouched, because the UI decides what to show and not what is true.

## The PHP engine (issue #64)

The page loads php-src compiled to WebAssembly by
[php-wasm-builder](https://github.com/lyrixx/php-wasm-builder) and lets the
analysis ask the real engine its questions. It loads at page boot by default:
the plain (sound-subset) result paints first, the ~4.5 MB download proceeds
alongside it, and the first converged run repaints. The *PHP engine* button
toggles it off, and off means the plain `sw_check` / `sw_annotate` pair,
nothing asked of the engine.

On, an analysis runs the ADR-0066 **replay loop**:

```
table = {}                          # session-global, monotone
loop:
  envelope = checkReplay(source, profile, table)   # + annotateReplay
  if envelope.pending is empty: render
  else: table += phpWorker.answer(envelope.pending); repeat
```

A pending key *is* the request — `{"method":…,"params":…}` — and is handed to
the PHP worker verbatim; the answer stored under it is the raw JSON-RPC `result`
from `steins_handle`, the same dispatch function the native sidecar embeds
(`crates/steins-sidecar/runner.php`, copied into `vendor/`, never forked).
The runner runs as the native child does: a batch goes in as NDJSON on stdin
and comes back one response line per request. Each batch starts from fresh PHP
state, and a fatal ends the run where the native child would die: that request
is a failure, and the ones after it are asked again.

Only a **converged** run is rendered. The iteration cap (32), the per-batch
deadline (10 s, enforced by terminating the PHP worker — a PHP busy loop starves
its own thread's timers, so no in-thread timeout exists), or any engine failure
falls back to the plain call for that run and says so in the engine bar. A
half-converged envelope is a partial lie and is never shown.

### What lights up, and what does not

The engine is PHP **8.5.13 built 32-bit** (`PHP_INT_SIZE = 4`: the builder
targets wasm32, where a C `long` is 32 bits), and that machine, not the
version, decides the boundary. On it:

| lane | state | why |
| --- | --- | --- |
| folded values | **53 of 68** allowlisted builtins | the verified portable subset (ADR-0066 amendments), under an argument range guard of ±(2³¹−1) counting array keys — the page derives the live counts from the catalog |
| the refused names (`abs`, `intval`, `sprintf`, `version_compare`, `range`, `json_decode`, `json_encode`, `preg_split`, `preg_match`, `preg_match_all`, the `dec*`/`bindec`/`hexdec` family) | refused | twelve of them produce or render an integer in the machine's word; `sprintf("%x", -1)` is `"ffffffff"` here and `"ffffffffffffffff"` on a 64-bit runtime, `version_compare` compares numeric runs through a C `long`, `range("3000000000", …)` and `json_decode("3000000000")` yield floats here where a 64-bit engine yields ints, and `json_encode` turns the same numeric string into `3000000000.0` here under `JSON_NUMERIC_CHECK\|JSON_PRESERVE_ZERO_FRACTION`. `preg_split`, `preg_match` and `preg_match_all` are the odd ones out: all three are refused for this build's PCRE, which has no JIT and so honours the inline `(*LIMIT_MATCH=…)` verbs a JIT-enabled build ignores |
| reflected return envelopes | live | a declared return type is platform-independent |
| the absence family | live | existence is not arithmetic |
| curated refinement rows | declined | a curated row is verified against the 64-bit engine at the pinned minor; `strlen()` is `int` here, not `int<0, max>` |

None of that is written into the frontend. Every replay envelope carries a
`boot` object — the engine surface **as the shared fold policy sees it**,
computed from the same helpers that gate admission — and the engine bar plus its
*Precision boundary* panel are composed from it. A gate change moves the page in
the same commit; a 64-bit engine answering instead would make the same code say
"all 68" and name no refusals. That is issue #61's second half: the boundary is
legible, and it cannot go stale in the safe-but-illegible direction the way the
sound-subset banner did once an engine was actually present.

## Files

- `index.html` — the whole frontend: CodeMirror 6 (from esm.sh via an import
  map, so the module graph shares one instance of each package), a 600 ms
  debounced check, wavy underlines + a findings panel, the posture banner, the
  engine toggle + replay orchestration, and the *Precision boundary* panel
  composed from the envelope's `boot` object.
- `worker.js` — the analysis thread; owns the wasm instance. Runs the plain
  pair, or one replay iteration when the request carries a table.
- `php-worker.js` — the PHP thread; owns the PHP module, and replaces it when
  a run aborts it. Its own rather than the builder's Worker helper, because the
  main thread terminates it on a deadline.
- `php-dispatch.mjs` — install the sidecar runner in the PHP module and answer
  a batch of request keys with it. Shared by `php-worker.js` and
  `smoke-replay.mjs`.
- `replay.mjs` — the fixpoint driver, transport-agnostic. Shared by the
  frontend and the smoke, so the loop the suite pins is the loop that ships.
- `steins.mjs` — the dependency-free JS half of the `steins-wasm` C ABI.
- `smoke.mjs` — the Node smoke over the built module (canned answers); the CI
  gate before any artifact upload.
- `smoke-replay.mjs` — the Node end-to-end over the REAL engine: the node
  build of the same engine, driven through `replay.mjs`.
- `build.sh` — builds `steins-wasm`, builds the engine with php-wasm-builder
  at its pinned commit, and copies it plus the sidecar runner into `vendor/`.
- `vendor-licenses/` — the license texts the engine obliges (tracked; `build.sh`
  fails if the builder's LICENSE or a library pin they were taken at moves).

## The engine build

php-wasm-builder publishes no binaries, so `build.sh` builds one: it checks out
the builder at an exact commit and runs its `docker buildx bake`, which pins
Emscripten and every library and verifies php-src's commit. That needs Docker
with buildx; a cold build takes a few minutes, and CI caches the result on
`build.sh` and `vendor-licenses/`.

Two build arguments are choices, and `build.sh` says why:

- **The extensions.** What the engine defines is what the absence family calls
  defined, so a missing extension turns every call into it into
  `call.undefined-function`. The build carries every extension whose linked
  libraries are php-src's own or carry a notice tracked here (libxml2 for the
  XML family, Oniguruma for mbstring, SQLite, zlib). iconv (LGPL), openssl,
  gd, zip, sodium and intl are left out, so a call into them is still reported
  as undefined.
- **The memory.** The module's memory is fixed at link time. `runner.php` sets
  `memory_limit = 256M` so that a fold's outcome is a property of the code and
  not of the host. The heap is 320 MB because below about 300 MB the heap, not
  that limit, would end a large fold.

## Local development

```sh
./apps/playground/build.sh
cd apps/playground && python3 -m http.server 8642
# open http://127.0.0.1:8642/
```

Any static file server works; nothing else runs. `build.sh` is idempotent and
re-runnable: it skips the engine build when the stamp in `vendor/php-wasm/`
matches the pinned commit, version, memory and extensions. `steins_wasm.wasm` and `vendor/` are build products and gitignored —
deployment (issue #58, deferred) fetches published artifacts instead.

## Smoke tests

Both run in Node over the built module. The first needs no engine; the second
needs `vendor/` populated, so run `build.sh` first.

```sh
cargo build -p steins-wasm --target wasm32-unknown-unknown --release
node apps/playground/smoke.mjs

./apps/playground/build.sh
node apps/playground/smoke-replay.mjs
```

`smoke.mjs` pins the ABI (the pending contract, the key format, a fold landing
once the table is complete, and the `boot` object of a 64-bit engine) against a
canned table captured from a real `php`. `smoke-replay.mjs` pins the loop against
php-src itself: the flagship `dumpType(greet(2, "World"))` inlining to
`'Hello, World! Hello, World! '`, issue #61's own two table rows folding in the
margin and being absent without the engine, `abs("3000000000")` widening because
the name is refused on this build's integer width while `abs(-3)` still answers
`3` on the transfer seam, the `boot` object matching the engine that actually
booted, the absence family lighting up with the engine and silent without it,
`env` being asked once and never again, a fatal mid-batch costing only its own
request, and calls into the bundled extensions not being reported undefined.
CI's `wasm` job runs both.

## Attribution

The optional engine is built by
[php-wasm-builder](https://github.com/lyrixx/php-wasm-builder), MIT
(`vendor-licenses/php-wasm-builder-LICENSE`), with Emscripten
(`vendor-licenses/emscripten-LICENSE`, and `musl-COPYRIGHT` for its libc).
It links libxml2 (`libxml2-Copyright`), Oniguruma (`oniguruma-COPYING`), zlib
(`zlib-LICENSE`) and SQLite, which is in the public domain. Shipping its
`.wasm` redistributes PHP in binary form: this product includes PHP software, freely available from
<https://www.php.net/software/> (`vendor-licenses/PHP-LICENSE-3.01`). The page
carries the same attribution, and none of it belongs in
`THIRD-PARTY-LICENSES.md`, which is generated from the cargo graph only.
