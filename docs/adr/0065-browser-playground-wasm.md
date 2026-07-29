# Browser playground: the analysis core as a static wasm asset, no backend

Owner directive (2026-07-30): implement the browser playground, following the
spec of the rigor-playground app (rigor ADR-29) in its **end state** — the
engine runs fully in-browser, the wasm module is served as a static asset, and
there is no origin server. Deployment (issue #58) is deliberately deferred;
this ADR covers the module, its ABI, its posture, and the frontend contract.
Status: PENDING ratification (post-hoc mode).

## 1. Context

rigor reached in-browser execution the hard way: a Ruby interpreter compiled to
wasm, C-extension link work, and a 15 MB+ artifact, gated for a year behind its
WD6 conditions. Steins is Rust, so the equivalent question — *does the analysis
core compile to wasm at all?* — was answered by a build spike in minutes, and
the answer shapes everything else here:

- `steins-infer` (with salsa 0.28, the pinned Mago fork, and every analysis
  crate under it) compiles to `wasm32-unknown-unknown` **unmodified**.
- `rayon` enters the graph through `mago-database` but is never exercised on
  the single-file parse path — proven by the smoke suite running 50+ checks on
  one instance in a plain wasm VM, where a single thread-spawn would trap.
- The artifact is **1.77 MB raw, 0.43 MB brotli** (unoptimized `--release`,
  no `wasm-opt` pass yet) — a static file well under every CDN's concern, the
  try.ruby-lang.org hosting model rigor's re-evaluation recorded, an order of
  magnitude smaller.

## 2. Decision: the sound subset is the playground's posture, by construction

The browser has no PHP, and Steins' value-precision comes from executing the
project's own PHP (ADR-0004). The playground therefore runs the documented
**sound subset** — the exact `--no-php` posture: findings that require
executing PHP are omitted, nothing false is added, and the degradation is
announced rather than hidden.

Three mechanisms make that true by construction rather than by convention:

1. **The sidecar dependency is target-gated out**, not merely unused:
   `steins-infer`'s `steins-sidecar` dependency moved to
   `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, and
   `SidecarFolder` plus its helpers are `#[cfg]`-gated with it. The wasm
   dependency graph contains no `std::process` — a wasm build cannot even
   *try* to spawn. The only folder `steins-wasm` can construct is `NoFold`.
2. **The notice travels as data.** On the CLI the sound-subset notice goes to
   stderr; a wasm module has no stderr a user reads. Every envelope carries a
   `notice` field with `SOUND_SUBSET_NOTICE` verbatim, and the frontend
   renders it as a persistent banner. The CLI's stderr behavior is untouched.
3. **No filesystem, no config, no baseline.** One snippet is one single-file
   project (`ProjectLayout::fallback()`); the pipeline is the CLI's
   (ADR-0050 §6) minus the channels that have no referent for a pasted
   snippet: vendor filtering, `[[policy]]`, and the baseline. Inline
   `@steins-ignore` **is** applied, including `suppress.unmatched` anti-rot —
   a snippet demonstrating suppression behaves like the real tool.

## 3. Decision: a hand-rolled C ABI, not wasm-bindgen

`steins-wasm` exposes six exports (`sw_alloc`, `sw_dealloc`, `sw_check`,
`sw_annotate`, `sw_result_ptr`, `sw_result_len`): UTF-8 in wasm memory, a JSON
envelope out. The JS glue is ~60 dependency-free lines the playground owns
(`apps/playground/steins.mjs`), runs unchanged in a browser, a Worker, or Node,
and instantiates with a plain `WebAssembly.instantiate` — no generated glue, no
import object at all.

Why not wasm-bindgen: it would add a code generator and a crate family to the
dependency graph (and `THIRD-PARTY-LICENSES.md`) to do string-in/string-out,
which is the one case the raw ABI handles trivially. The cost of this choice is
manual pointer discipline at exactly two places (write source, read result),
both inside the glue file, both covered by the smoke suite. If the surface ever
grows past strings-and-JSON, revisit; for `check`/`annotate` it is the smaller
system.

JSON, not a binary format, because the envelope **is** the CLI's `--format
json` schema: `findings` carries the same keys (`id`, `layer`, `level`,
`path`, `line`, `column`, `message`, facet key when declared), so a playground
reader and a CI reader learn one schema. Additions are additive only:
`ok`/`error` (the exit-2 config error as data — an unknown profile name must
not trap), `notice` (§2), and `parse_errors`.

## 4. Decision: parse errors are reported, not swallowed

`SourceTree::parse_errors()` has no consumer on the CLI check path — the
PHPStan cross-check recorded that as a real gap (a file `php -l` rejects is
analyzed via recovery, silently). The playground does not inherit the
silence: the envelope's `parse_errors` array reports every recovered error
with its line. This is deliberately **ahead of** the CLI: a playground whose
first audience is someone pasting an incomplete snippet cannot afford the
quiet-recovery reading, and the field costs nothing. Closing the same gap on
the CLI stays its own decision (it changes a shipped surface); this ADR does
not make it.

## 5. The frontend contract (issues #55–#57)

A single static `index.html` under `apps/playground/`, CodeMirror 6 from
esm.sh at runtime, the wasm module in a **Web Worker** (a check must never
block typing), a 600 ms debounce, wavy underlines plus an expandable findings
panel, the §2 banner, a profile selector over the rung ladder
(`default ⊂ contracts ⊂ strict`, ADR-0062 A-G10) with a seeded sample whose
findings appear as the rung climbs, an annotate overlay toggle, and
dark/light/auto themes. Profile resolution is **shared, not duplicated**: the
`Surface` engine moved from `steins-cli` to `steins_infer::profile` (the
no-second-relation discipline applied to surface selection), and the CLI
re-exports it unchanged.

No server anywhere: local development is any static file server; deployment
(#58, deferred) is a static-host push plus a tag-triggered artifact build.

## 6. What this is not

- **Not a second analyzer.** `steins-wasm` contains no analysis logic — it is
  an envelope around the same `check_project_with_runtime`/`annotate_project`
  entries the CLI calls, and its native tests pin the envelope, not the
  analysis.
- **Not a WASI port.** `wasm32-unknown-unknown` suffices because the module
  needs no filesystem, clock, or environment; WASI would buy nothing and cost
  an import surface.
- **Not the doctor/transform surface.** `check` and `annotate` are the
  playground; `transform` writes files and `doctor` reports an environment,
  neither of which a browser snippet has.

## 7. Consequences

- Every future dependency of the analysis crates must compile for
  `wasm32-unknown-unknown` or be target-gated the way the sidecar now is; the
  smoke suite (`apps/playground/smoke.mjs`) is the regression gate.
- The `0.1.x` findings-change warning applies to the playground identically —
  it ships whatever the module it loads shipped. The artifact is versioned
  with the workspace (one `[workspace.package] version`).
- The known-gap list gains one asymmetry: the playground reports parse errors,
  the CLI does not yet (§4).
