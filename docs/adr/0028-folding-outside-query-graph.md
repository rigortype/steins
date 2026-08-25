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

## Amendment (2026-08-01): a bounded union of constants folds member-wise (issue #74)

A fold argument was one constant. ADR-0069's amendment tabulates the return
ladder against PHPStan's extension stack and names the residue: an extension
takes a constant **or a union of constants**, calls the real function per
member, and composes. `$x = $c ? 'a' : 'b'; strtoupper($x)` widened here
where PHPStan answers `'A'|'B'`. The seam now enumerates the union.

1. **Nothing new crosses the wire, and no second lowering exists.** Each
   member combination is an ordinary fold: it is handed back to the same
   `try_fold` a written literal call takes, so the allowlist, the shadowing
   refusal, the array budget of the amendment above, the `(function, args)`
   memo and issue #64's integer-width gate apply once, in one place. The
   member→argument conversion is the domain's own `Val`→`ArgValue` seam.

2. **The resolution ladder is literal, then union.** An argument resolves by
   `resolve_literal` (which already reduces a `Singleton` env fact, a nested
   fold and a proven concatenation), else by a `Fact::OneOf` env fact every
   member of which is a foldable argument. Anything else declines the whole
   call — the silence that was already there, reached one rung later. A
   `Singleton` is the one-member case of the same ladder, which is what makes
   `str_repeat($union, 2)` a product of four and one.

3. **Bounds, and a decline rather than a truncation.** At most 4 members per
   argument and 16 combinations in total, charged **before** any combination
   is built so an over-wide union costs no engine traffic. Over either bound
   the fold declines. This is not the array budget's reasoning, which widens
   for cost: a union missing a member is a *wrong* value domain, not a wider
   one, so there is no admissible partial answer to fall back on. A member
   that widens or throws declines the whole fold for the same reason.

4. **Every combination is asked before the decline is returned.** The verdict
   is unchanged, but the browser's replay transport (ADR-0066) collects a
   *batch* of unanswered requests per iteration, so asking the whole product
   in one pass is the difference between one round trip and one per member.

5. **Composition is `Fact::from_vals`** — the narrowing lanes' own finite
   constructor. Past the domain's `OneOf` CAP it hands back its *computed*
   widening (ADR-0035), which is sound and still strictly better than the
   declared envelope.

Against ADR-0048 again: the enumeration order is fixed by the argument list's
source order and the `OneOf`'s own canonical (sorted, deduped) order, with the
last argument varying fastest — no map iteration enters it, so the walk stays
the same pure function of (CST, entry state, query answers, fold memo). The
stratum is the §5 derivation clause verbatim: every member answer is
engine-`Verified`, but the composed fact consumed the input facts, so it
carries `min` over them — an `Asserted` union in, an `Asserted` fact out.

## Amendment (2026-08-14): array results cross the seam (issue #330)

The amendment above carried an array *into* the sidecar and stopped there;
its §5 declined the return direction, on the ground that "carrying a folded
array back would seed synthesized array facts into the env, which is
#41/#42's subject". That ground has since dissolved from two sides. #41/#42
closed on the type rung, and issue #327 gave a literal with one unknown slot
its own `Fact::Shape` rung — below the concrete path, reached only when a
slot is unknown. A fold result has no unknown slot: PHP built the whole
array, so it lands on the **concrete** path (`Val::Array` → `Fact::Singleton`)
and never constructs a synthesized shape at all. The seam now carries an
array result.

1. **The `order` witness question does not arise.** #327's `ShapeFact::order`
   is provenance-only and deliberately inert, because believing a *declaration*
   order is the phpstan#14940 false-positive class. A `Val::Array` is
   `Vec<(Key, Val)>` in insertion order — the order is part of the value, not
   a claim about the source — so a folded `array_merge` answer needs no
   witness and grants none.

2. **The result envelope is the argument envelope, with a stricter key
   rule.** Same `__steins_array` tag, same recursion, same nesting. But a
   result is an array PHP has already finished building: every key is
   materialized, duplicates are resolved, normalization has happened. The
   absent-key spelling (`null`) that the argument direction needs is
   therefore *unreachable* in a result, and the result decoder rejects it as
   malformed rather than accepting it. Accepting it would mean Rust
   re-deriving a next-int this engine did not choose — the exact failure
   ADR-0004 exists to prevent — and rejecting it turns that class of runner
   bug into a widen. A duplicated key is rejected on the same ground: a
   materialized array cannot contain one, and honoring it would be Rust
   choosing last-wins on the engine's behalf. So is an integer-like string
   key (`"5"`, by `php_canonical_int_string` — the same primitive the
   write side uses): the engine casts it to `5` when the array is built,
   so keeping it as a string would be a *wrong* fact and re-casting it in
   Rust would be the re-derivation this clause forbids.

3. **The result budget is the argument budget, charged twice.** The same 256
   entries / 8 levels, charged in the runner *before* encoding (so an
   oversized answer never becomes a megabyte of JSON) and again on arrival
   (so the gate's verdict and the decoder's are the same verdict computed
   twice). Over budget widens. The invariant is symmetry: a shape admissible
   as an argument is admissible as a result.

   This is **not** issue #258's pre-flight bound and must not be merged into
   it. #258 prices "the child will die allocating this" from the runner's
   `memory_limit`, before the request is sent; this prices "how much value
   the env will absorb", after it returns. `range(1, 1000000)` passes the
   first and fails the second, so a single constant would have to be wrong
   for one of them.

4. **The integer-width classification becomes three-valued.** `PORTABLE`
   is evidenced by differential probes and `REFUSED` by a recorded
   divergence per row; a name with neither belonged to neither, which is why
   the allowlist held no array-returning name. `UNVERIFIED` is that
   third place: *not measured*, so the name folds only on a provably 64-bit
   engine and declines in the browser — which is exactly what `foldable`'s
   default-deny sentence already described, now a row instead of a gap.
   `foldable` becomes a predicate derived from `portability_class`, so that
   "refused (a divergence is on record)" and "unverified (nobody looked)"
   cannot be conflated: the refused rows' one-divergence-per-row discipline
   is what makes them auditable, and mixing in unevidenced rows would erase
   it. Promotion to `PORTABLE` is a later slice with php-wasm probes; the
   correct number of probes behind an `UNVERIFIED` row today is zero.

5. **A name joins the allowlist only when the fold is strictly stronger than
   the rung it would shadow.** `array_slice` has an exact Rust rung that
   answers `array_slice(['x', $s, 'z'], 1)` as `list{string, 'z'}` — with a
   non-literal element, which a fold can never have. The fold would cover a
   proper subset of what the rung already covers exactly, so it buys a second
   implementation of the same answer and a fixture to keep them agreeing.
   It stays off the list. `array_combine` and `array_fill_keys` are excluded
   by the same rule (issue #335 gave them exact rungs). `explode` and `range`
   are admitted by it: their rungs are type-level only
   (`non-empty-list<string>`, `non-empty-list<int>`), so the fold upgrades a
   type to a value on the all-literal path — and the rung survives beneath it
   as the no-sidecar floor.

   Note the rule cuts both ways: a name whose rung later becomes exact should
   *leave* the allowlist, and this clause is the warrant.

6. **No version handshake, because no two versions can meet.** `runner.php`
   is embedded in the binary via `include_str!`; the browser runs that same
   text unmodified inside php-wasm; ADR-0066's answer table is built live in
   the session rather than recorded and shipped. There is no pairing of an
   old encoder with a new decoder anywhere in the system, and the
   `__steins_array` tag is its own discriminator in the one place a stale
   pairing could otherwise be constructed. ADR-0080 §3.1 anticipated "its own
   version handshake" for the byte-string wire; on the evidence above that
   cost is not real, and §3.1 is annotated accordingly so the next reader does
   not budget for it.

7. **Byte strings slot in rather than being retrofitted.** `Val::Str` is
   already a byte string (ADR-0080), so the domain is ready and the only
   obstacle is JSON. The result decoder is therefore written so a tagged
   bytes variant is a sibling of the array tag inside the same envelope, not
   a new envelope. Until §3.1 lands, a non-UTF-8 string anywhere in a result
   widens the whole result — pinned in both directions, including the
   *scalar* result case that had a runner branch but no test.

The whole change fires only when every argument is a literal, which is why it
is last in its issue chain and why its wave order is by frequency against
that condition: `str_replace` and `substr_replace` first, since they are
already `PORTABLE` and only §5 held their array results back — so they need
no width verdict and work in the browser — then `array_merge` and `explode`
behind the new class.

Against ADR-0048 once more: the answer is still a pure function of
`(function, args)`, and an array answer does not weaken that — PHP's array
construction is deterministic for a fixed argument list, and the insertion
order the value carries is that determinism made visible rather than an
extra input. The stratum is §5's derivation clause unchanged: an all-literal
argument list is `Verified`, so a folded array is a `Verified` `Singleton`,
with the same consequence #327 measured — a `Verified` array fact is strong
enough to move the contract lanes, and that movement is a true positive.

### Follow-up (2026-08-15, issue #354): the five deferred names, measured

The wave order above stopped after wave 1 and deferred five names §5 had
already admitted in principle: `array_unique`, `range`, `preg_split`,
`str_split`, `array_fill`. All five are now probed, and the ADR-0066 amendment
of 2026-08-15 carries the tuples, the counts and the two divergence witnesses.
Three land in `PORTABLE` (`array_unique`, `str_split`, `array_fill`) and two
in `REFUSED` (`range`, `preg_split`).

Two of the clauses above are worth reading against that outcome:

- **§4 held.** `UNVERIFIED` did not grow by a single row. Each probed
  name went to the class its evidence chose, which is what the class was
  defined to make possible; its two rows still have zero probes behind them.
- **§5's admission of `range` stands, and the fold is still strictly stronger**
  — `range_transfer` answers a type, the fold a value. A width refusal does not
  contradict §5: a `REFUSED` name is still `foldable`, so the rung it
  shadows is shadowed on the CLI and stands beneath it in the browser, exactly
  as the clause describes.

One prediction in this amendment was wrong in a way worth recording. Wave 0
admitted `str_replace` and `substr_replace`'s array results "with no width
verdict needed", reasoning that the array form of an already-`PORTABLE` name
cannot introduce one. The reasoning holds, but the *check* behind it could not
have caught a counterexample: array elements cross the seam with no per-element
type tag, so an `int`/`float` flip inside a result is visible only in the
response bytes, and the probe harness of the day compared parsed JSON. Both
names were re-probed bytewise in #354 and are unchanged — the conclusion was
right, the evidence for it was not, and that distinction is the reusable part.

## Amendment (2026-08-25): the revisit trigger lands as generation inputs (ADR-0092 §4)

The trigger above said fold results would move into the graph "as an
explicit salsa input layer". ADR-0092 restates the destination: fold
results become recorded rows in the per-package generation artifact,
written and replayed through the ADR-0066 table seam, keyed under the
generation's engine identity — a different engine is a miss, never a
reinterpretation. The invariant is untouched: a fold that fails widens,
a recorded row never outlives the fingerprint that scopes it, and the
`replay_fold.rs` differential oracle is the acceptance pin that
replay-from-disk means what ask-the-engine means.
