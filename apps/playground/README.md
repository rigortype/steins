# Steins Playground

The browser playground (ADR-0065): a static page that runs the Steins analysis
core **fully in-browser** as a wasm module in a Web Worker. There is no backend
— the spec follows the rigor-playground frontend (rigor ADR-29) in its end
state, where the engine is a cached static asset.

By default every run here is the documented **sound subset** (ADR-0004): no PHP
sidecar, so findings that require executing PHP are omitted and nothing false is
added. A builtin's return type then comes from the catalog's mined declaration
(ADR-0069), rendered `(asserted)` — a claim, never a runtime answer. The banner at the bottom of the page is the analysis envelope's `notice`
field — the same sentence `steins check --no-php` prints — rendered as data.
With the optional engine loaded that sentence is no longer the posture, so the
banner states what the engine did and points at the boundary panel; the
envelope's `notice` text itself is untouched, because the UI decides what to
show and not what is true.

## The optional PHP engine (issue #64)

The page can also load **php-wasm** — php-src compiled to WebAssembly — and let
the analysis ask the real engine its questions. It is off until you click *Load
PHP engine*, and off means literally today's page: the plain `sw_check` /
`sw_annotate` pair, nothing fetched, nothing changed.

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

Only a **converged** run is rendered. The iteration cap (32), the per-batch
deadline (10 s, enforced by terminating the PHP worker — a PHP busy loop starves
its own thread's timers, so no in-thread timeout exists), or any engine failure
falls back to the plain call for that run and says so in the engine bar. A
half-converged envelope is a partial lie and is never shown.

### What lights up, and what does not

php-wasm 0.1.0 is PHP **8.5.2 built 32-bit** (`PHP_INT_SIZE = 4`), and that
machine — not the version — decides the boundary. On it:

| lane | state | why |
| --- | --- | --- |
| folded values | **44 of 57** allowlisted builtins | the verified portable subset (ADR-0066 amendments), under an argument range guard of ±(2³¹−1) counting array keys — the page derives the live counts from the catalog |
| the refused names (`abs`, `intval`, `sprintf`, `version_compare`, `range`, `preg_split`, the `dec*`/`bindec`/`hexdec` family) | refused | ten of them produce or render an integer in the machine's word; `sprintf("%x", -1)` is `"ffffffff"` here and `"ffffffffffffffff"` on a 64-bit runtime, `version_compare` compares numeric runs through a C `long`, and `range("3000000000", …)` yields floats here where a 64-bit engine yields ints. `preg_split` is the odd one out: it is refused for this build's PCRE, which has no JIT and so honours the inline `(*LIMIT_MATCH=…)` verbs a JIT-enabled build ignores |
| reflected return envelopes | live | a declared return type is platform-independent |
| the absence family | live | existence is not arithmetic |
| curated refinement rows | declined | a curated row is verified against the 64-bit engine at the pinned minor; `strlen()` is `int` here, not `int<0, max>` |

None of that is written into the frontend. Every replay envelope carries a
`boot` object — the engine surface **as the shared fold policy sees it**,
computed from the same helpers that gate admission — and the engine bar plus its
*Precision boundary* panel are composed from it. A gate change moves the page in
the same commit; a 64-bit engine answering instead would make the same code say
"all 22" and name no refusals. That is issue #61's second half: the boundary is
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
- `php-worker.js` — the PHP thread; owns the php-wasm instance. Hand-written
  because php-wasm 0.1.0 ships no working worker entrypoint.
- `php-dispatch.mjs` — boot the sidecar runner inside php-wasm and answer one
  request key with it. Shared by `php-worker.js` and `smoke-replay.mjs`.
- `replay.mjs` — the fixpoint driver, transport-agnostic. Shared by the
  frontend and the smoke, so the loop the suite pins is the loop that ships.
- `steins.mjs` — the dependency-free JS half of the `steins-wasm` C ABI.
- `smoke.mjs` — the Node smoke over the built module (canned answers); the CI
  gate before any artifact upload.
- `smoke-replay.mjs` — the Node end-to-end over the REAL engine: the vendored
  php-wasm, driven through `replay.mjs`.
- `build.sh` — builds `steins-wasm`, and vendors php-wasm at its pin plus the
  sidecar runner into `vendor/`.
- `vendor-licenses/` — the license texts the vendored engine obliges (tracked;
  `build.sh` fails if the packaged texts drift from them).

## Local development

```sh
./apps/playground/build.sh
cd apps/playground && python3 -m http.server 8642
# open http://127.0.0.1:8642/
```

Any static file server works; nothing else runs. `build.sh` is idempotent and
re-runnable: it skips the php-wasm download when the pinned version is already
vendored. `steins_wasm.wasm` and `vendor/` are build products and gitignored —
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
margin and being absent without the engine, `abs(-3)` widening because the name is
refused on this build's integer width, the `boot` object matching the engine that actually booted, the
absence family lighting up with the engine and silent without it, and `env` being
asked once and never again.

## Attribution

The optional engine runs PHP via [php-wasm](https://github.com/seanmorris/php-wasm)
by Sean Morris, Apache-2.0 (`vendor-licenses/php-wasm-LICENSE`,
`vendor-licenses/php-wasm-NOTICE`). Vendoring its `.wasm` redistributes PHP in
binary form: this product includes PHP software, freely available from
<https://www.php.net/software/> (`vendor-licenses/PHP-LICENSE-3.01`). The page
carries the same attribution, and none of it belongs in
`THIRD-PARTY-LICENSES.md`, which is generated from the cargo graph only.
