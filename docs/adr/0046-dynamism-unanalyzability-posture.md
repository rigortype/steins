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

## Amendment (2026-09-26): `eval` is a label that moves envelope diagnostics, and an inclusion reads a file

**Status: owner ruling (2026-09-26).** The ruling decides the three rules
below and where `eval` sits in the taxonomy, with the owner's reasoning for
that placement recorded under the headings after the rules; the wiring beside
them follows from them.

The last paragraph above promised `eval` a label and deferred every envelope
diagnostic "until boundary profiles". Neither half happened: no label was
registered, and the effect-origin scan had no arm for `eval` or for any of the
four inclusion constructs. The only consumers that saw them were scope
poisoning (§1), the per-scope opaque sites, and the file-wide dam (§2). So a
body holding either was summarized `effects: {}` — exhaustive and effect-free,
which is exactly what a proven-pure body earns — and `#[\Steins\Pure]` or
`@pure` over `return eval($s);` was silent at every profile. Deferral had
turned into a claim of purity.

1. **`eval` is a core root label, beside `ffi`.** Every `eval(...)` is a
   proven `eval` finding at its span, and it **does** move envelope
   diagnostics: a pure envelope over an `eval` earns `effect.envelope-exceeded`
   on the same surface `echo` does (the contract layer). `eval` is its own
   root, not a child of `nondet` or `io`
   ([why](#why-a-root-and-not-nondeteval)), so neither envelope admits it;
   `#[\Steins\Effect('eval')]` or `@phpstan-impure eval` does. As a core root
   it belongs to Steins under ADR-0068 §2: a plugin may refine it, never claim
   it.

   The payload is not inspected, and a literal one earns no exception. The
   owner's reasoning: even a payload whose every arm is a pure literal, as in
   `eval($c ? 'return 1;' : 'return 2;')`, is expressible directly as
   `$c ? 1 : 2`, and an interpolated one like `eval("return {$n};")` has
   essentially no justification in modern PHP. Treating `eval` itself as the
   effect is therefore not a false positive worth calibrating for. A project
   that disagrees has the ADR-0084 tolerated-effects policy.

2. **An inclusion is a proven `io.fs.read`.** `include`, `include_once`,
   `require` and `require_once` read a file whatever the file holds, so each is
   an `io.fs.read` finding at its span, named by its own keyword in the
   message. A path proven in-universe changes what the dam says (§2), not
   what the effect lane says.

3. **Both are non-exhaustive.** The evaluated string and the included file's
   top-level code run in this frame, and the scan sees neither, so each
   construct also marks the body `…?`, as an unresolvable call does. The
   summary reads `effects: {eval, …?}` or `effects: {io.fs.read, …?}`: the
   construct's own effect is proven, and what the code it runs does is not.

### The escape-hatch family, and who is in it

A label joins the escape-hatch family when the code it stands for runs
**outside the analyzed universe**. There are two ways to get there: native code,
which is `ffi`, and code passed as data, which is `eval`. Everything else that
runs unseen code runs code *inside* the universe: an unresolved dynamic call,
the magic methods `unserialize` can reach (§3), and the code of an included
file. Those stay what they were, non-exhaustive (`…?`) with no label, because
the code exists as PHP source and the gap is only that the scan could not
connect the call to it. That holds even for §2's out-of-universe include, the
compiled-template cache: the file is source a wider run could analyze, so it is
a gap in this run's universe rather than code of another kind, and the dam and
the `…?` already answer for it. An inclusion's own label is the file read
(rule 2), not the code it runs. In modern PHP the family is closed at these
two, because the other ways to run a string as code are gone:
`create_function()`, string `assert()` and `preg_replace()`'s `/e` modifier have
all been removed.

What a consumer needs from the family is one **property**: an escape-hatch
label is ⊤ to any consumer asking what a call could touch. A statement carrying
one is never a dead statement, and crossing one invalidates everything
remembered. A consumer that scopes its answer by label (ADR-0096's discardable
set, label-scoped forgetting of remembered values) has to treat `eval` and
`ffi` as reaching every label, not as the one label they are.

### Why a root, and not `nondet.eval`

Three reasons, each sufficient on its own:

1. **This ADR's own thesis.** `eval` is deterministic given its input; the
   problem it poses is unanalyzability, not nondeterminism. Filing it under
   `nondet` would contradict the first paragraph of this ADR.
2. **Prefix subsumption would admit too much.** Under ADR-0018 a declared
   `nondet` admits every `nondet.*`, so a `nondet` envelope would silently
   admit an `eval` whose payload can `exit` or write files.
3. **Read-shaped consumers would get it backwards.** Consumers that classify
   labels as read-shaped (`nondet.*`, `global.read`, `io.fs.read`) include the
   no-effect statement rule and label-scoped forgetting of remembered values.
   Under `nondet.eval` they would read a bare `eval($code);` as a dead
   statement, and would keep what they remember across it.

`io.eval` fails reason 2 in the same way: an `io` envelope would admit it.

Reason 3 is also why an inclusion's `io.fs.read` needs the `…?` of rule 3.
`io.fs.read` alone is read-shaped, and the `…?` beside it is what stops a
consumer from treating a bare `include $p;` as dead.

### Why not an `escape.*` parent

A shared parent (`escape.eval`, `escape.ffi`) was considered at a moment when
it would have cost no compatibility at all: Steins is in its v0.1.x preview,
Rigor is not yet announced, and the interop proposal
([phpdoc-effects-interop.md](../type-specification/phpdoc-effects-interop.md))
has not been taken upstream. It was declined anyway. The membership is closed
at two, so a parent would name a group that never grows. And what consumers
need from the family is a property, not a prefix: the ⊤ property stated above,
which this amendment states directly rather than encoding it in a spelling.

### Sibling alignment

The sibling analyzer Rigor aligns with this in rigortype/rigor#1431: string
`eval` produces the shared `eval` label together with a taint, and `ffi` gains
a taint as well.

### Consequences

The origins belong to the function-like that lexically contains them, like
every other effect origin: an `eval` inside a closure is the closure's, and
reaches the enclosing function only through a call to that closure.

`statement.no-effect` is unaffected, and cannot fire on a bare `eval(...);` or
`include $p;`: that judgment reads only statically-named function calls
(ADR-0096 §5), and neither construct is one.

The lowered effect origins are persisted in frozen generations (ADR-0092), so
the two new origin variants come with a schema bump: an artifact written before
them records a body holding `eval` as effect-free, and replaying it would answer
the old `{}`.
