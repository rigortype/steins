# Folding runs outside the salsa query graph (for now)

Salsa queries must be deterministic functions of their inputs; a fold is an
IPC call into an external PHP process. Rather than pretending the sidecar is
pure, the check pass that may fold runs as a plain function *outside* the
query graph — parse and function indexing stay memoized salsa queries, and
fold results are memoized per run in a plain map keyed by
`(function, args)`. For allowlisted pure+deterministic builtins the fold IS
referentially transparent, so this placement loses no correctness, only
cross-run incrementality of folded findings.

Revisit trigger: when LSP incrementality makes re-folding on every
keystroke measurable, fold results move into the graph as an explicit salsa
input layer (recorded facts fed back into queries), not as hidden impurity
inside a tracked function. The invariant either way: a fold that fails —
timeout, crash, unknown function — widens and never invalidates
memoized analysis (ADR-0024's never-a-wrong-diagnostic rule).

## Amendment (2026-07-26): array literals cross the fold seam (issue #39)

A fold argument was a scalar literal and nothing else, so three entries on
the ADR-0008 allowlist — `count`, `in_array`, `implode` — could never
qualify: their arguments are arrays. They were parked there waiting for
this. The seam now carries an **array literal** as an argument. The
allowlist did not change; the gate under it did.

1. **Nothing new is represented.** The trace IR has carried
   `ArgValue::Array` (keyed and unkeyed entries, nested recursively) and
   the domain its `Val::Array` Singleton since the offset family landed;
   what did not exist was the *wire form*. So this is not a new value
   carrier, and emphatically not an array-**shape** type domain — the
   array family of argument-dependent return typing (#41/#42) still has
   no representation here and is not started by this.

2. **The argument gate is `is_concrete_value`**, i.e. `is_literal`
   extended over the array carrier: an array is a fold argument exactly
   when every element, at every depth, is itself a self-evident value.
   One `Var`/call/offset-read element anywhere widens the **whole**
   array — `count([1, $x])` is not `2`, because `$x` is not known to be
   one entry. That widening is the ADR-0002 side and is fixture-pinned.

3. **PHP owns array semantics on the wire.** An argument encodes as an
   ordered *entry list* — `{"__steins_array": [[key, value], …]}`, key
   `null` for an absent key — and the runner rebuilds it with `$arr[] =`
   / `$arr[k] =`. Absent keys therefore get **this engine's** next-int
   (including the negative-key edge PHP 8.3 changed) and duplicates
   **this engine's** last-wins. A JSON object could express neither. The
   general principle restated: a fold is the value the project's own PHP
   produces (ADR-0004), so no array rule is reimplemented in Rust to be
   wrong about later.

4. **A budget, because a fold argument is now unbounded in size.** At
   most 256 entries counted recursively, at most 8 levels of nesting;
   over either bound the call widens. It is charged before the memo key
   is built, so a generated lookup table is never cloned, hashed,
   serialized, or executed. The depth bound is also what keeps the two
   recursive encoders (Rust's and the runner's) off an unbounded stack.

5. **Array *results* still widen** — a documented boundary, not an
   oversight. Carrying a folded array back would seed synthesized array
   facts into the env, which is #41/#42's subject, not this one. The
   runner already reports an array result faithfully; the Rust side
   declines it.

Against ADR-0048: a fold is keyed by `(function, args)` where the args are
syntactic literals of the call site, so the walk stays a deterministic
function of (CST, entry state, query answers, fold memo) — **replayable**,
contributing nothing to the **entry state** (a literal needs no seeding),
and reading no whole-project iteration order (**no global-ordering
dependence**). ADR-0052 §5's derivation clause needs no new case either:
an all-literal argument list is `Verified` by construction.
