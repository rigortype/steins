# Triage: a report over the check stream, never a second surface

**Status: proposed (2026-09-12), PENDING ratification.** Drafted alongside
the first `steins triage` slice (issue #50), in post-hoc-ratification mode
(ADR-0077 precedent). ADR-0022 deferred "a `triage`-like surface" until
baseline use existed; ADR-0062 A-G10 promised that general offset reads
"are counted in triage (#50) buckets from day one". This ADR gives the
command its shape and rules on the three questions the slice raised: what
the command may read, what it may not change, and what it says about a
bucket the input cannot fill.

## 1. Context: zero-FP is a calibration, and calibration wants data

The default surface must not be stricter than a project's code reality
(ADR-0002, ADR-0050's lenient-default principle). The consequence is a set
of named stricter surfaces — `contracts`, `strict`, `throws-direct`,
`pedantic` — and a per-project judgment about whether to enable one. That
judgment has been made blind: a team raises `[check] profile`, watches the
count, and either keeps the stage or backs out. The baseline makes the
raise survivable (ADR-0050 §8) but says nothing about whether it was
wise; `check` reports findings one line at a time and never says "these
forty are one id in one file".

Rigor's `rigor triage` (its ADR-23) is the model: pure aggregation over
the checker's own diagnostic stream, rendering separated from the data,
no second analysis pass. Steins adds one part Rigor has no need for. The
offset family is split across two layers and three rungs by ADR-0062
A-G10 — `offset.missing` proven on `default`, `offset.undeclared` on
`contracts`, `offset.maybe-missing` on `strict` — and the question a
project asks before enabling `strict` is exactly "how many optional-key
reads are unguarded here, and would `strict`'s findings be false
positives for us?" The discharge ladder (S4 guards, S5 covers) answers it
one read at a time; nothing sums the answer.

## 2. Decision: aggregation over the JSON `check` already emits

`steins triage` reads a `check --format json` document and prints a
report. That is the whole dependency:

1. **The input is the stream, and only the stream.** The command parses
   the document ADR-0050 §2/§7 already fixed — `findings[]` with `id`,
   `layer`, `level`, `path`, and the run-level `profile`,
   `vendor_suppressed`, `suppressed`, `baselined` — and nothing else. It
   names no analyzer type, imports nothing from `steins-infer`, and asks
   the analyzer no question. The one place the registry is named is a
   unit test that pins the three id spellings triage writes by hand
   (`offset.missing`, `offset.maybe-missing`, `strict`) to their
   registry constants, so a rename fails a test instead of quietly
   emptying a bucket; the command itself never reads it. A field the
   stream lacks is a fact triage does not have (§4), not a reason to
   reach past the schema.
2. **Given paths, it runs `check` itself — once, as a child process.**
   `steins triage <paths>` starts this binary's own `check --format json`
   over the same paths with the same flags (`--profile`, `--no-php`,
   `--no-cache`, `--vendor-diagnostics`, `--ignore-baseline`,
   `--baseline`, `--no-tolerated-effects`) and aggregates its stdout.
   One analysis pass, the one the user would have run; `check`'s code is
   untouched and its stderr passes through. An in-process call was
   refused: it would have needed `check`'s report *before* rendering,
   which is `steins_infer::Diagnostic`, which is the coupling point 1
   forbids. The process boundary is the schema boundary made physical.
3. **Data model first, renderers second.** Aggregation builds one serde
   struct tree — `source`, `summary`, `distribution`, `hotspots`,
   `offset`, `hints` — and `text` and `json` spell that value. `--format
   json` is the tree serialized verbatim, with no field a text reader
   cannot see and none the JSON consumer must infer. The hotspot cap
   (`--top`, default 10) lives in the data model (`limit`,
   `files_with_findings`) so the JSON says what it hid.
4. **A measurement, never a gate.** `triage` exits `0` on every successful
   run, whatever the counts. Exit `2` is reserved for its own usage
   errors and for a `check` that exited `2` (usage or config), forwarded.
   A `check` that exited `1` produced a stream with findings in it — the
   normal case for a what-if — and is a success here. Hints are advice:
   they name no line, carry no level, and never touch the exit.

## 3. The what-if is a property of the split, not a feature

A not-yet-enabled surface is measured with `steins triage --profile strict
<paths>`. This changes nothing about `check`: the flag is `check`'s own
`--profile`, the surface machinery is ADR-0050 §5 unchanged, and a project
whose `steins.toml` says `default` keeps a `default` CI. The profile is
resolved by `check` (a config error is `check`'s exit `2`, forwarded), so
triage cannot invent a surface `check` would refuse — ADR-0050 §10's
refusal of unnamed surfaces holds through this command too.

The adoption flow this produces is **measure → judge → enable →
baseline**: triage the target stage, read the distribution and hotspots
for the systemic-vs-localised split, fix what is localised, enable the
stage in `steins.toml`, and capture the rest with `--set-baseline`. The
user manual carries the flow; this ADR records that its first step is a
run that leaves `check` alone.

## 4. Guard-status buckets: measured or honestly not

The offset section reports three buckets (issue #50), each either
`measured` with a count or `not-measured` with the reason:

- **`provably-missing`** — `offset.missing`. Proof layer, `default` floor,
  in every stream; always measured.
- **`unguarded`** — `offset.maybe-missing`. Admitted by the `strict` rung
  alone (A-G10). Measured when the document's `profile` is the built-in
  `strict` or when the id fires (a user profile extending `strict`);
  otherwise `not-measured`, with a note that says what triage cannot
  tell — the document carries the profile's *name* and not the id set it
  resolved to, so a user profile extending `strict` with zero fires is 0
  or unmeasured, never asserted either way — and names the run that
  settles it.
- **`guarded-and-discharged`** — a read the ladder deleted. A deleted
  finding leaves nothing in a stream of findings, so this bucket is
  `not-measured` from every stream, and the report says so in the
  bucket itself rather than printing a zero that would read as "none".

**The gap is recorded, not closed.** Measuring discharges needs a
`check`-side count the pipeline does not emit today — a run-level
`discharged` counter, or a per-id accounting beside `baselined`. Issue
#50's brief puts `check`'s schema out of scope, and this ADR agrees:
the count is a real addition with its own design (which discharges —
S4 promotions, S5 covers, `??` arms — and at which stage of ADR-0050
§6's pipeline it is taken), and the strict offset leg (#51) is where it
belongs. The same counter is what A-G10's other promise needs: the
general-map and list reads it says "do not fire in v1 but are counted in
triage buckets" leave no finding either, so a findings-only stream cannot
count them, and §1's citation is a promise this ADR hands to #51 with
the discharge count rather than one it keeps. Until then the bucket is a
named hole, which is what the
issue's "derive from what the stream already carries" asks for.

## 5. Hints: a recognizer catalogue, and what a hint is not

A hint is one recognizer's advice over the aggregate. The catalogue is a
static table of `(name, fn)` rows; v1 ships four — a strict what-if that
was not measured, a baseline hiding debt from the totals, an id whose
findings are ≥ 80% in one file (localised, fix before freezing), and
optional-key reads ≥ 60% under one directory (a shape-to-DTO transform
candidate). Every hint carries its recognizer's name so a reader can
discount one they disagree with, and every row has a test that fails if
the row is removed.

Refusals, each anchored:

- **A hint is not a finding and never becomes one.** It has no `id`,
  enters no suppression channel (ADR-0023), is never baselined
  (ADR-0022), and cannot fail a run. A recognizer that wants to gate is
  asking to be a diagnostic id, which is a registry decision (ADR-0022,
  ADR-0050 §2) and not a triage one.
- **No message-regex recognizers.** Recognizers read `id`, `layer`,
  `level`, `path` and counts; wording is not a contract (ADR-0023,
  ADR-0050 §10).
- **No per-recognizer config.** The catalogue is the tool's opinion about
  count shapes; a project that wants a different opinion reads the
  distribution, which is the data the hints were computed from.

## 6. Consequences

- `check`'s JSON is now read by a second first-party consumer. Its
  additive discipline (ADR-0050 §2: new keys, never renamed ones) is what
  keeps triage stable; a rename would be a triage regression as well as
  a consumer break.
- The command's own JSON is additive from here on the same terms: a new
  section or field is fine, a renamed or removed one is a break.
- The fp-gate is unaffected by construction: triage emits no finding, and
  the `check` it runs is the shipped `check`.
- ADR-0020's command set gains `triage`. ADR-0022's deferred
  "statistics helper over the baseline" is *not* this command: triage
  reads the display stream, not the baseline file, and a baselined
  finding reaches it only as the `baselined` counter (and the
  `baseline-hides-debt` hint that reads it). A baseline-file reader, if
  one is ever wanted, is a separate decision.
