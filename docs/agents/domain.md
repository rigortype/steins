# Domain docs

Use this guide for domain terminology, architecture, ADRs, or a rename.

## Route the context

- Read **`CONTEXT.md`** when naming a domain concept or deciding whether a
  rename changes what the concept is called.
- Read only the ADRs that touch the area being changed.

If a referenced file doesn't exist, **proceed silently**. Create `CONTEXT.md` or
an ADR only when a term or decision actually needs to be recorded.

The repository has one domain context at `CONTEXT.md` and its decisions under
`docs/adr/`; there are no nested context files.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## ADR status

ADRs use post-hoc ratification. A merged ADR marked `Status: PENDING
ratification` is normal and does not block implementation, release, or a
follow-up amendment. To list the current set, run:

```sh
grep -lri 'pending ratification' docs/adr/
```

## Flag ADR conflicts

If the requested change contradicts an existing ADR, name the ADR and the
conflict explicitly, then explain why the decision may need to be reopened.
