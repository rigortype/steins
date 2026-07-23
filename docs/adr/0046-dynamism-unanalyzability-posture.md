# Dynamism posture: eval, dynamic include, unserialize — unanalyzability, not nondeterminism

`eval` and `unserialize` are deterministic given their inputs; the
problem they pose is *unanalyzability* — code as data, values as data —
which is a different axis from true nondeterminism (random/time/IO,
the effect-label lane). Three havoc kinds, three answers:

1. **Scope havoc — already solved.** `Scope.poisoned` (eval, include/
   require, extract, variable-variables, by-ref capture, …) makes every
   local unknown in the containing scope: the "eval rewrote my local"
   false-positive class is structurally impossible. Coarse, sound,
   unchanged.
2. **Universe havoc — the transform-precondition gap.** An unproven
   `eval` can call any function with no CST call site; the string-value
   reference scan cannot see `'foo(42)'`. Dynamic `include`/`require`
   is the same family with one mitigation: call-site enumeration is
   per-file, so an include whose path is *proven to resolve inside the
   analyzed universe* (project + vendor) is enumeration-benign (its
   file's calls are already counted); the obstacle is an unproven path
   or a proven out-of-universe one — compiled-template caches being the
   real-world case. Rules:
   - New global obstacles for all-callers-proven: `eval-present`,
     `dynamic-include-present` — recorded once in the completeness
     oracle; every candidate refuses.
   - **Vendor presumption**: dynamic includes inside vendor/ are
     presumed universe-internal (composer autoload plumbing) — a
     rebuttable, documented soundness trade; without it every composer
     project refuses everything. Non-vendor dynamic includes are
     obstacles.
   - Definition havoc (an external include winning a
     `function_exists`-guarded definition) is already absorbed by the
     `resolution-ambiguous` refusal on conditional/duplicate
     definitions.
   - **The vouching valve**: legacy targets contain eval; refusing
     everything forever kills the transforms exactly where they matter.
     A user may vouch specific sites (steins.toml); vouched runs do not
     silently pass — the completeness oracle's claim itself downgrades
     to "conditional on N user-vouched dynamic-code exemptions",
     carried in the report and the EditPlan (ADR-0037: user assertion
     is a trust stratum, and the proof says so). Literal-eval payload
     parsing (enumerating calls inside proven eval strings) is
     deferred-with-design.
3. **Magic-method havoc — the unserialize face (Stage-5 charter).**
   `unserialize` with an unproven payload can instantiate any class and
   invoke its `__wakeup`/`__unserialize`; `__destruct`/`__toString` are
   likewise callable with no visible call site. For method transforms:
   magic methods are never promotion candidates, and unserialize
   presence is an enumeration obstacle for `__wakeup`/`__unserialize`
   of every class. Value side: the `failure.input` arm and dual-purpose
   `false` sentinel are already catalogued; *conditional foldability*
   (proven literal payload + proven `['allowed_classes' => false]` ⇒
   no magic methods possible ⇒ sidecar-foldable, deterministic) is
   deferred-with-design.

Effect side: `eval` joins the escape-hatch label family beside `ffi`;
`unserialize` is honestly incompatible with purity claims unless
allowed_classes is proven false. Labels only — no envelope diagnostics
move until boundary profiles.
