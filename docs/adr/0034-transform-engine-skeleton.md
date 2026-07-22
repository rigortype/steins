# Transform engine: EditPlan transactions, code preconditions, dual verification

1. **`EditPlan`** — an atomic transaction of non-overlapping
   `(file, span, replacement)` edits plus new-file creations; JSON-
   serializable as the currency of the dry-run → diff → approve loop
   (MCP, ADR-0010/0024). Built on span+splice (ADR-0003), so untouched
   regions stay byte-identical by construction; overlaps are rejected at
   planning time.
2. **Preconditions are code, not declarations**: `Transform::plan(&ctx) ->
   Result<EditPlan, Refusal>`, where `ctx` exposes the salsa query surface
   (values, types, effects, resolution) — "is this callback provably
   pure?" is asked of the inference engine directly. **`Refusal` is
   first-class**: refusals carry named reasons ("callback purity
   unprovable: effects {io, …?}") — the Certainty discipline applied to
   rewriting; agents read refusals and continue the conversation.
3. **Dual verification after apply**: (a) post-check — the edited project
   is re-parsed and re-analyzed, and *zero new diagnostics* is the
   transform's safety net (the fp-gate discipline transposed to
   rewriting); (b) the completeness oracle accounts every enumerated site
   as transformed-or-refused — nothing silently dropped (consult-rector
   inheritance, ADR-0010).
4. **First transform: phpdoc→native promotion.** Where `@param int $x`
   exists and call-site propagation proves every project call site flows
   `int`, add the native declaration (and drop the now-redundant tag).
   Chosen because the precondition — *all callers proven* — is
   structurally unavailable to modular tools (PHPStan, Rector), it is the
   annotation-restraint story in executable form, it serves monorepo
   modernization directly, and its small EditPlans keep the safety net
   easy to trust. DTO promotion and stringly→enum follow.
