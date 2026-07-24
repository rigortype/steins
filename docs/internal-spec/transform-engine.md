# The Transform Engine

**Status: implemented** for `EditPlan`, the refusal taxonomy, the completeness
oracle, obstacles and the vouch valve, the region model (slice A), and two
transforms. ADR-0010, ADR-0034, ADR-0037, ADR-0041, ADR-0046, ADR-0047.

## What a transform is

A **standalone semantic rewrite whose preconditions are spelled in types and
effects** — not a pattern with a replacement. It is the conceptual heir of
Rector-style codemods with the essential difference stated in ADR-0034: the
precondition is *proven against the inference engine*.

The headline example: promoting a `@param int $n` docblock to a native `int $n`
declaration requires proving that **every call site in the project flows an
`int`**. That precondition is structurally unavailable to a modular tool, which
never looks at call sites when analyzing a function.

## `EditPlan`

An atomic transaction (ADR-0034 point 1):

```rust
ByteSpan { start: u32, end: u32 }        // end-exclusive; serializable mirror of Span
Edit     { path, span, replacement }     // delete = empty replacement; insert = zero-width span
NewFile  { path, contents }
EditPlan { edits: Vec<Edit>, new_files: Vec<NewFile> }
```

Built on [span+splice](syntax-tree-contract.md): untouched regions stay
byte-identical *by construction*. Overlapping edits are rejected at **planning**
time, as an error — never a panic. Adjacency is not overlap: two edits may meet
at a point.

The plan is JSON-serializable because it is the **currency of the dry-run → diff
→ approve → apply loop**, and that loop will run over MCP when M7 lands.

## Refusals

The Certainty discipline applied to rewriting: a site that cannot be proven is
**refused with a named reason**, never silently skipped.

```rust
SiteRef { path, line, column, label }
Refusal { site, reason, detail }
```

`reason` is a stable machine-readable name; `detail` is the human sentence an
agent reads and can act on. The taxonomy in use today includes:

| Reason | Meaning |
| --- | --- |
| `argument-not-proven` | a call site's argument value is not proven |
| `return-not-proven` | a return value is not proven |
| `no-observed-callers` | nothing calls it — promotion would be unfalsifiable |
| `dynamic-call-present` | a dynamic call could reach the target |
| `eval-present` / `dynamic-include-present` | a universe-havoc obstacle stands |
| `resolution-ambiguous` | the FQN has more than one definition |
| `named-or-spread-args` | an argument form the binder cannot account for |
| `magic-method` / `method-inheritance` | the class world cannot bound the callers |
| `function-referenced-as-value` | the function escapes as a callable |
| `type-not-natively-representable` | no native syntax spells the proven type |
| `type-not-renderable` | no faithful phpdoc spelling exists |
| `native-contradicts-proven` | the existing native type disagrees with the evidence |
| `phpdoc-finer-than-native` | promoting would *lose* information |
| `default-not-admitted-by-native` / `implicit-nullable-default` | the parameter default would break under the new type |

## The completeness oracle

```rust
CompletenessOracle { enumerated, transformed, refused }
is_complete() == (transformed + refused == enumerated)
```

Every enumerated candidate site is accounted for as transformed **or** refused.
A mismatch is an internal invariant violation — a bug in the transform, not a
user-facing state. This is what makes a claim like "23,148 candidates
enumerated, 0 transformed" meaningful rather than alarming: nothing was dropped
silently.

## Dual verification

ADR-0034's safety net, wired in by the CLI:

1. **Post-check** — after `--apply`, the project must produce **zero new
   diagnostics**. `--apply` is gated on it.
2. **Oracle** — every site transformed or refused.

Dry-run is the default. `--apply` is explicit.

## Dynamism obstacles and the vouch valve

A **project-global caller-enumeration obstacle** (ADR-0046 §2) is a dynamic-code
construct — `eval`, or a dynamic/out-of-universe `include`/`require` — that makes
"all callers proven" unknowable for *every* candidate. It is recorded **once per
run** with the full offending-site list (text output caps the display; JSON
carries them all), and while an unvouched obstacle stands, **every candidate
refuses**.

The vouch valve (`[transform.vouch]` in [config.md](config.md)) lets a user
declare that a specific site does not mint the names in question. A run that
vouched anything **downgrades its completeness claim loudly**:

```text
DOWNGRADE: completeness claim is conditional on 1 user-vouched dynamic-code exemption(s):
    vouched src/Legacy/Loader.php:88:1: eval
```

In JSON the same downgrade appears as a prominent top-level note beside the
`obstacles` and `vouched_exemptions` arrays. A vouch matching no obstacle is
reported as a no-op rather than silently ignored.

## Regions

`PartitionMap` (ADR-0047 slice A) is a **pure function of config and file path**:
given declared partition and observer path-sets, it answers which region a
file's declaring scope belongs to (`Partition(name)`, `Shared { vendor }`,
`Observer`). Assignment precedence and glob syntax are in
[config.md](config.md).

Slice A threads the map through to the planners; **no planner decides on it
yet**. With one region the planner is byte-identical to whole-universe behavior.
Slices B–E are the recorded precision axis: the prediction to be judged against
measurement is 3,000–4,000 additional unlocked sites (ADR-0047 §8).

## The two shipped transforms

**`phpdoc-to-native`** — promotion (ADR-0034 point 4, ADR-0037). Turns a
docblock-only type into a runtime-enforced native declaration when every call
site provably flows it. Landed through method scope (ADR-0043 stage 5) with the
full refusal taxonomy.

**`phpdoc-honesty`** — the inverse (ADR-0037 point 4, ADR-0041 point 4). Widens
a *lying* `@param`/`@return` to the proven truth from call-site and return
evidence. Where promotion tightens code toward the runtime, honesty repair makes
the documentation stop lying about it.

Measured whole-universe closing run: **23,148 / 509 candidates enumerated, 0
transformed** — dynamic dispatch is the sound floor, and partitioning is the
recorded way past it.

## Not implemented

- **New transform kinds** — DTO promotion (array-shape sprawl → class), stringly
  → enum. Queued for M7 (ADR-0034).
- **Fold- and dataflow-backed transform proofs.** v1's dominance argument is
  literal-only (`argument-not-proven`, ADR-0041 §1).
- **Effect-precondition transforms** (loop → map requires purity). The effect
  system exists; no transform consumes it.
- **`steins mcp`** — the dry-run → diff → approve → apply loop over MCP, with
  `EditPlan` as the wire currency (ADR-0010, roadmap M7).
- **Fix-its** — a transform attached to a diagnostic as a payload (`check
  --fix`).
- **Partitioning slices B–E** and the checker-side region scoping (ADR-0047 §9).
