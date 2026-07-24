# The Syntax Tree Contract

**Status: implemented** (`steins-syntax`; ADR-0003).

## The rule

> The pinned Mago fork is a dependency of `steins-syntax` **only**, and **no
> Mago type appears in this crate's public API.**

Everything the analyzer sees is the owned, lowered representation defined in
this crate: `SourceTree` and its plain-data structs. This is the seam that lets
a parser backend be replaced without touching an analysis crate — and the reason
Mago is described as an *adopted backend*, never as "the parser".

Mago is the existing Rust PHP toolchain whose parser was spike-verified and
adopted behind this contract. It is not the contract owner.

## What is lowered

`SourceTree::parse(text)` produces an owned tree carrying:

- `declare(strict_types=1)` per file, and the namespace / `use`-import context
  (`NsCtx`) at each offset — the input to PHP name resolution.
- **Function declarations** (`FunctionDecl`): FQN, parameters with native types
  and defaults, return type, docblock text and span, effect envelope, effect
  origins, throw origins, and a `conditional` flag.
- **Class-likes** (`ClassDecl`): classes, interfaces, enums, traits, with
  `extends`/`implements` refs, methods, properties, constants, enum cases and
  backing type, `uses_traits`, and the same docblock/conditional facts.
- **Methods and properties** with visibility, `static`/`final`/`abstract`,
  `readonly`, promotion, and return-bound keywords (`self`/`static`/`parent`).
- **Analysis scopes** (`Scope`) carrying the [trace IR](trace-ir.md).
- **Reference sites** (`NameRef`) tagged with how the name was written —
  fully-qualified, qualified, or unqualified — which is the syntactic input the
  resolution rules key on. Resolution itself lives in `steins-infer`.
- **Dynamism sites** (`eval`, `include`/`require` with a classified path,
  `class_alias`) for the [dam](../type-specification/dynamism.md).

The lowering is deliberately *demand-grown*: it started at exactly what the
first check needed, and each construct was added when a check required it. What
the tree does not model is visible as an explicit unknown in the trace IR rather
than as a silent gap.

## Spans and positions

`Span { start, end }` is a byte-offset range, `end`-exclusive.
`SourceTree::position(offset)` resolves it to a 1-based `Position { line,
column }`.

Byte offsets, not line/column, are the primary currency. Everything downstream —
diagnostics, the annotate margin, the transform engine, the baseline hash's line
lookup — derives from them.

## Span+splice editing

The rewriting model the contract guarantees (ADR-0003, ADR-0034): **text edits
are computed from accurate node spans and spliced into the retained source
bytes.** Unchanged regions stay byte-identical by construction — not by a
formatter's best effort.

This was chosen because Mago's tree is data-lossless but not uniformly
traversable, which makes format-preserving *printing* (re-rendering the tree)
the harder and riskier approach. Splicing sidesteps it: Steins never re-renders
a file it did not change.

The consequence for the transform engine: an `EditPlan` is a set of
non-overlapping `(file, span, replacement)` triples, and applying it is a byte
operation. See [transform-engine.md](transform-engine.md).

## Comments and docblocks

Docblock text is carried on declarations along with its span, so the PHPDoc
parser runs on the raw text and the transform engine can rewrite the block in
place. Raw comment trivia is also available for the inline-suppression scanner,
which needs to distinguish a comment **trailing code on a line** from one
**alone on its own line** (`SourceTree::is_line_leading`).

## Error tolerance

The contract is for a lossless, error-tolerant CST. A file that does not parse
cleanly still yields a tree; constructs the lowering cannot represent become
explicit unknowns rather than parse failures. No input panics.

## Not implemented

- **Incremental re-parse.** `parse` is a memoized salsa query, so an unchanged
  file is not re-parsed, but a changed file is re-parsed whole.
- **Position-indexed lookup structures.** ADR-0048 decided *replay over
  retention* for position queries: nothing position-indexed is retained today,
  by design. See [query-graph.md](query-graph.md).
- **A second parser backend.** The seam exists; nothing has been swapped through
  it.
