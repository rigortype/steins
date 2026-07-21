# Mago parser spike report (ADR-0003)

Date: 2026-07-22. Harness: [`src/main.rs`](src/main.rs) against
`mago-syntax` 1.44.0 (path deps into the local Mago checkout, rustc 1.97.1 —
**MSRV 1.97 forced a toolchain update**). Corpus: 4,433 real vendor PHP files
from consult-rector's `vendor/` (rector, phpstan, symfony, doctrine, mcp-sdk,
…).

## Results

### Test B — real-corpus parsing: PASS

- **4,433/4,433 files parse without a single error** (100.00%).
- Throughput 27.2 MB/s single-threaded, arena-allocated, including our
  full-tree walks; parsing alone is faster.

### Test A — structural losslessness: PASS *for splice editing*, with a caveat

Generic child-walk reconstruction (leaf-node spans + trivia spans) tiles only
2/4,433 files. Both anomaly classes are fully explained and neither is data
loss:

- **Gaps = punctuation.** `(`, `)`, `{`, `;`, `,` are stored as `Span`
  *fields* on their parent structs (`left_parenthesis: Span`,
  `terminator: Terminator`), not as child nodes — `children()` never yields
  them. Every byte IS accounted for in some field, but the tree is **not
  uniformly traversable**: reconstructing text from the tree alone requires
  per-struct field knowledge.
- **Overlaps = empty containers.** An empty `array(\n)` / `{ }` block is a
  childless leaf whose span includes interior whitespace that is also
  recorded as trivia. Spans themselves are accurate.

**Consequence for the Steins syntax contract:** rewriting must be
**span+splice based** (compute text edits from accurate node spans; splice
into retained `source_text`; unchanged regions stay byte-identical by
construction) — not tree-rendering based. Mago's own formatter reprints from
scratch, so Mago never exercises format-preserving printing; we would be its
first consumer of that style, but the data needed is all there.

### Test C — error tolerance: MIXED

300 clean files, four mutation classes:

| Mutation | Errors reported | Silent accept | Node survival |
|---|---|---|---|
| truncate at 80% | 99.3% | 0.7% | **16.5%** |
| delete final `}` | 100% | 0% | **18.7%** |
| garbage at midpoint | 50.0% | 50.0%* | ~100% |
| delete a mid-file `;` | 98.3% | 1.7% | **99.0%** |

\* Explained: a byte-midpoint frequently lands inside a string literal,
comment, or docblock, where arbitrary bytes are *legal* — correct behavior,
not missed detection. (Our first garbage string `@#...` was itself legal PHP:
error-suppression + hash comment. Measurement bug, fixed.)

- **Local damage (the realistic LSP mid-typing case) recovers excellently**:
  a deleted semicolon reports an error and keeps 99% of the tree.
- **Unclosed-delimiter damage recovers coarsely**: truncation or a missing
  closing brace drops the *entire enclosing declaration* (~17–19% survival —
  vendor files are mostly one class, and the class node vanishes). The
  parser never panics and always returns a `Program`, but a file is
  near-treeless during the transient "just typed `{`" state. This is the
  concrete substance behind Pzoom's recovery complaint.

## Verdict: ADOPT, with four conditions

1. **Contract style**: Steins' syntax-tree contract is span+splice, wrapping
   mago's tree; no dependence on uniform tree traversal for printing.
2. **Pin a fork** (as Pzoom does — `muglug/mago-clone`): Mago releases
   near-daily and has shipped parser-API migrations; MSRV moves with it.
3. **Last-good-tree policy in the LSP layer**: during unclosed-delimiter
   transients, analysis queries keep consuming the previous valid parse
   (natural under salsa — the parse query simply doesn't advance on a
   collapsed tree beyond a quality threshold).
4. **Recovery improvements are upstreamable**: finer-grained recovery for
   unclosed braces (retaining a partial class with an error marker) is a
   contribution Mago itself would benefit from — aligned with our
   parody-and-flow-back stance (ADR-0016).

Incremental reparse is absent in mago-syntax; acceptable because salsa's
granularity is the per-file parse query and 27 MB/s makes whole-file reparse
cheap at edit time.
