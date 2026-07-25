# Argument-dependent builtin returns: the type rung, its envelope gate, and the PHPStan port terms

ADR-0056 left one rung out deliberately. A pure builtin with all-literal
arguments folds to a Singleton by executing the real function (ADR-0028's
allowlisted folding); every reflected builtin call is seeded with its
runtime-confirmed return envelope (ADR-0056 §1.1); between them sits the
call whose arguments carry *facts but not literals* — `count($list)` where
the walk knows the list's shape, `explode($sep, $s)` where `$sep` is a known
non-empty string — and there nothing computes. The nsrt gap classes ADR-0056
records (scalar unions 970, plain string 893, bool predicates 741, refined
strings 675, plain int 575, int ranges 546) are only partly reachable by
argument-insensitive rows; the residue is argument-dependent by nature, and
ADR-0056 §4 deferred it with a one-paragraph design. This ADR discharges
that deferral: it lands the **type rung** — a compiled rule computes a
return fact from argument facts — and settles the terms on which PHPStan's
MIT-licensed dynamic-return-type logic is translated to populate it. The
two rules that made the silence correct still bind: ask-the-real-thing
(ADR-0004) and zero-FP (ADR-0002).

## 1. The four-rung ladder

Per builtin call, most precise admitted fact wins; every rung below stays
as the floor:

| Rung | Condition | Result |
| --- | --- | --- |
| **Value** | foldable allowlist, all args literal, sidecar live | execute the real function; Singleton (ADR-0028) |
| **Type** | a rule for the name accepts the call's argument facts | the rule's output, admitted under §2 |
| **Envelope** | the sidecar reflects a return type | the reflected envelope alone (ADR-0056 §1.1) |
| **None** | no sidecar | nothing — the sound subset (ADR-0004) |

This composes with the existing ADRs; it restates none of them:

- The value rung is untouched. A fold that fails — timeout, crash, unknown
  function — still widens per ADR-0028's invariant, but it now widens onto
  the type rung rather than past it.
- A `return_facts.toml` row (ADR-0056 §3) is the degenerate type-rung rule:
  a constant, argument-insensitive one. The rung subsumes the rows rather
  than competing with them — when a ported rule exists for a name it is
  consulted first, and a rule that **declines** (argument facts outside the
  shapes it handles) falls back to the row, then to the envelope. Declining
  is a first-class outcome: it is the house translation of a PHPStan
  extension returning null, and it is how "silence over guess" survives the
  port.
- The type rung's output is not an alternative to the envelope but a
  refinement inside it — §2 makes that structural, not aspirational.
- ADR-0057's consumption ladder is unchanged: a builtin call still takes
  the builtin lane; this ADR upgrades what that lane can carry.

Two ADR-0056 positions are touched by name. The §4 v1 bound
("argument-insensitive facts only") is **superseded**: its deferred v2 form
— a guarded arm keyed on one argument's General base — survives as the
simplest rule shape, but rules may consume any layer of the argument's fact
(a Singleton separator, a OneOf flag set, a Refined length), because the
value domain already computed it and matching on it costs nothing. The §6
refusal of "dynamic-return-type extension machinery" **stands where it was
aimed**: rules are compiled into the catalog crate, keyed by function name,
enumerable and testable like every other table — not a plugin protocol.
ADR-0039's seam remains where a plugin consumer would someday attach; none
exists, so none is built.

## 2. The admission gate: ported logic proposes, the runtime countersigns

**A type-rung rule's output is admitted only under the full three-leg gate
of ADR-0056 §2**: the sidecar is present; the sidecar-reported PHP minor
equals `PINNED_PHP`; and the output is an extensional subset of the
reflected envelope. Any leg fails ⇒ the output is not seeded at all and the
ladder falls through to the next rung.

This has to be argued, because a ported rule set is precisely the artifact
ADR-0014 warns about. That ADR's reason for sidecar-audited catalog
sourcing is that hand-maintained function maps rot *silently* — and a
translation of PHPStan's dynamic-return extensions is a hand-maintained
function map with better provenance: hundreds of per-function judgments,
authored once against one upstream snapshot, drifting as php-src moves and
as upstream fixes bugs Steins will not automatically inherit. Importing
that body of knowledge without a countersignature would graft PHPStan's
maintenance treadmill onto a project whose whole posture is that the
running engine answers for itself.

The gate converts that liability into bounded sharpening, by direction of
failure:

- **Widening staleness** — the runtime grew an arm the rule predates
  (`string` became `string|false` in a later minor; a build flag added a
  nullable path). The subset check catches it per call: the rule's narrower
  claim is no longer contained in what this engine declares, the output is
  discarded, the envelope stands. A stale rule loses precision; it cannot
  manufacture a premise from a type the running engine disowns.
- **Narrowing staleness within an arm** — the honest limit of the subset
  check, stated so nobody leans on it: a rule claiming `non-empty-string`
  where this runtime can return `""` still passes the subset check, because
  the refinement lives *inside* an envelope arm the check cannot see into.
  ADR-0056 §2 already closes this direction for curated rows with the minor
  pin plus per-row evidence, and the port inherits both: the pin guarantees
  the engine is the one the port was validated against, and **every
  refinement a ported rule asserts beyond the reflected envelope carries a
  behavioral witness at `PINNED_PHP`** — a `php -r` probe or php-src
  citation recorded in the rule's fixtures, ADR-0056 §3's evidence bar
  transposed from TOML rows to rule tests. Upstream's say-so is provenance,
  not evidence.

So the zero-FP argument for the port is three-legged, not one-legged:
containment by the subset check, environment identity by the pin, and
within-arm truth by the witness. Under all three, ask-the-real-thing
survives the port intact — PHPStan's logic proposes, the project's own
engine draws every boundary.

**The cost, named so it is never rediscovered as a bug.** Where the
declared envelope is coarse, a *correct* ported refinement is discarded:

- A builtin reflecting neither a return type nor a tentative one hosts no
  rung at all — nothing to refine within (ADR-0056's recorded posture,
  inherited unchanged). The rule is dead weight on that engine.
- The subset check compares against the *lowered* envelope; precision the
  lowering cannot express coarsens the boundary, and a refinement that
  would have fit the true envelope can fail against the lowered one.
- Minor skew withholds the entire rung — on a sidecar reporting a different
  minor, every ported rule and every curated row goes quiet and analysis
  runs envelope-only. That is the gate working, and doctor's posture
  surface is where a user learns why their types got wider.

The discard is deliberate asymmetry: a lost refinement costs one `unknown`
that PHPStan would have typed; a wrong refinement costs a false premise
under a zero-FP bar. The ledger only balances one way.

## 3. Stratum: binary admission, derivation-clause arithmetic

**No third stratum.** The N2 machinery (ADR-0052 §5 and its amendment) has
two strata and a min-rule; this ADR adds no tier and forks no join.

- Gate failure means **not seeded** — never demoted-to-Asserted. ADR-0056
  §2's argument transfers verbatim: Asserted seeding would make fixture and
  dump behavior diverge between sidecar modes, and would put translated
  upstream judgments into the narrowing stream on the strength of nobody's
  runtime.
- An admitted output enters at **the minimum stratum over the argument
  facts the rule consumed** — the ADR-0052 derivation clause applied, not
  extended. The rule itself is Verified-grade (runtime-countersigned by the
  gate), but its conclusion is only as trustworthy as its premises:
  `count($arr)` computed from a shape the walk *proved* is Verified;
  computed from a shape a docblock *claimed* is Asserted.
- Consequences fall out of existing rules rather than new ones: a
  proof-layer finding may premise a type-rung fact only when every consumed
  argument fact was Verified (the all-Verified premise rule); an
  Asserted-derived refinement never overwrites the Verified envelope fact
  (N2's replace-if-weaker half) and serves the contract layer and requested
  introspection instead. The envelope floor is Verified always, exactly as
  R1 shipped it.

## 4. The port policy: what "ported" means, and where the notice lives

Owner decision, recorded: PHPStan's dynamic-return-type logic is
**translated, with its MIT licence stated**. MIT into Apache-2.0 is one-way
compatible — this section is about attribution, not permission.

**What counts as ported.** A Rust item is *ported* when it was authored by
working from identifiable phpstan-src source text — translating an
extension class or its helpers, however far the expression traveled in
transit. It is *independently implemented* when authored from PHP's
documented or probed behavior, php-src stubs or C source, or Steins' own
design — even where the result is extensionally identical to PHPStan's,
because copyright protects expression, not behavior. The boundary is what
text was in front of the author. Doubt resolves to *ported*:
over-attribution costs a line; under-attribution is a licence violation.
The precedent boundary already in the tree confirms the definition from
both sides — the phpstan-src data-provider tests are ported fixtures
(ADR-0030 said so when it took them), while the four-layer domain is
independent by construction (ADR-0035 diverges from the accessory model on
purpose).

**How a file records it.** Every ported item's module carries a
fixed-format header:

```
// Ported from phpstan-src: src/Type/Php/CountFunctionReturnTypeExtension.php
// @ <upstream tag or commit> (MIT). Notice: NOTICE at the repository root.
```

One upstream file per Steins module is the working grain, so the marker is
file-level; the format is fixed so a future check can cross-reference
headers against NOTICE mechanically. Independently implemented code carries
no marker — absence of the header *is* the claim of independence, which is
why the definition above has to be sharp.

**Where attribution lives: a hand-maintained `NOTICE` file at the
repository root.** Not `THIRD-PARTY-LICENSES.md` — that file is generated
(`cargo xtask licenses` runs cargo-about over Rust *dependency* metadata),
regeneration would destroy a hand-written section, and CI's drift guard
would fail the tree either way. The split is principled, not incidental:
the generated file covers exactly what cargo's dependency graph can see,
and ported source is invisible to it — a translation enters the tree as
authored Rust text, leaving no crate edge for any tool to discover. (The
Mago fork shows the contrast: it is a crate dependency, so its notices flow
into the generated file automatically.) Provenance no generator can see
needs a file no generator writes.

`NOTICE` is the right such file because Apache-2.0 §4(d) already obligates
every redistributor to carry it — the MIT notices ride an obligation that
exists, instead of a bespoke file with a bespoke rule someone must
remember. Its contents: Steins' own copyright line, then one entry per
upstream project whose source has been translated — project name, the
upstream `LICENSE` text verbatim at the consulted tag (MIT's condition is
that the copyright and permission notice accompany copies; the full short
text is cheaper than an argument about "substantial portions"), and that
tag. It is embedded in the binary and printed by `steins license`
alongside `LICENSE` and `THIRD-PARTY-LICENSES.md` — the exact pattern issue
#43 landed, and for the same reason: a Homebrew or `cargo install --git`
user gets a bare binary, and the notices must travel with it. It also
ships in the release archives.

**The sync rule.** Headers carry the per-file inventory; `NOTICE` names
each upstream project once. The file changes only when the first ported
file from a new project lands or the last one leaves — so it cannot rot
file-by-file, and the fixed header format leaves a mechanical cross-check
(headers referencing projects absent from NOTICE, or vice versa) available
as an xtask guard when the first port slice lands.

**The relicensing boundary, ADR-0025's third case.** ADR-0025 has two
cases: the MIT attributes package (vocabulary meant to spread) and the
Apache-2.0 core. Ported MIT logic *inside* the Apache-2.0 core is a third:
the translated file is sublicensed under Apache-2.0 with the rest of the
work — MIT permits exactly that — and what survives the boundary is the
notice obligation, discharged by the header and `NOTICE`. Nothing about
the package boundary moves: the analyzer stays Apache-2.0, the attributes
stay MIT, and a licence audit now reads three things — the dependency
graph, the generated file, and `NOTICE`.

## 5. The translation friction, stated

PHPStan's extensions are written against PHPStan's type system; a literal
transcription will not compile, and a faithful one will not land. What
ports is the **logic** — the case analysis mapping argument shapes to
return shapes, the boundary conditions, the enumerated special cases. The
expression is rewritten twice, and anyone porting should expect both
rewrites on every function, not discover them three functions in:

- **Into the four-layer domain (ADR-0035).** Upstream composes Type
  objects: `ConstantIntegerType` becomes Singleton; small finite results
  become OneOf under its cap; accessories and `IntegerRangeType` become
  Refined predicate bits and `IntRange`; `TypeCombinator::union` becomes
  the domain join (Steins has no type combinator — ADR-0030's amendment —
  and a port that tries to rebuild one has left the rails). The predicate
  set is closed: an upstream output with no Steins spelling widens to the
  nearest expressible layer, and the rule's tests record the loss rather
  than the port quietly extending the vocabulary.
- **Into the fact-and-stratum discipline (ADR-0037/0052).** An upstream
  extension receives a `Scope` and may consult context Steins does not
  hand the builtin lane; where the logic needs more than the call's
  argument facts carry, the rule declines and the envelope stands. Every
  output re-enters through §2's gate and §3's stratum arithmetic — no
  ported code path bypasses either.
- **Version branches resolve at port time.** Upstream logic carries
  version conditionals because PHPStan emulates many PHPs; Steins refuses
  version emulation (standing refusal, ADR-0056 §6), so a port strips the
  branches to the `PINNED_PHP` truth and the witness for that truth goes in
  the fixtures. A ported rule is a snapshot of upstream's knowledge about
  *this* engine, not a fork of upstream's compatibility matrix.

Each port slice lands with the discipline ADR-0056 §5 set: fp-gate with
verbatim triage on any tripwire movement (the FP channel is unchanged — a
wrong fact "disproving" a correct docblock via `phpdoc.return-mismatch`),
corpus run, and the nsrt match-rate delta as the acceptance instrument.
The oracle that measured the gap referees each rule that claims to close
part of it.

## 6. ADR-0048's three constraints, discharged for the rung

- **Replayability.** A rule evaluation is a pure function of (function
  name, the call's argument facts, the per-name reflected envelope, the
  generated tables). No IPC happens at rule time — the envelope comes from
  R1's per-name-per-run cache, which joins the fold memo in the replay
  tuple as a recorded input. A scope re-walked later with the same entry
  state and the same cached answers reproduces every type-rung fact
  bit-for-bit.
- **Canonical entry state.** The rung contributes nothing to entry states
  and consumes nothing from them beyond the argument facts the walk already
  carries; no new fact kind crosses a scope boundary.
- **No global ordering.** Admission is decided per call site from
  call-local inputs; no rule observes another call's outcome, and the
  whole-project pass order can change without moving a single fact.

## 7. Refusals

- **A plugin protocol for return rules** — ADR-0056 §6's refusal, restated
  with its boundary: compiled rules in the catalog crate, enumerable and
  testable; ADR-0039's seam waits for a plugin consumer that exists.
- **Wholesale porting sweeps** — translating upstream's extension directory
  end-to-end is the bulk import ADR-0056 §6 refuses in row form. Rules land
  in measured-priority order, each with fixtures and witnesses; the per-rule
  evidence bar is the point, not an inconvenience.
- **Version emulation** — standing refusal; §5's port-time resolution is
  the whole accommodation.
- **A third stratum, or Asserted seeding on gate failure** — §3; ADR-0056
  §2's arguments inherited unchanged.
- **Rules for builtins with no reflected envelope** — nothing to refine
  within; revisit with a measured case, per ADR-0056's open question.
- **Method-keyed rules in v1** — deferred with ADR-0056's method rows, on
  the same grounds (the reflect slice's method surface first; a half-keyed
  path would misclassify rather than refuse honestly). The port terms of §4
  apply unchanged when they arrive.

## 8. Open questions

- When a name has both an admitted ported rule and an admitted curated row,
  v1 takes the rule (or the row, when the rule declines). Intersecting the
  two admitted facts is sound in the domain (bit-and, interval meet) but
  has no measured case where it buys anything; revisit with one.
- Whether the rung's machinery should someday serve narrowing-side ports
  (PHPStan's type-specifying extensions — guard territory, ADR-0052's
  lane, not return typing). Named as adjacent and deliberately not covered
  here; those ports would need their own gate argument, since no return
  envelope bounds a narrowing claim.
- The header↔NOTICE mechanical cross-check (§4's sync rule) as an xtask/CI
  guard — decided desirable, specified when the first port slice lands.
