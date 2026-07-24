# Certainty

**Status: implemented** (`steins-domain::Certainty`, re-exported by
`steins-infer`).

## One trinary, project-wide

PHPStan's `TrinaryLogic` and Rigor's `Certainty` are the same lattice, and
Steins has exactly one of it (ADR-0031). Condition evaluation in the branch
walk, PHPDoc contract acceptance (ADR-0030), the is-a oracle (ADR-0043), and the
value domain's own queries all speak the same type:

```
Yes    — provably true
No     — provably false
Maybe  — not decided; the honest middle
```

## Composition

Kleene strong three-valued logic, exactly:

```text
and:  No ∧ _ = No        or:   Yes ∨ _ = Yes       not: ¬Yes = No
      Yes ∧ Yes = Yes           No ∨ No = No             ¬No  = Yes
      _ ∧ _ = Maybe             _ ∨ _ = Maybe            ¬Maybe = Maybe
```

Two lifts exist: `from_bool` for a decided boolean, and `from_opt` where `None`
means `Maybe` — the idiom for "the helper could not decide".

## The discipline

**`Maybe` never promotes.** Combining evidence can only move *toward* `Maybe`;
it can never conjure a `Yes` out of repetition. Two `Maybe`s do not make a
`Yes`, and a hundred do not either.

**`Maybe` is silence in the proof layer.** This is the operational meaning of
the zero-FP bar: a proof-layer diagnostic fires on `Yes` and on nothing else.
`No` and `Maybe` alike produce no output, for different reasons — `No` because
there is nothing wrong, `Maybe` because we do not know.

**Absence proofs invert the polarity.** For the finding-breadth family
(`call.undefined-method`, `offset.missing`, …) the *claim* is an absence, so the
reporting condition is a definite `No` on the "does it exist" question. The
discipline is unchanged: `Maybe` is silence. See [dynamism.md](dynamism.md).

## Where `Maybe` comes from

Enumerated so a reader can predict silence rather than discover it:

- **Unresolvable names** — an FQN with two definitions in the project index is
  never resolved (PHP would fatal on a real double-definition, and we cannot
  know which body runs).
- **Dynamic dispatch** — a receiver whose class is not proven exact.
- **Budget cutoffs** — binding descent past `MAX_BINDING_DEPTH`, or a recursion
  cycle. A cutoff *names itself* as silence; it never manufactures a finding.
- **Opaque contract types** — everything `ContractTy::Opaque` covers:
  conditionals, templates, const fetches, `$this`/`self`/`static`, offset-access
  types. See [contract-types.md](contract-types.md).
- **Provenance-flavored string types** — `class-string`, `literal-string`, and
  kin are non-extensional (ADR-0038), so they can decide `No` for a non-string
  but never `Yes` for a string.
- **No sidecar** — the sound subset. Anything requiring PHP execution widens.
- **A poisoned scope** — `extract`/`compact`, `global`, `static $x`,
  variable-variables, reference assignment, by-ref closure capture, or
  `include`/`eval` in a body makes no variable value known anywhere in that
  scope.
- **Unknown hierarchy** — an is-a question about a class Steins cannot fully
  enumerate resolves `Maybe`, never `No` (see [object-model.md](object-model.md)).
