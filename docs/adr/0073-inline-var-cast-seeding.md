# A statement-level inline `@var` is a cast: it re-seeds the lane a `@param` would have seeded

Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode).

## 1. Context: the seeding surface had a hole the corpus measures

ADR-0052 §9 made scope entry THE seeding point for declared contracts: a
parameter's native member list (`Verified`) refined by its `@param`
envelope (`Asserted`), and — since ADR-0062 S3 — a lane whose array
vocabulary collapsed to one arm also seeds the value lane with its
canonical shape fact. Every consumer downstream of that pair (the S7
projection family, the S3/S6 shape reads, the dump surface) answers over
a `@param`-declared subject.

PHP has a second spelling of the same declaration: the statement-level
inline docblock, `/** @var array{a: int, b: int} $arr */` above a
statement, PHPStan's cast idiom for a variable that is already bound.
That spelling seeded **nothing** — not because anything declined it, but
because no code path read it. The gap is not marginal: phpstan-src's
`nsrt/array-slice.php` carries 56 assertType observations and 53 of them
sat in `differ` purely because every fixture in the file declares its
subject through an inline `@var` rather than a `@param`; the issue #118
`array_slice` transfers, correct over `@param` subjects, could move one
row.

## 2. Decision: the tag is a cast, lowered by the `@param` machinery

A docblock **immediately preceding a trace statement** (only whitespace
between — the same adjacency rule declaration adoption has used since
ADR-0029) that carries `@var T $x` re-declares `$x` from that statement
on:

1. every carrier of the old value dies first (`env` entry, heap
   binding, member facts, contract lane — the same forgetting a rebind
   performs), because a cast *re-declares* what the variable holds
   rather than narrowing the declared possibilities;
2. the contract lane is re-seeded through the one lowering the `@param`
   path owns (`refine_contract_arms`), against an **empty native
   envelope** — so every arm seeds `Asserted`, the ADR-0037 trust order's
   answer for a declaration the runtime never checks;
3. a lane whose array vocabulary is one arm seeds the value lane with
   its canonical shape fact, `Asserted`, exactly ADR-0062 S3's entry
   rule (and A-G9's corollary holds by construction: a cast-derived fact
   can never premise a proof-layer finding).

What the projections, shape reads and dump surface then see over a cast
variable is indistinguishable from the same declaration arriving as a
`@param` — one seeding law, two spellings.

The native `array $arr` hint is deliberately NOT used as a refinement
envelope for the cast (the way a `@param` refines within its native
member list). A cast replaces; a contradictory cast (`@var string $arr`
under `array $arr`) therefore seeds the asserted lane it names instead
of seeding nothing. Asserted arms feed no proof-layer finding either
way, so the difference is spelling fidelity to PHPStan, which trusts
the inline tag outright.

## 3. The guards, each with its own reason

* **Property targets never cast.** `@var T $this->p`, `@var T $obj->p`
  and bare `$this` speak about a *property*; reading them as a cast of
  the receiver could manufacture declared-receiver findings (S6) out of
  a tag that says nothing about the receiver. The docblock scanner now
  flags these for `@var` exactly as it always has for the assertion
  family (`DocTag::property_target`, the renamed
  `assert_property_target`).
* **The adjacency is strict.** Any non-whitespace in the gap — code or
  a non-doc comment — breaks the association, mirroring declaration
  adoption. A property docblock can therefore never leak into a method
  body's first statement (`public $x;` sits in the gap).
* **`@template` names shadow** (issue #5, applied to the body): the
  owning declaration's set plus, for a method, the class-level set —
  the same two idempotent stages the declaration envelopes get. `@var T
  $x` under `@template T` stays opaque.
* **An unparseable or unlowerable type casts nothing** — the ADR-0029
  silence, never a `Barrier`-style erasure.
* **`@phpstan-var`/`@psalm-var` displaces the plain `@var`** for the
  same variable in the same docblock (the ADR-0029 precedence rule).
* **Plain per-scope pass only.** A binding descent carries
  call-site-proven values; propagated truth outranks a docblock
  assertion — the same reason the §9 entry seeding skips a
  descent-bound parameter. Skipping in a descent only ever keeps the
  *stronger* carrier.

## 4. Declined and deferred, explicitly

* **The assignment-form `@var` is deferred.** Above `$x = expr;`,
  PHPStan casts the *RHS*; this slice's pre-statement cast is simply
  erased by the rebind, so the tail sees the plain inference again — a
  silence, never a stale claim about the old value. Importing the RHS
  cast is its own decision (it must weigh a `Verified` RHS value against
  an `Asserted` override) and is not taken here.
* **The nameless `@var`** (`/** @var T */` above an expression) targets
  the following *expression*, not a variable; out of scope with the
  assignment form.
* **The ADR-0062 §2 declined import stays declined.** A cast seeds a
  key *set*; the positional projections over it remain the sound
  widenings S7 defines. The `array-slice.php` rows whose expectations
  read field declaration order as runtime order (the `array{…}`
  positional ones) must stay `differ` — a run that "fixes" them has
  imported the defect, not closed a gap.
* **Loop bodies and other `Opaque` regions** are untouched: a tag above
  a statement the trace does not model is not seen, the standing
  ADR-0027 ratchet direction.

## 5. Consequences

* `nsrt/array-slice.php`: the ten `@var`-declared normal-array rows
  (33/34/37/38/45/46/49/50/53/54) become answerable; the positional
  rows stay `differ` by design (§4).
* A new syntax query (`SourceTree::stmt_docblock`) exposes the
  statement-adjacency rule; the walk applies casts in one place, before
  a statement's own checks read the env.
* An inline `@var` naming a class seeds a declared-receiver lane, so S6
  can now judge method calls on cast variables — the same surface a
  `@param` already had, at the same `Asserted` stratum.
* The cast may *silence* proof-layer findings that a proven value would
  have premised (the old carriers die). That is the FP-safe direction,
  and PHPStan parity besides: the programmer asked for the tag's
  reading.
