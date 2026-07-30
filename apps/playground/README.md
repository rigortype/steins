# Steins Playground

The browser playground (ADR-0065): a static page that runs the Steins analysis
core **fully in-browser** as a wasm module in a Web Worker. There is no backend
— the spec follows the rigor-playground frontend (rigor ADR-29) in its end
state, where the engine is a cached static asset.

Every run here is the documented **sound subset** (ADR-0004): a browser has no
PHP sidecar, so findings that require executing PHP are omitted and nothing
false is added. The banner at the bottom of the page is the analysis
envelope's `notice` field — the same sentence `steins check --no-php` prints —
rendered as data.

The module also exposes a **replay** pair (ADR-0066, issue #64) for a caller
that *can* run PHP — php-wasm, once the frontend loads it. `checkReplay` /
`annotateReplay` take a table of already-known fold/reflect/env answers and
return the envelope plus `pending`: the requests the run could not answer.
Answer them, put them back under the same key strings, call again, stop when
`pending` is empty. A non-empty `pending` means the results are degraded to the
sound subset and **must not be rendered**. The frontend half of that loop is
not wired up yet; `smoke.mjs` drives it over a canned table.

## Files

- `index.html` — the whole frontend: CodeMirror 6 (from esm.sh via an import
  map, so the module graph shares one instance of each package), a 600 ms
  debounced check, wavy underlines + a findings panel, and the posture banner.
- `worker.js` — the analysis thread; owns the wasm instance.
- `steins.mjs` — the dependency-free JS half of the `steins-wasm` C ABI.
- `smoke.mjs` — the Node smoke suite over the built module; the CI gate before
  any artifact upload.
- `build.sh` — builds `steins-wasm` for `wasm32-unknown-unknown` and copies
  the module beside `index.html` for local development.

## Local development

```sh
./apps/playground/build.sh
cd apps/playground && python3 -m http.server 8642
# open http://127.0.0.1:8642/
```

Any static file server works; nothing else runs. The wasm file itself is a
build product and gitignored — deployment (issue #58, deferred) fetches a
published artifact instead.

## Smoke test

```sh
cargo build -p steins-wasm --target wasm32-unknown-unknown --release
node apps/playground/smoke.mjs
```
