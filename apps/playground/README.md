# Steins Playground

The browser playground (ADR-0065): a static page that runs the Steins analysis
core **fully in-browser** as a wasm module in a Web Worker. There is no backend
— the spec follows the rigor-playground frontend (rigor ADR-29) in its end
state, where the engine is a cached static asset.

By default every run here is the documented **sound subset** (ADR-0004): no PHP
sidecar, so findings that require executing PHP are omitted and nothing false is
added. The banner at the bottom of the page is the analysis envelope's `notice`
field — the same sentence `steins check --no-php` prints — rendered as data.

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

What lights up is **reflection and existence**: builtin return envelopes and the
absence family (`call.undefined-function` and friends), plus the boot-surface
label. Not folded values — php-wasm 0.1.0's PHP 8.5 is a 32-bit build, and a
fold is only sound on a provably 64-bit engine (ADR-0066 §4).

## Files

- `index.html` — the whole frontend: CodeMirror 6 (from esm.sh via an import
  map, so the module graph shares one instance of each package), a 600 ms
  debounced check, wavy underlines + a findings panel, the posture banner, and
  the engine toggle + replay orchestration.
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
once the table is complete) against a canned table captured from a real `php`.
`smoke-replay.mjs` pins the loop against php-src itself: that it converges, that
the absence family lights up with the engine and is silent without it, and that
`env` is asked once and never again.

## Attribution

The optional engine runs PHP via [php-wasm](https://github.com/seanmorris/php-wasm)
by Sean Morris, Apache-2.0 (`vendor-licenses/php-wasm-LICENSE`,
`vendor-licenses/php-wasm-NOTICE`). Vendoring its `.wasm` redistributes PHP in
binary form: this product includes PHP software, freely available from
<https://www.php.net/software/> (`vendor-licenses/PHP-LICENSE-3.01`). The page
carries the same attribution, and none of it belongs in
`THIRD-PARTY-LICENSES.md`, which is generated from the cargo graph only.
