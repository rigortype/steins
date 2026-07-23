# Template scope transfer: templates as functions, render sites as call sites

A PHP template (`foo.phtml`) is a file whose top-level scope expects
variables nobody in the file assigns — a controller's
`render('foo.phtml', ['foo' => 1])` puts them there through
`extract`/`include`, which today is pure ADR-0046 scope-and-universe
havoc: the render function's body is poisoned, the template's entry is
unknown, and the checker is blind on both sides of the most common
code shape in legacy PHP. The design is one sentence: **a template is
a function whose parameters are the forwarded array's keys, and a
render call is a call to it** — the existing call-site-propagation
machinery (ADR-0001; binding descent, fact seeding) applied with the
template file's top-level scope as the callee. This is the *declared
bridge* counterpart of the ADR-0046 extract/include havoc: the havoc
stays poisoned everywhere, and a proven render edge carries facts
*around* it, never through it.

1. **The model.** A render sink is a function (or method) with a
   path-argument and a vars-argument. At a call site where the path
   argument is proven to resolve to an analyzed-universe file and the
   vars argument is a proven array shape, the engine descends into
   that file's `ScopeOwner::TopLevel` scope with an entry env seeded
   from the array: each identifier-shaped string key becomes a
   variable carrying the `singleton_fact` of its proven value —
   exactly what `descend()` does for function parameters, minus the
   coercion step, because a template declares no parameter types.
   Runtime semantics verified against PHP 8.5.8 (`php -r`, the
   ADR-0049 discipline), normative for seeding: `extract` silently
   skips non-identifier string keys, integer keys, and `GLOBALS` —
   those seed nothing; a `this` key is a fatal `Error: Cannot
   re-assign $this` in any scope — it seeds nothing, and the provable
   fatal at an extract-based sink is an adjacent deferred id.
   Entries whose *value* fails literal resolution still prove their
   *key*: the variable is defined-but-unbound in the entry env — key
   presence and value binding are separate judgments, and the
   distinction is what point 6's absence claim stands on. Forwarded
   facts ride the proven stratum (ADR-0037): they are call-site-proven
   values, not declarations. A template whose own top-level scope is
   poisoned (it contains `extract`, `include`, …) refuses seeding as
   any poisoned callee does — the bridge never lands on havoc.

2. **Registration: `[templates]` in steins.toml** (ADR-0020/0023
   conventions; a top-level section, not under `[transform]`, because
   the transfer feeds the checker and annotate, not only transforms):

   ```toml
   [[templates.render]]
   function = "render"           # or: method = "App\\View::render"
   path_arg = 0
   vars_arg = 1                  # optional: omitted = forwards nothing
   base_dir = "templates"        # optional, project-root-relative
   ```

   Array-of-tables carries multi-render-fn projects; `function` and
   `method` (FQN `Class::name`) are mutually exclusive per entry. A
   method sink matches call sites whose receiver resolution (the
   ADR-0043 machinery, all receiver forms including `$this->`) lands
   on that exact declaring method; overrides of a registered method
   are not sinks unless registered themselves — conservative, one
   entry per declaration. Validation is a config error (exit 2):
   a name resolving to no declaration in the analyzed universe
   (project + vendor-as-source, ADR-0015 — a typo'd registration
   would otherwise silently disable the transfer, the ADR-0050
   mechanics-layer rot pattern applied at config-load time),
   `path_arg == vars_arg`, an argument position past the target's
   parameter count, duplicate registration of one declaration.
   Composition with `[transform.partitions]`/`[transform.vouch]` is
   plain coexistence: no key interacts, and region assignment (point
   7) reads the ADR-0047 map unchanged.

3. **Path judgment reuses the include machinery** (ADR-0046 §2): the
   path argument lowers through the same `Literal` / `DirRelative` /
   `Unproven` judgment as `include` paths. Resolution: `Literal`
   resolves against the registration's `base_dir` when present
   (project-root-relative, matching the config glob convention),
   else as-is only when absolute; `DirRelative` resolves against the
   *calling* file's directory; a relative literal with no `base_dir`
   is **unresolved** — the engine will not guess `include_path`/cwd
   semantics, and the fix is one config line. Rejected: a project-root
   presumption for bare relative paths — a wrong presumption seeds a
   wrong file's scope with confident facts, which is worse than
   silence. An `Unproven` or unresolved path, or one resolving
   outside the universe (compiled caches), makes the site an
   enumeration obstacle in the `dynamic-include-present` family
   (point 7); it never descends.

4. **Entry state: per-site descent for evidence, exhaustive join for
   canon** (the ADR-0043/0048 reconciliation). Diagnostics come from
   per-site descents, as function binding does: each proven render
   site runs `analyze_scope` on the template's top-level scope with
   its own seeded env, memoized under a `BindingKey` of (template
   path, bound shape), sharing `MAX_BINDING_DEPTH` and the
   cycle-guard stack — two sites forwarding different shapes are two
   passes attributing findings to their own provenance ("bound at
   render(…) call at file line N"). The template's **canonical entry
   state** (ADR-0048 §3) is defined by the same formula as any
   declaration's — declared envelope refined by observed-caller
   evidence only where enumeration is exhaustive — degenerated by the
   fact that a template *declares nothing*: the envelope term is
   empty, so the canonical entry state is exactly the join of the
   enumerated render sites' forwarded facts **when the site set is
   exhaustive**, and unknown-with-`…?` otherwise (the annotate
   discipline promoted, verbatim ADR-0048). Exhaustiveness for a
   template requires: every registered render call with a path that
   *could* denote it is path-proven (any `Unproven` render path
   anywhere is an obstacle to every template, exactly as a dynamic
   include is an obstacle to every caller-enumeration); every raw
   `include`/`require` edge proven to target the template file
   counts as a render site of unproven shape (PHP include shares the
   including scope's variable table — an arbitrary entry state)
   unless that include is the interior of a registered/recognized
   render sink (its accounted mechanism, points 7–8); and every
   enumerated site is shape-proven for the *join* to claim the key
   set. Per-site descents run regardless — each is sound on its own
   binding; only completeness-gated claims (point 6) and the
   canonical join wait for exhaustiveness. `steins annotate
   <template>` surfaces the canonical entry state at the top of the
   file: joined facts when exhaustive, per-site facts under `…?`
   when not — under the tracer slice, which has no exhaustiveness
   oracle, the marker is unconditionally present.

5. **Nested renders need nothing new.** A render call inside a
   template's top-level scope is an ordinary call in a walked scope:
   during a descent pass it descends again (template → template),
   bounded by `MAX_BINDING_DEPTH` and de-cycled by the `BindingKey`
   stack — self-rendering templates terminate by construction.
   Functions/classes declared inside template files are ordinary
   index declarations already (the project index collects
   declarations at any nesting depth, ADR-0049 §1); two templates
   declaring one helper collide into `Ambiguous` as any duplicate
   does. No template-specific declaration rules exist.

6. **The absence finding: `template.undefined-variable`** (the
   accounting slice's yield). Claim: a variable read in a template's
   top-level scope that **no enumerable render site forwards** —
   verified (PHP 8.5.8): `E_WARNING "Undefined variable $x"`, reads
   `null`. Layer: **proof** (ADR-0050) — a proven `E_WARNING`
   is proof-layer reportable per the ADR-0049 §7 decision, and the
   ladder is that ADR's closed-world discipline transposed: definite
   No only under complete enumeration. All legs, or silence: (a) the
   render-site enumeration for this template is exhaustive (point 4)
   — including zero shape-unproven sites, since a key can hide in an
   unproven shape; (b) the key is absent from *every* enumerated
   site's proven key set (key presence, not value binding — point 1);
   (c) the read is not preceded by a template-local assignment on
   some path, and the top-level scope is not poisoned (its own
   `extract`/`global` could define anything); (d) the variable is not
   a superglobal. Fires once per variable per template, in the plain
   pass with the site inventory as evidence, not per descent (no
   ordering dependence, ADR-0048 §4). Message speaks PHP first,
   closure second (ADR-0049 §9 register): `undefined variable $user —
   no render site forwards it (3 sites enumerated: src/a.php:10,
   src/b.php:22, src/c.php:7); reads null with "Undefined variable
   $user"`. Zero-FP gated: opened in measurement mode first with
   verbatim corpus triage (ADR-0043 §5 discipline).

7. **Obstacles, strata, regions.** (a) An unproven render path joins
   the `dynamic-include-present` obstacle family (ADR-0046 §2) as a
   site, vouchable through the existing `[transform.vouch]` valve
   with the same claim downgrade. (b) The registration *discharges*
   the render sink's own interior dynamic include for
   universe-accounting exactly when every call site's path argument
   is proven in-universe (the ADR-0046 rule "a proven in-universe
   include is enumeration-benign", reached through the sink's
   argument instead of its body). Stratum honesty: for a merely
   *registered* sink the claim "the interior include receives only
   the path argument" is user-asserted — the discharge rides the
   user-assertion stratum and completeness claims say "conditional on
   N template registrations" (ADR-0037/0046 valve pattern); for a
   *recognized* sink (point 8) the body is engine-verified and the
   discharge is proven. Registration wins on shape; recognition wins
   on stratum; both can hold at once. (c) ADR-0047: a proven render
   edge is a cross-region *reference edge* in the §3a verified scan —
   a partition-A render call resolving to a partition-B template
   demotes the template's file to S like any foreign reference;
   observer render edges demote nothing; a dynamic render path in
   region R is an obstacle scoped by nameability(R), verbatim the
   include rule. Templates have no special region kind — they are
   files in whatever region claims their path.

8. **Extract-idiom recognition** (the recognition slice): a project
   function whose body is provably the forwarding idiom is a render
   sink with no configuration — inference, not registration, and
   engine-verified (point 7b). Accepted shape: optional benign
   prefix/suffix (statements that neither write the two parameters
   nor branch around the core), then `extract($vars)` of a plain
   parameter with **no** mode/prefix arguments (the `EXTR_*` modes
   change binding semantics), then an unconditional
   `include`/`require` whose path expression is the path parameter
   under the point-3 judgment forms — `include $path`, `include
   <literal> . $path`, `include __DIR__ . <literal> . $path` (the
   concatenation prefix *proves* the effective `base_dir`, closing
   point 3's relative-path gap for hand-rolled sinks). Each rejected
   variant — extract of a non-parameter, any extract mode argument, a
   conditional include, a path expression outside the proven forms —
   refuses recognition with a fixture per variant (the ADR-0049 §10
   silence-matrix discipline). Precedence: an explicit `[templates]`
   entry for the same declaration wins outright on conflict (declared
   intent beats inference; the disagreement is reported). Poisoning
   interplay, precisely: the recognized function's own scope **stays
   poisoned** — nothing is un-poisoned, ever; the transfer is a
   separate proven edge from call site to template that bypasses the
   poisoned body, and where the shape or path is unproven at a site,
   that site simply has no edge and the blanket poisoning is the
   whole story, unchanged.

9. **Slices** (each boundary under the full verification protocol;
   unregistered projects byte-identical at every stage). **Tracer**:
   registration parsing/validation (function sinks; method sinks are
   designed here and may land with or immediately after), `Literal` +
   `base_dir` and `DirRelative` resolution, single-site seeding
   through the existing descent, diagnostics and annotate inside the
   template (annotate under the unconditional `…?` of point 4), no
   obstacle bookkeeping, no new finding ids. **Accounting**: the
   render-site enumeration query, the exhaustiveness oracle and
   obstacle wiring (point 7a–b), the canonical entry-state join,
   `template.undefined-variable` under measurement-mode opening.
   **Recognition**: point 8. **Engine spike** (design, separate ADR):
   compiled engines (Smarty/Blade) — this ADR's plain-PHP semantics
   is the substrate; the presumed shape is per-engine packs
   (ADR-0044 umbrella) mapping engine render APIs to sinks and
   compiled caches back to sources, with caches remaining the
   canonical out-of-universe include until that ADR says otherwise.

10. **Refusals** (one line each): treating `extract($x); include $y;`
    as generally transparent — poisoning stays; only the proven idiom
    bridges (ADR-0046 §1). Name-heuristic sink detection ("functions
    called render") — recognition is body-proof or nothing; a
    heuristic seeds wrong facts confidently. A `templates_dir` scan
    declaring every `.phtml` a template — files gain entry states
    only from proven edges, not extensions. Worst-case
    maybe-reporting ("possibly undefined under non-exhaustive
    sites") — the ADR-0002 anchor, verbatim. Guessing relative-path
    resolution (point 3). Any template-specific syntax layer —
    `?>`HTML`<?php` is ordinary PHP; non-PHP template syntax is the
    engine spike's problem.

11. **Deferred-with-design**: template-local declared surfaces — a
    `@var` docblock in a template is the conventional
    declared-parameter idiom and would restore the empty envelope
    term of point 4's formula (declared, refined by exhaustive
    callers), the natural contract-layer complement (`phpdoc.*`)
    once the phpdoc lane meets templates; `template.missing-file` (a
    proven path resolving to no file inside a claimed `base_dir` — a
    provable runtime include failure) — one slice behind the
    accounting oracle; transforms targeting template interiors —
    render-site exhaustiveness *is* all-callers-proven transposed,
    so the completeness-oracle rows extend without new concepts when
    a transform ever wants a template (nothing here paints it out,
    ADR-0034/0041); vars-arg spreads and `compact()` shapes at render
    sites (today `Other` → shape-unproven, correctly conservative);
    engine packs (point 9).
