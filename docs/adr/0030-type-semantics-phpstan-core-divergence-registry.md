# Type-operation semantics: PHPStan's denotational core + a divergence registry

The semantics of type operations import in three tiers:

**Imported as-is** — the denotational core: types denote value sets,
acceptance is set inclusion, judgments are trinary (yes/maybe/no — the same
shape as the Certainty discipline), constant types sit below general types
in the lattice. Compatibility on this core is fixed mechanically by porting
phpstan-src's data-provider tests (`TypeCombinator`, accepts/isSuperTypeOf)
as fixtures — the ADR-0029 discipline applied to semantics.

**Deliberately divergent** — tracked in a **divergence registry** (the
rigor-rs concept imported: every intentional departure is recorded with its
rationale and, where applicable, the upstream proposal it feeds). Initial
entries:

1. **Two acceptance relations, not one.** Declared-contract acceptance
   (envelope checking; PHPStan-like, no coercion) is distinct from runtime
   survivability (proof layer; the empirically-verified coercion tables).
   PHPStan has only the former; the latter is Steins' differentiator and has
   no import source.
2. **Array-shape / list semantics follow the phpstan/phpstan#14939 RFC
   natively**: `array{...}` is an order-agnostic key *set*, `list{...}` a
   positional key *sequence*, `isList` trinary. Steins is the BC-free
   proving ground (ADR-0016) where the proposed semantics run first and
   produce evidence for the upstream discussion.
3. **No benevolent unions.** `BenevolentUnionType` is a compensation
   mechanism for worst-case-reporting false positives; a proof layer that
   acts only on proven values does not need it. **Grammar compatibility is
   preserved**: the parser accepts `__benevolent<T1|T2>` (it occurs in real
   stubs) and expands it to plain `T1|T2` — accepted syntactically,
   erased semantically. The replacement for what benevolence compensated
   (practically-infallible `|false` returns like `curl_init`) is
   failure-cause labels on union arms + policy-profile consumption:
   ADR-0042.

**Deferred until needed** — narrowing details (co-evolving with the branch
analysis ratchet), template variance in full, subtraction types: decided in
envelope-checking priority order, not up front.

## Governing rule (amendment)

Vocabulary and minor judgments track PHPStan's model (yes/no/maybe,
message idioms, familiar spellings) — familiarity is cheap and compounding.
But when a decision touches the *nature of the inference* and a
fundamentally better outcome is in reach, Steins replaces the PHPStan
approach **without hesitation** (precedents: call-site propagation over
modular analysis, ADR-0001; no template solver where propagation reaches,
ADR-0032). The divergence registry is what makes this boldness safe:
every replacement is recorded, justified, and traceable back upstream.
