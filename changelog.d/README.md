# Changelog fragments

`[Unreleased]` entries land here as one file per change, instead of as edits to
`CHANGELOG.md`. GitHub ignores `.gitattributes merge=union` when it decides
whether a PR can merge, so two PRs that each append under `[Unreleased]` still
conflict on the same anchor and have to land one after the other. A fragment is
a new file, so independent PRs never touch the same lines. The scheme follows
rigortype/rigor's ADR-105.

- **Path**: `changelog.d/<section>/<slug>.md`, where `<section>` is one of
  `added`, `changed`, `deprecated`, `removed`, `fixed`, `security` (Keep a
  Changelog 1.1.0's headings, lowercased), and `<slug>` is lowercase
  `[a-z0-9._-]`, usually the branch name. Create the section directory if it
  does not exist yet.
- **Content**: one entry in the grammar `CHANGELOG.md`'s header comment
  describes. A bullet opening with a bold one-sentence summary,
  `- **Summary sentence.** Detail.`, on **one** physical line however long,
  optionally followed by `  - ` child items, each also one line. Write it for
  someone deciding whether to upgrade, and say plainly when findings can appear
  or disappear.
- **Only release prep writes `[Unreleased]`.** The `steins-release-prep` skill
  moves every fragment under the matching `###` heading at the cut and deletes
  the files, so this directory holds only this README right after a release.
- **Gate**: `cargo test -p xtask changelog` (`xtask/src/changelog.rs`) checks
  every fragment's path and grammar, and still checks `CHANGELOG.md` for a
  repeated heading.
