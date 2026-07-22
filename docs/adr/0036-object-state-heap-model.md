# Object state: store-based heap, escape sets, readonly immunity

Property facts are keyed by **allocation identity, not variable**: env maps
variables to `ObjRef(allocation-id)`; a heap maps ids to
`{ class, props: {name → Fact}, escaped }`. Aliasing (`$b = $a`) shares the
id, so writes through any alias are visible through all — PHP's handle
semantics becomes the design's input rather than its enemy.

- **Escape sets bound the sweeps**: an object escapes when passed to an
  unresolved call, stored into an unknown structure, or returned. Unknown
  calls sweep the props of **escaped ids only** — a purely local object's
  facts survive (the Rigor fact-bucket discipline, narrowed further by
  escape). Stage-1 conservatism: passing anywhere escapes; descent-time
  precision for resolved callees comes later.
- **`$this` is a pre-escaped id**: `$this->p` facts survive own
  private/final method calls (descend), and are swept by any overridable
  call on `$this` — the dispatch guard rules reused.
- **`readonly` is sweep-immune**: constructor-established readonly facts
  persist through escapes and unknown calls — the language guarantees the
  immutability, so the analyzer honors it permanently. Deliberate product
  stance: adopting readonly is rewarded with precision (the modernization
  incentive).
- **New proof-layer checks**: `type.property-mismatch` (native property
  types are runtime-enforced; strict/coercive per the assigning file) and
  `readonly.reassigned` (a proven second write on one path).
  Uninitialized-typed-property reads are an explicit TODO (needs
  initialization analysis). `@var` on properties joins the phpdoc contract
  family as `phpdoc.property-mismatch`.
- Out of stage 1: objects inside arrays, static properties (global-state
  family), descent-time escape precision.
