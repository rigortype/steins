# phpdoc→native promotion v1: scope, refusal taxonomy, honesty-repair sibling

Concretizes ADR-0034 point 4 for the first shipped transform.

1. **Promotion domain (v1)**: free functions only. Methods are deferred
   wholesale — even the *eligibility* split (private/final-no-
   inheritance promotable vs `method-inheritance` refusal, a Liskov
   question per ADR-0033) requires sound method-call-site resolution
   across receivers, and a partial method path would misclassify rather
   than refuse honestly. The refusal taxonomy, oracle, and reverse-sweep
   architecture generalize when that resolution lands (deferred-with-
   design in the `promote` module docs). A param is a candidate only
   when the source carries *no* native hint at all (a complex hint that
   lowers to `None` is out-of-domain, not refused — the completeness
   oracle counts only true candidates). v1 proofs are literal-argument
   only: a non-literal caller arg refuses `argument-not-proven`
   (sound, not complete; fold-backed and dataflow proofs are the
   designed extension).
2. **Type domain (v1)**: only phpdoc types renderable as a `NativeType`
   (scalars, nullable, scalar unions). When the phpdoc type is strictly
   finer than its native rendering (`int<0,max>`, `non-empty-string`),
   v1 refuses (`phpdoc-finer-than-native`) rather than promote-and-keep:
   a half-promoted pair invites drift, and the finer-type story belongs
   to the honesty layer, not the promotion layer.
3. **Refusal taxonomy** (named, per ADR-0034's first-class Refusal):
   `dynamic-call-present`, `resolution-ambiguous`,
   `named-or-spread-args`, `function-referenced-as-value` (first-class
   callables, `call_user_func*`, callable strings, Reflection — detected
   by scanning for the name as a *value*, since these are invisible to
   call resolution), `argument-not-proven` (any call-site arg whose
   resolved fact does not admit the candidate type with
   `Certainty::Yes`), `method-inheritance`,
   `type-not-natively-representable`, `phpdoc-finer-than-native`,
   `default-not-admitted-by-native` (a non-null parameter default not
   provably admitted by the candidate type — a native hint would turn it
   into a compile-time fatal; unresolvable constant defaults refuse
   conservatively), `implicit-nullable-default` (`= null` against a
   non-nullable native — the removed implicit-nullable form). The
   default-value gates live in the *planner*: compile-time fatals emit
   no diagnostic, so the post-check is structurally not a backstop for
   them. Variadic params prove every positional arg from their index
   onward, not just the index-matching one.
   Refusals carry site + reason + human detail; the completeness oracle
   accounts every candidate as edited-or-refused.
   Two project-global caller-enumeration obstacles are added by ADR-0046 §2:
   `eval-present` (a non-vendor `eval` — an invisible caller) and
   `dynamic-include-present` (a non-vendor dynamic / out-of-universe
   `include`/`require`). While one stands unvouched, *every* candidate
   refuses with that reason, and the obstacle is recorded once with its
   site list; a `steins.toml` vouch clears the site but downgrades the
   completeness claim (carried as `vouched_exemptions`).
4. **Sibling transform (next): `phpdoc-honesty`** (ADR-0037 point 4) —
   widen a lying `@param`/`@return` to the proven union from call-site
   evidence. Enumeration domain = exactly the sites where
   `phpdoc.param-mismatch` / `phpdoc.return-mismatch` fire (437
   measured across the corpus), so impact is a number on day one. Same
   reverse sweep, same refusal taxonomy, same all-callers-proven
   precondition as promotion: widening from partial evidence moves the
   doc *toward* truth but cannot claim the verified stratum, so
   partial-evidence widening is deferred-with-design. The edit replaces
   the tag's type-text span (the docblock-offset plumbing built for tag
   deletion serves both directions).
5. **Trust stratification feedback (deferred-with-design)**: the
   all-callers proof the transform computes is precisely ADR-0037's
   "verified phpdoc" stratum. Persisting it as a queryable fact (so the
   checker can distinguish verified from asserted phpdoc without
   re-sweeping) is deliberately not done in v1 — the proof is
   revision-scoped and cheap to recompute inside a transform run, and a
   stored stratum would need invalidation semantics we have not
   designed.
