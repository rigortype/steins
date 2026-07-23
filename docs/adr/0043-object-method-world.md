# Object/method world: native object acceptance over a trinary is-a oracle

Motivation, measured: 100% of the legacy monorepo's 405 phpdoc.* sites are method
calls (Transform #2 measurement), and 9 of the 18 triaged conformance
fails share one root cause — non-scalar types lower to `None` and are
never checked. Method call-site *resolution* already exists (all six
receiver forms, chain walk, final/private override guard, arg check,
descent); this milestone gives it types to check.

1. **Object types join `NativeType`**, not a parallel type:
   `TypeMember::Instance(fqn)` (namespace-resolved at lowering) sits
   beside the scalars, so `Foo|null`, `Foo|false`, `A|B` are one union
   shape and ADR-0042 failure arms extend to object-returning builtins
   unchanged. `self`/`static`/`parent` remain unlowered (silent) —
   late-static-binding is not v1.
2. **Enums become class-likes**: `ClassDecl { is_enum }` with cases as
   members; `ArgValue::EnumCase(class, case)` (an object value of the
   enum class) and `ArgValue::ClassConst(class, konst)` replace the
   `Other` erasure. Class-const *value* resolution imports only literal
   initializers in v1; anything else stays unproven.
3. **The is-a oracle is trinary** — Yes / No / Unknown — and **No
   requires a completely enumerated hierarchy**: every ancestor edge
   (parent chain + transitive `implements`) resolved inside the project
   or found in the catalog's builtin hierarchy, and the target absent
   from that closed set. A chain that leaves the project into an
   uncatalogued builtin, an ambiguous class, or a trait-using region
   yields Unknown (silent). This is the Certainty discipline applied to
   subtyping: membership is provable, non-membership only under closure.
   `instanceof` evaluation and catch matching move onto the same oracle;
   the existing exact-class heap (ADR-0036) supplies the value's class,
   and instanceof still binds no exactness fact (membership is not
   exactness).
4. **Acceptance**: `is_type_error` gains a definite-No arm — object
   value whose exact class is-a-No against every union member. The
   phpdoc side opens `contract_touches_class` on the same oracle. The
   four-layer domain stays object-free (ADR-0035/0038 extensionality);
   objects ride the CVal/heap path, as today.
5. **Gate discipline for the opening**: object acceptance can surface
   *true* positives on the corpus where the expectation is zero
   proof-layer diagnostics. Every new corpus finding is triaged verbatim
   (5-sample minimum per class); false positives are fixes, true
   positives are reported to the orchestrator with the triage before
   any expectation-table change. Silence-by-default (Unknown) is the
   safety valve at every stage boundary.
6. **Transforms unlock last**: method sweeps key on `Sym::Method`, the
   eligibility split reuses the `final_or_private` predicate
   (promotable) vs everything else (`method-inheritance` refusal per
   ADR-0041), and the legacy monorepo's 405-site re-measurement is the milestone's
   closing number.

Staging (each boundary gate-verified): (1) lowering only — enums,
`Instance` members, new `ArgValue` forms, zero behavior change; (2) the
is-a oracle + instanceof/catch migration; (3) native acceptance
definite-No + corpus triage; (4) phpdoc-side class contracts (tripwire
management); (5) transforms to methods + re-measurement.
