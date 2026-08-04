# The Transform Engine

**Status: implemented** for `EditPlan`, the refusal taxonomy, the completeness
oracle, obstacles and the vouch valve, the region model (slice A), and three
transforms. ADR-0010, ADR-0034, ADR-0037, ADR-0040, ADR-0041, ADR-0046,
ADR-0047.

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
| `escape-not-proven` | every envelope-relevant escape is Maybe — never annotated |
| `already-declared` | every proven escape is already covered (the idempotent no-op) |
| `docblock-not-round-trippable` | no lossless insertion point, or the seeded tag fails the re-parse round-trip |
| `declaration-mid-line` | the declaration head does not start its own line |

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

### Which surface the post-check measures

"Zero new diagnostics" needs a *set* of diagnostics to be zero on, and the
answer is **not uniform across transforms**. Each one names its own surface at
its call site (`PostCheckSurface` in the CLI); the asymmetry is deliberate.

| Transform | Measured against | Because |
| --- | --- | --- |
| `phpdoc-to-native` | everything, vendor-filtered — proof, mechanics **and** contract | rewriting a type is not meant to change what the docblock promises |
| `phpdoc-honesty` | same | its most plausible regression *is* a new `phpdoc.*` finding; the contract layer is the only thing that would catch it |
| `throws-envelope` | the default surface only (proof + mechanics) | its product **is** a contract, so a new contract finding is the intended effect |

The rule that separates them: measure a transform against the layer it is
*supposed* to move, and it can veto its own success. Seeding an `@throws`
envelope onto an override is exactly what gives the ancestor's narrower envelope
something to be widened against — `throw.liskov-widened` appears where there was
none, and a contract-layer post-check would refuse to write a correct seed.
Seeding a parent method does the same from the other side, giving an existing
child envelope an abstraction carrier it did not have. Neither is a regression;
both are pre-existing debt the envelope makes visible under an opt-up profile.

This is pinned by the case rather than argued for: a CLI unit test runs one
seeding plan through **both** surfaces and asserts the broad one vetoes it while
the default one passes. A second test holds the other arm — a contract finding
survives the broad surface and is invisible on the default one — so unifying the
two would fail loudly instead of silently weakening the phpdoc transforms' net.

For `throws-envelope` the remaining safety net is the proof layer: the fp-gate
discipline transposed to rewriting. Note what is *not* reachable, and so not
relied on — a seed cannot newly raise `throw.undeclared` anywhere, because the
enumeration domain includes **propagated** escapes: a caller that would gain a
declared boundary to violate is itself a candidate and is seeded in the same
run, and the write set is by construction every proven escape not already
covered.

## Dynamism obstacles and the vouch valve

A **project-global caller-enumeration obstacle** (ADR-0046 §2) is a dynamic-code
construct — `eval`, or a dynamic/out-of-universe `include`/`require` — that makes
"all callers proven" unknowable for *every* candidate. It is recorded **once per
run** with the full offending-site list (text output caps the display; JSON
carries them all), and while an unvouched obstacle stands, **every candidate
refuses**.

One asymmetry is deliberate and current: the *checker-side* dam classifies
bare-relative and `./`-prefixed include literals as unproven (the amended A5
rule — runtime resolves them against `include_path` first), while the transform
obstacle detector still resolves a relative literal against the including
file's directory and treats an in-universe hit as benign. The transform side
was kept byte-identical when the checker rule was corrected; the two surfaces
do **not** share the amended rule yet.

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

## The three shipped transforms

**`phpdoc-to-native`** — promotion (ADR-0034 point 4, ADR-0037). Turns a
docblock-only type into a runtime-enforced native declaration when every call
site provably flows it. Landed through method scope (ADR-0043 stage 5) with the
full refusal taxonomy.

**`phpdoc-honesty`** — the inverse (ADR-0037 point 4, ADR-0041 point 4). Widens
a *lying* `@param`/`@return` to the proven truth from call-site and return
evidence. Where promotion tightens code toward the runtime, honesty repair makes
the documentation stop lying about it.

**`throws-envelope`** — `@throws` envelope seeding (issue #115, ADR-0040). For
every declaration with an envelope-relevant escaping throw class, writes the
**proven** escape set — exactly the classes behind `throw.undeclared` — as
`@throws \FQN` tags, creating the docblock when absent and extending it
losslessly when present (whole inserted lines; every existing line
byte-preserved, verified by a re-parse round-trip before the edit enters the
plan). A Maybe escape refuses `escape-not-proven`, never annotates (ADR-0037:
written-by-tool is declared, not proven); the second run refuses
`already-declared` (idempotence). Unlike its siblings it consults no vouch
valve: proven escapes are forward facts, so caller-enumeration obstacles have
no bearing. It is also the one transform measured on the default surface alone
(see above) — the recorded surface decision, and the only transform for which
it holds.

**`loop-to-array-map`** — ADR-0010's flagship, landed under ADR-0076. The first
transform whose precondition is an **effect** judgment: an append loop becomes
`array_map` only where the engine proves the body's effect lane empty on every
label, the exhaustiveness bit intact, and — stricter than ADR-0006 `Pure`, which
admits `throw` — the proven throw set empty, because a body throwing on element
*k* leaves the accumulator holding the first *k* results and every enclosing
`catch` can see it. Declared `≤` bounds never qualify (ADR-0067): the probe keeps
the lanes apart and the transform reads a non-empty declared lane as unproven,
which matters because the effect pass deliberately discharges the exhaustiveness
taint at a call a declared receiver answered.

The seam is `steins_infer::region_purity_project`: each pass's per-origin
classification, applied to a byte span instead of a whole unit, so the
precondition is the fixpoints' own verdict rather than a second opinion. The
subject's `array` / `is_list` facts come from `steins_infer::probe_subjects`, a
thread-local probe the walk answers from a statement's entry environment.

Candidates are **every** `foreach` in the analyzed set, so the refusal
distribution measures v1's narrowness instead of hiding it.

Measured whole-universe closing run of the first two: **23,148 / 509 candidates
enumerated, 0 transformed** — dynamic dispatch is the sound floor, and
partitioning is the recorded way past it.

## Not implemented

- **New transform kinds** — DTO promotion (array-shape sprawl → class), stringly
  → enum. Queued for M7 (ADR-0034).
- **Fold- and dataflow-backed transform proofs.** v1's dominance argument is
  literal-only (`argument-not-proven`, ADR-0041 §1).
- **`steins mcp`** — the dry-run → diff → approve → apply loop over MCP, with
  `EditPlan` as the wire currency (ADR-0010, roadmap M7).
- **Fix-its** — a transform attached to a diagnostic as a payload (`check
  --fix`).
- **Partitioning slices B–E** and the checker-side region scoping (ADR-0047 §9).
