# A file that fails to parse is named loudly and dams the absence family

Issue #180. Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode, per the ADR-0063/0067/0076/0077 precedent).
Context: ADR-0046 (dynamism posture), ADR-0049 §2 (the runtime-definition
dam), ADR-0013 (zero-FP bar), the cross-check evidence in
`docs/notes/20260725-phpstan-cross-check.md` §2, and the grilled decisions of
2026-08-08 recorded in `docs/notes/20260808-phpstan-rule-port-map.md`.

## 1. The two failures, separated

A file `php -l` rejects analyses today to exit 0 with no diagnostic:
`SourceTree::parse` recovers, `parse_errors()` has no consumer outside a
smoke test, and inference emits proof-grade findings from the recovered
tree. Two distinct things are wrong, and they have different grades:

1. **The silence.** A checker that accepts broken PHP without comment is not
   adoptable regardless of its rule count. The real instance: pixelfed ships
   `if($this->status->)` in a committed file titled "Lint"; PHPStan reports
   it (and aborts); Steins says nothing.
2. **The unsoundness.** A file that does not parse can hide *declarations* —
   a recovery point may have swallowed a class, a function, or **members of
   a class-like the recovery kept**. Every absence proof in the project
   rests on complete enumeration; an unparsable file makes the enumeration
   unprovable. This is sharper than the existing dam's threat model: `eval`
   can mint new names but cannot reopen a defined class (the ADR-0049 §2
   immunity asymmetry), whereas a mangled class body can have lost methods —
   so a parse failure undermines **method**-absence for the classes declared
   in that file, not only name existence.

## 2. Decisions

1. **`syntax.unparsable`** — a new **mechanics**-layer id (`Default` floor),
   emitted **once per unparsable file**, positioned at the first parse error
   and naming the count of further errors. One per file, not per error:
   recovery cascades make every position after the first unreliable.
   Mechanics semantics apply in full: fail level, red on sight,
   profile-`disable`-proof, suppression-exempt per the registry's existing
   mechanics rules. The remedy is fixing the file, and the exit code says so.
2. **The dam gains `DamKind::Unparsable`.** A non-vendor file with parse
   errors is a dam site (path + first-error position), joining the
   whole-universe dam fact of ADR-0049 §2. While any such site stands, the
   existence-absence ids are silent project-wide — the same posture, the
   same `doctor` surface naming the sites, the same remedy-first shape as
   `eval`.
3. **The vendor presumption carries over verbatim** (ADR-0046 §2): an
   unparsable file under a `vendor/` path component is presumed plumbing or
   fixture material (parser test suites ship deliberately broken PHP) and is
   **not** a dam site. Its `syntax.unparsable` finding exists but flows
   through the vendor filter like every other finding — visible under
   `--vendor-diagnostics`, suppressed by default. This is a recorded
   presumption, not a proof, exactly as it is for vendor `eval`.
4. **The broken file itself emits nothing beyond `syntax.unparsable`.** Its
   recovered tree may misattribute anything locally, and a finding built on
   a misparse is precisely the manufactured-FP shape ADR-0002 forbids. Its
   recovered declarations still enter the index as **presence** evidence —
   presence can only silence an absence claim, never fire one, so a
   half-recovered declaration is safe in that direction and keeps
   cross-file resolution working.
5. **Class-likes declared in an unparsable file are member-incomplete.** The
   method/property-absence ladders treat a hierarchy that passes through
   such a class as not enumerable (the chain-closure leg fails through it).
   This is the §1.2 asymmetry made explicit: the universe-wide dam covers
   name existence, this leg covers member enumeration, and both come from
   the same site list.

## 3. Deferred with design: position-aware refinement

The blunt instrument above silences the absence family everywhere because
one file is broken. The refinement — an error region provably *inside a
statement body* cannot have swallowed a declaration, so top-level recovery
dams and body-local recovery does not — would keep pixelfed's absence
findings alive while its one broken method body is quarantined. It is
deferred, not refused: it needs the syntax-tree contract to expose recovery
*regions* (spans the recovery skipped), which the backend does not surface
today. When it lands, `DamKind::Unparsable` sites gain a region and the dam
consults it; nothing else changes shape. Conditional class declarations
inside bodies (legal PHP) mean the body-local judgment must still check the
region for declaration keywords — recorded here so the refinement is not
implemented naively.

## 4. Consequences

- One broken non-vendor file turns the absence family off everywhere, with
  a loud, positioned, fail-level finding naming the cause and `doctor`
  listing the site. Chosen over PHPStan's hard abort (the rest of the
  analysis — value flow, warning-grade findings on parsed files — continues)
  and over file-local containment (unsound, §1.2).
- The fp-gate corpus must be swept for pre-existing unparsable files before
  the id ships; any found are corpus facts to record, not surprises to
  debug.
- `parse_errors()` gains its first real consumer; the smoke test stops being
  load-bearing documentation.
