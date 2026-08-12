# Output is an ambient channel under `io`, split by ob_start-capturability

Issue #316. Status: **accepted 2026-08-12, owner-decided.** **Amends ADR-0008
and ADR-0018** — the `output` root of the initial lattice is retired and its
meaning re-hung under `io`; the taxonomy's shape rules (dot paths, prefix
subsumption, an open registry) are untouched. Provenance: the change is the
Steins half of decision **D-V2** in the PHPStan-side design notes
(`phpstan-notes/generated-report/20260812-effect-extension-api-design.md`
§5.11), with the registry diff and label meanings written up as
`20260812-steins-vocab-sync-proposal.md`. The owner's ruling that made it
possible: Steins is early enough that the shared vocabulary can still move,
and this is the moment to move it.

## Problem

ADR-0008 gave `output` its own root, beside `io`. That looked right when the
labels were a short flat list: writing to the response is not "opening a
resource," so it got its own name.

Porting the same taxonomy into a PHPStan extension put it under load, and it
buckled in a specific, repeatable way. The catalog audit (upstream stage 9)
kept producing rows that named two roots for one operation:

```
fwrite    = io, output
system    = io.process, output
readfile  = io, output
```

Every one of those pairs is the same symptom: a stream's *destination* can be
the script's own output, so an operation that is plainly one thing has to be
spelled as two unrelated roots. The hierarchy was fighting the language.

Two smaller holes sat beside it. `php://input` was being colored
`global.read`, which is what `$_GET` gets — but `$_GET` is parsed memory and
`php://input` is a stream, and calling them the same effect loses the
distinction that matters. And nothing in the vocabulary marked the one
property a future `ob_start()`-aware analysis has to know about a piece of
output: whether the output buffer can capture it at all.

## Decision

Retire the `output` and `output.header` roots. Output becomes an **ambient
channel under `io`**, and gains a symmetric input sibling:

```
io.output          the umbrella: "writes to the script's ambient output channel"
io.output.buffer   OB-layer, ob_start()-capturable: echo, print, printf,
                   inline HTML, php://output, flush, ob_flush
io.output.stdout   process-fd direct, outside OB's reach: php://stdout,
                   fwrite(STDOUT, …)
io.output.stderr   the same: php://stderr, STDERR
io.output.header   response metadata, not OB-subject: header(), setcookie()
                   (the old output.header)
io.input           ambient input: php://input, php://stdin
```

`core_roots()` loses `output`; the interop v1 vocabulary goes from 21 to 25
labels (the `failure.*` exclusion is unchanged). No predicate changes:
`subsumes`, `is_known_label`, `nearest_label` and `LabelRegistry` are prefix
machinery and carry the new shape as they carried the old.

**The organizing principle** is the *provenance of the channel*, not whether
a file descriptor was opened by hand. `io`'s children are the resources a
script opens (`io.fs`, `io.net`, `io.db`, `io.ipc`) **and** the ambient
channels it is born holding (`io.output`, `io.input`). Both are the program
talking to the world outside its own memory, which is the only thing `io` has
ever claimed to mean. Koka's `io` — console included — is the same umbrella.

**The internal split is the masking boundary.** `io.output`'s children divide
on one question, and only that question: *can `ob_start()` capture this?*
That is not taxonomic tidiness, it is preparation. The future effect masking
(an `ob_start()` region analysis, or the HOF annotation
`@phpstan-masks io.output.buffer $fn`) needs a rule for what may be deducted
from a callee's effect set, and putting the answer in the hierarchy reduces
that rule to a single prefix test: **only labels subsumed by
`io.output.buffer` are ever deductible.** `fwrite(STDOUT, $x)` cannot be
captured by an output buffer, and now the label says so — no table of
exceptions has to remember it.

**Split evidence takes the parent.** Where it is not settled whether OB
captures an operation's output — `system`, `passthru`, and `curl_exec`'s
response echo — the catalog row carries the parent `io.output`, never
`.buffer`. Over-approximating toward "cannot be masked" is the sound side of
a masking rule: the worst a parent label costs is a mask that does not fire,
whereas a wrong `.buffer` would silently delete a real effect. `readfile` and
`fpassthru` do take `.buffer`, on the strength of the manual's documented
`ob_start()` + `readfile()` capture pattern.

**`io.input` is reserved with no rows.** Steins has no stream-target
awareness, so no builtin can yet be colored with it — recognizing
`fopen('php://input')` needs the argument analysis that
`fwrite(STDOUT, …)` narrowing also waits on. The label is registered anyway,
for the same reason `ffi` is: a declaration may name it today, and the rows
that will carry it should not require a registry change to land. `$_GET` and
friends stay `global.read`; they are parsed memory, not a stream.

**Inline HTML becomes an origin.** ADR-0008 always listed raw text between
`?>` and `<?php` as output; the scan never implemented it. It does now, as
`io.output.buffer` with the keyword `inline HTML`. Whitespace-only inline
text produces nothing — the newline and indentation between two tag pairs are
source layout, and coloring them would make a function's effect set depend on
how its template is formatted.

### The one deliberate meaning change

**A bare `io` envelope now admits output.** `io.output.buffer ⊑ io`, so
`#[\Steins\Effect('io')]` on an echoing function is silent where it used to be
a finding. This is intended.

It is honest, first: after the stage-9 sweep, bare `io` is what a row says
when the destination of some stream work is unknown, and "the destination
might be stdout" is precisely the case it is covering. It is also narrow —
only bare `io` loses edge. Every fine-grained envelope keeps it, because
`io.db` does not subsume `io.output.buffer`; a repository declared `io.db`
that starts echoing is still caught, which is the case anyone actually writes
the annotation for.

"Does io, but does not output" remains expressible: enumerate the children,
or use the reserved `-except` form (`io -except io.output`). The migration
hands `-except` its first concrete motivation, noted on issue #312.

### Compatibility

- **Breaking, for the attribute spelling.** `#[\Steins\Effect('output')]`
  is now `effect.unknown-label` — mechanics layer, every profile, no
  suppression channel. And there is no automatic rescue: `output` →
  `io.output` is Levenshtein 3, past the suggestion cap, so the finding
  carries no "did you mean". Migration guidance is the docs' job, not the
  diagnostic's. v0.1.x preview versioning applies.
- **Inert, for the interop spelling.** `@phpstan-impure output` degrades to
  an unspecified tag under the ADR-0082 amendment: unknown label ⇒ the whole
  tag reads ⊤, no bound arrives, no finding is invented. A codebase in the
  middle of migrating is safe, which is exactly the migration job that
  amendment was written to do.
- **Byte-untouched, for the transform.** `steins transform effects-envelope`
  refuses a docblock carrying the old spelling as `existing-tag-unreadable`
  and writes nothing; its emission side spells the new vocabulary.
- The interop spec's backward-compatibility argument (current
  `phpstan/phpdoc-parser` sees a `GenericTagValueNode` and discards the
  parameter) is about the tag's shape, not its vocabulary, and stands.

### Catalog rows changed and added

Renamed: `print_r`/`var_dump`/`var_export`/`printf`/`vprintf` →
`io.output.buffer`; the `header`/`header_remove`/`setcookie`/`setrawcookie`/
`http_response_code` family → `io.output.header`; `session_start`'s composite
row → `io.fs.write, io.output.header, global.write`.

Added, closing a false-negative gap that predates this ADR (the upstream
stage-9 audit found the same hole): `readfile` and `fpassthru` →
`io.fs.read, io.output.buffer`; `system` and `passthru` →
`io.process, io.output`; `curl_exec` → `io.net, io.output` (its failure-arm
row is a separate table and is unchanged); `flush` and `ob_flush` →
`io.output.buffer`. Before these rows, a body whose only statement was
`readfile($p)` carried no output component at all.

## Considered and rejected

**Keep `output` as a root and live with the pairs.** This was the original
recommendation, on the grounds that moving output under `io` blunts the `io`
envelope. The concern is real and is answered structurally rather than by
refusal: the blunting is confined to bare `io`, the children stay sharp, and
`-except` covers the residue. Refusing the move would have kept the
two-roots-for-one-operation spelling permanently, in both implementations.

**`output` as a root *with* an ob-capturability split.** Fixes the masking
boundary without fixing the awkward pairs, and leaves the two implementations
disagreeing about where output lives. Half the change for the same breakage.

**Split by mechanism instead (`io.output.echo` / `.write` / …).** Names the
implementation rather than the property anything downstream needs. Masking
does not care whether the bytes came from `echo` or `printf`; it cares
whether the buffer can see them.

**Color `fwrite(STDOUT, …)` as `io.output.stdout` now.** The narrowing is
only a syntactic check on a `STDOUT`/`STDERR` `ConstFetch` argument, but
`effect_labels` is a name→labels table with no argument awareness, so it
needs a real seam. Deferred rather than faked; `fwrite` stays `io.fs.write`.

## Consequences

- ADR-0008 and ADR-0018 gain amended-by pointers. Their bodies stand: this
  changes which labels exist, not what a label *is*.
- The `EffectOrigin::Output` variant name is unchanged — it names the
  construct family, not the label, and the label it produces moved beneath it.
- Every fixture premised on "an `io` envelope catches `echo`" reverses to
  silence. The principle those fixtures existed to pin is re-pinned with a
  non-subsuming pair (`io.db` + `echo`), and the reversal itself is pinned
  from both sides so it can never regress silently into a bug report.
- **Deferred:** `fwrite`/`file_put_contents` destination narrowing to
  `io.output.stdout`/`.stderr`; catalog rows for the `ob_start` family
  (unknown-effect widening is the sound default until masking exists, so
  `ob_start`/`ob_get_clean` stay uncatalogued); and effect masking itself,
  which is what the `.buffer` leaf was built to make cheap.
- Issue #312's reserved `-except` form gains its first concrete motivation:
  `io -except io.output` is now the only compact spelling of "stream work,
  no output".

## Amendment (2026-08-13): retirement is a table the tool reads, not only prose

This ADR's compatibility note ended by assigning migration guidance to the
documentation: `output` → `io.output` is three edits apart, past the
suggestion cap, so a stale spelling earns a bare "unknown label" and the
reader is sent to the release notes. That was the honest description of a
mechanism we had; it was not a good place to leave a migration.

**The decision.** A retired spelling is recorded in a table beside the label
registry — the spelling plus the guidance for what to write instead — and
both consumers read it: the attribute stratum's `effect.unknown-label`
message, and the interop stratum's new `effect.interop-unknown-label`
(issue #311). A retirement therefore names its replacement at the
declaration that carries it, which is where the person who must act is
looking. `output` and `output.header` are the table's first two rows, and
the table carries an append-on-move contract: when a later ADR moves or
retires a node, it adds the row in the same change.

**Why a table rather than widening the edit distance.** Raising the
Levenshtein cap to reach `output` → `io.output` would make every distant
word a suggestion candidate and would start guessing at prose — the exact
failure the ADR-0082 amendment refuses. Retirement is not a spelling
accident to be inferred; it is a fact this project knows and can simply
state. The two mechanisms stay separate: distance answers "did you mistype
a live label", the table answers "did you write a label we removed".

This supersedes the "migration guidance is the docs' job" sentence above for
retired *labels*. The release notes still carry the narrative; they are no
longer the only place a user can learn what to write.
