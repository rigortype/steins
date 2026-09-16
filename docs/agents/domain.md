# Domain docs

Use this guide for domain terminology, architecture, ADRs, or a rename. The
repository has one domain context, `CONTEXT.md`, with its decisions under
`docs/adr/`.

## Route the context

- Read **`CONTEXT.md`** when naming a domain concept or deciding whether a
  rename changes what the concept is called.
- Read the ADRs that touch the area being changed.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`, not a synonym the glossary lists under _Avoid_.

The glossary binds renames too: when a rename changes what a concept is called in code, update `CONTEXT.md` first. An entry that lists the new word under _Avoid_ is a decision, not a stale note; reopen it before renaming.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## ADR status

ADRs use post-hoc ratification. A merged ADR marked `Status: PENDING
ratification` is normal and does not block implementation, release, or a
follow-up amendment. List the current set with
`grep -lri 'pending ratification' docs/adr/` rather than keeping a copy of it.

## Flag ADR conflicts

If the requested change contradicts an existing ADR, name the ADR and the
conflict explicitly, then explain why the decision may need to be reopened.
