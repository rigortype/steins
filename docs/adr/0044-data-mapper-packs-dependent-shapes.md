# Data-mapper support: dependent shapes, witness refs, mapper returns as runtime truth

Target libraries (investigated 2026-07-23, reports pinned in session
records): azjezz/psl 6.x (PHP 8.4+), Crell/Serde 1.0-dev (PHP ~8.2),
CuyZ/Valinor 2.5 (PHP 8.2+). All three share one shape the modular
world needs extensions for and propagation gets natively: **the return
type is a function of proven argument values.**

1. **Mechanism, not per-library hacks — dependent shapes**: a catalog/
   pack entry may declare a return type *computed* from proven values at
   the call site. Three computation kinds cover all three libraries:
   - *type-string*: a proven literal string / `::class` value parsed
     with the ADR-0029 grammar becomes the return type (Valinor
     `TreeMapper::map`; Valinor-only syntax — enum/const wildcards,
     sealed-shape spread — degrades to Unknown/mixed, never to a wrong
     type; Valinor's own QA extensions chose host-parser delegation and
     accept the same degradation).
   - *class-string*: proven class-string value → `Instance(fqn)` (Serde
     `deserialize(to:)`; requires reading proven values from named
     args — the enumeration-side named-arg support is a precondition).
   - *witness*: a constructed object carries a computed type through
     the heap — the ADR-0033 closure-as-value pattern applied to
     `TypeInterface<T>` (PSL): the factory call computes T (incl.
     synthesizing keyed shapes from `shape()`'s literal array with
     `optional`/`nullish` wrappers), a witness ref stores it, and
     `assert`/`coerce` return it, `matches` narrows as a guard. No
     template solver (ADR-0032 discipline).
2. **Trust placement (ADR-0037 amendment-in-spirit)**: these mappers
   *enforce* their types at runtime (MappingError / Assert- and
   CoercionException / TypeMismatch on strict fields). Their returns
   therefore enter the trust order as **runtime-enforced truth** —
   native-declaration tier, above verified phpdoc — not as assertions.
   Data mappers are ask-the-real-thing for input data; that is why they
   deserve first-class support.
3. **Packaging — official packs, in-tree**: ADR-0039's v1 plugin wire
   format (static phpdoc declarations + labels) cannot express
   dependent shapes, and its v1 ban on plugin diagnostics is a ban on
   *third-party* finding quality — not on in-tree code. So all three
   ship as in-tree packs (catalog fragments + the small computation
   hooks), activated by composer.lock detection, versioned against the
   library ranges above. Plugin-API v2 may later externalize the
   declarative parts; the computation kinds stay core.
4. **Per-library beachheads** (value order): Valinor type-string
   returns (parser reuse, biggest precision jump from `mixed`); Serde
   class-string returns (+ the fact that deserialize NEVER calls the
   constructor — initialization reasoning must not assume ctor
   invariants); PSL witness refs (richest, needs the witness-ref
   plumbing; also `Psl\invariant()` as a narrowing assertion and the
   `Psl\Exception` envelope for throw accounting). Mapping-soundness
   diagnostics (Serde strict-field vs proven source shape, Valinor
   generic arity / unparseable-literal signatures, PSL coercion tables)
   are later stages under the zero-FP banner.
5. **Preconditions**: object/method world (ADR-0043) through its method
   stages — all entry points are methods; named-arg proven-value
   reading; int-range rendering for PSL's sized ints. Sequenced after
   ADR-0043 completes.
