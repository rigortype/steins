//! The `assertType` harness seam (oracle idea B): `PHPStan\Testing\assertType('T', $e)`
//! observations and the subject probes the nsrt harness consumes. Harness-only —
//! recognition is gated on a thread-local sink that only [`collect_assert_types`] and
//! [`probe_subjects`] install, so an ordinary check never sees `assertType` as anything
//! but a call.

use std::collections::HashMap;

use steins_db::{Db, Project};
use steins_domain::{Certainty, Fact, Val};
use steins_syntax::Stmt;

use crate::cx::Cx;
use crate::env::{Known, Stratum};
use crate::{Folder, NoFold, check_project};

// ---------------------------------------------------------------------------
// The assertType harness seam (oracle idea B): consume
// `PHPStan\Testing\assertType('T', $e)` and measure Steins' rendering of `$e`
// against PHPStan's asserted string. This is **harness-only** — recognition is
// gated on a thread-local sink installed exclusively by [`collect_assert_types`],
// so a normal check never sees `assertType` as anything but an ordinary call (the
// check surface is byte-identical). It reuses the D3 dump path verbatim
// (`best_dump_type` → `render_dump_fact`/speller) so the rendered fact matches
// what `PHPStan\dumpType` would emit for the same expression.
// ---------------------------------------------------------------------------

/// One `PHPStan\Testing\assertType('Expected', $expr)` observation (oracle idea B).
/// The expected type string PHPStan asserts, paired with Steins' own rendering of
/// the second argument at that call position (the same best-fact + speller path the
/// D3 dump surface uses). Collected ONLY by [`collect_assert_types`].
#[derive(Debug, Clone)]
pub struct AssertObservation {
    /// The project-relative path of the file the assertion lives in.
    pub path: String,
    /// The 1-based line of the `assertType(...)` call.
    pub line: u32,
    /// The 1-based column of the call.
    pub column: u32,
    /// The first-argument type-string literal PHPStan asserts, when it is a
    /// resolvable string literal. `None` when the expected slot is a
    /// `::class`/concatenation expression Steins cannot fold to a plain string —
    /// the harness counts those as *skipped*, never a spurious match.
    pub expected: Option<String>,
    /// Steins' rendering of the second argument's best fact (the dump-surface
    /// speller), or the honest `unknown` when nothing faithful can be spelled.
    pub got: String,
    /// Whether `got` rode an `Asserted`-stratum premise (a docblock/assert claim
    /// rather than a proven value) — surfaced so the harness never mistakes a
    /// laundered docblock type for a proof.
    pub asserted: bool,
}

thread_local! {
    /// The harness-only assertType sink (oracle idea B). `None` during every normal
    /// check — the [`emit_asserts`] recognizer is a no-op then and the ADR-0070
    /// survival gate's assertType read exception ([`is_assert_read_site`]) is off,
    /// so the check surface is byte-identical (assertType stays an ordinary call).
    /// [`collect_assert_types`] installs a fresh buffer for one project run and
    /// drains it; both consumers key on the same installed-sink condition, so the
    /// harness universe is entered and left as one piece.
    ///
    /// [`emit_asserts`]: crate::emit_asserts
    /// [`is_assert_read_site`]: crate::is_assert_read_site
    pub(crate) static ASSERT_SINK: std::cell::RefCell<Option<Vec<AssertObservation>>> =
        const { std::cell::RefCell::new(None) };
}

/// Harness entry point (oracle idea B, the nsrt assertType measurement): analyze
/// `project` exactly as [`check_project`] would, but collect every
/// `PHPStan\Testing\assertType('T', $e)` call's (expected string, Steins rendering)
/// pair. NOT part of the check surface — recognition is gated on the installed sink,
/// so a normal check never sees `assertType` as anything but an ordinary call. Inside
/// THIS run the sink also makes every `assertType` site a transparent read (the
/// ADR-0070 gate's assertType exception), so repeated assertions on one variable
/// each observe the undegraded env; the diagnostics `check_project` returns under
/// that universe are discarded here, never reported.
#[must_use]
pub fn collect_assert_types(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
) -> Vec<AssertObservation> {
    ASSERT_SINK.with(|s| *s.borrow_mut() = Some(Vec::new()));
    let _ = check_project(db, project, folder);
    ASSERT_SINK.with(|s| s.borrow_mut().take().unwrap_or_default())
}

/// What the propagation walk knows about one variable at one program point
/// (ADR-0076 §3, the loop-subject probe). Every field is `false` when the walk
/// bound nothing there — a missing answer is "not proven", never a guess.
///
/// The answer carries **both trust lanes in one struct**: `array`/`list` report
/// what the bound fact *says*, and `verified` reports which stratum (ADR-0052
/// §5) says it. A consumer requiring `verified` reads the proven lane; one that
/// deliberately accepts `verified == false` (the ADR-0076 issue-#175 opt-in) is
/// consuming the **Asserted** lane — a docblock claim, never a proof — and must
/// keep the two apart in everything it derives. The stratum bit is the wall
/// that stops one lane laundering into the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubjectFact {
    /// `true` when the bound fact is an array value (an abstract shape or a
    /// fully-known array literal) that does **not** also admit `null`. A bare
    /// native `array $xs` hint contributes nothing here at either stratum: the
    /// native lowering represents no `array` member (ADR-0002 silence), so
    /// only a docblock shape or a walked value can answer.
    pub array: bool,
    /// `true` when that array's denotational `array_is_list` verdict is `Yes`
    /// (ADR-0062 §3). `array_map` over a single array preserves keys, while
    /// `$out[] = …` renumbers `0..n-1`, so anything weaker is not equivalent.
    pub list: bool,
    /// `true` when the fact sits at the `Verified` stratum (ADR-0052 §5) — a
    /// runtime-executed test or a native declaration, fit to premise a proof.
    /// A docblock-asserted array shape answers `false` here and never
    /// premises the proven lane; `false` with `array`/`list` set is exactly
    /// the Asserted answer the ADR-0076 amendment's opt-in consumes.
    pub verified: bool,
}

thread_local! {
    /// The ADR-0076 loop-subject probe. `None` during every normal check, so the
    /// walk's per-statement hook is a single `is_some()` test and the check
    /// surface is byte-identical; [`probe_subjects`] installs a request table for
    /// one project run and drains the answers.
    static SUBJECT_PROBE: std::cell::RefCell<Option<SubjectProbeState>> =
        const { std::cell::RefCell::new(None) };
}

/// The installed probe's request table plus the answers collected so far.
#[derive(Default)]
struct SubjectProbeState {
    /// `(path, statement start offset)` → the variable name to observe there.
    wanted: HashMap<(String, u32), String>,
    /// `(path, statement start offset)` → what the walk knew on entry.
    answers: HashMap<(String, u32), SubjectFact>,
}

/// Ask the propagation walk what it knows about a variable at a program point
/// (ADR-0076 §3): each site is `(path, statement start offset, variable name)`,
/// and the answer is keyed by `(path, offset)`.
///
/// The walk observes **entry** facts — what holds *before* the statement runs —
/// which is the state a `foreach` head reads its subject in. A site with no
/// answer (unreachable, poisoned scope, or simply unbound) is absent from the
/// map, which callers read as [`SubjectFact::default`]: nothing proven.
///
/// Analysis runs in the sound subset ([`NoFold`]), matching the rest of the
/// transform engine: a planned rewrite must not depend on whether a PHP sidecar
/// happened to be reachable.
#[must_use]
pub fn probe_subjects(
    db: &dyn Db,
    project: Project,
    sites: &[(String, u32, String)],
) -> HashMap<(String, u32), SubjectFact> {
    if sites.is_empty() {
        return HashMap::new();
    }
    let wanted: HashMap<(String, u32), String> =
        sites.iter().map(|(p, off, var)| ((p.clone(), *off), var.clone())).collect();
    SUBJECT_PROBE.with(|s| {
        *s.borrow_mut() = Some(SubjectProbeState { wanted, answers: HashMap::new() });
    });
    let _ = check_project(db, project, &mut NoFold);
    SUBJECT_PROBE.with(|s| s.borrow_mut().take().map(|st| st.answers).unwrap_or_default())
}

/// Answer any installed [`SUBJECT_PROBE`] request keyed at this statement, from
/// the walk's **entry** env. A no-op with no probe installed (every normal
/// check), and never run under a binding descent — the plain per-scope pass is
/// the one universal reading, exactly as the dump and trace observers are gated.
pub(crate) fn record_subject_probe(cx: &Cx<'_>, stmt: &Stmt, env: &HashMap<String, Known>) {
    SUBJECT_PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else { return };
        let key = (cx.path().to_owned(), stmt.span.start);
        let Some(var) = state.wanted.get(&key) else { return };
        let fact = env.get(var).map_or_else(SubjectFact::default, |known| {
            let (array, list) = match &known.fact {
                Some(Fact::Shape { shape, nullable: false }) => {
                    (true, shape.is_list == Certainty::Yes)
                }
                Some(Fact::Singleton(Val::Array(entries))) => {
                    (true, steins_domain::array_is_list(entries))
                }
                _ => (false, false),
            };
            SubjectFact { array, list, verified: known.stratum == Stratum::Verified }
        });
        state.answers.insert(key, fact);
    });
}
