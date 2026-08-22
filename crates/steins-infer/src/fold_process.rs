//! The native transport: a resident `php` child behind [`ProcessEngine`], spawned
//! lazily, with the sound-subset and stopped-answering notices (ADR-0004, issue
//! #110). Native-only — the whole module is `cfg(not(target_arch = "wasm32"))`.
//!
//! [`ProcessEngine`]: crate::ProcessEngine

use steins_sidecar::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
};
#[cfg(not(target_arch = "wasm32"))]
use steins_sidecar::Sidecar;

use crate::{SIDECAR_HANDSHAKE_NOTICE, SOUND_SUBSET_NOTICE};
use crate::fold::{EngineFolder, FoldEngine, FoldPosture};

// ---------------------------------------------------------------------------
// The native transport: a resident `php` child.
// ---------------------------------------------------------------------------

/// The process [`FoldEngine`]: a lazily-spawned PHP [`Sidecar`] (ADR-0004/0024).
///
/// Owns exactly the transport's own state — whether folding is disabled, whether
/// the spawn already failed, whether the sound-subset notice has been printed,
/// and (issue #110) whether the "stopped answering" notice has been printed.
/// No analysis policy lives here; that is [`EngineFolder`]'s.
#[cfg(not(target_arch = "wasm32"))]
pub struct ProcessEngine {
    sidecar: Option<Sidecar>,
    disabled: bool,
    spawn_failed: bool,
    notified: bool,
    /// Whether [`SIDECAR_HANDSHAKE_NOTICE`] has already been printed this run —
    /// the issue #110 latch, sibling to `notified` above but for "spawned, then
    /// a request went unanswered" rather than "could not spawn at all". Kept as
    /// a separate flag since it guards different text and can stay meaningfully
    /// false after `ensure` has stopped being consulted.
    ///
    /// A prior revision suppressed this notice permanently after any request
    /// succeeded, on the theory that later poisoning is always the
    /// respawn-tolerant failure mode. That does not hold: `Sidecar`'s contract
    /// is that a lost reply is never retried, so respawn recovers the
    /// INSTANCE but does not un-widen the answer already lost — a mid-run
    /// timeout is exactly as silent as one at the start. So this is a plain
    /// once-per-run latch, armed by the first poisoning event anywhere in the
    /// run, with no permanent suppression from an earlier success (review
    /// finding on PR #134).
    unresponsive_notified: bool,
    /// Requests that ended with the child dead or silent (issue #245) — the
    /// [`FoldPosture::losses`] counter. Counted on the EDGE into poison rather
    /// than per poisoned call: past the respawn cap `is_poisoned` stays true for
    /// every remaining request of the run, and counting those would report tens of
    /// thousands of "losses" for one dead child.
    losses: u32,
    /// The edge detector for `losses`: the last `Sidecar::is_poisoned` this engine
    /// observed. A `false → true` step is a loss; a `true → false` step is a
    /// successful respawn, which is [`Sidecar::respawns`]'s business to count.
    poisoned_seen: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl ProcessEngine {
    /// `disabled` (the CLI's `--no-php`) makes this a permanent no-op that never
    /// spawns PHP, and never announces the sound subset (the user asked for it).
    #[must_use]
    pub fn new(disabled: bool) -> Self {
        Self {
            sidecar: None,
            disabled,
            spawn_failed: false,
            notified: true, // suppress our own notice; only spawn-failure re-arms it.
            unresponsive_notified: true, // suppress; only enabled() re-arms it (mirrors `notified`).
            losses: 0,
            poisoned_seen: false,
        }
    }

    /// An enabled engine that emits the sound-subset notice itself if it cannot
    /// spawn PHP, or the "stopped answering" notice the first time a request
    /// poisons the sidecar, at any point in the run.
    #[must_use]
    pub fn enabled() -> Self {
        Self { notified: false, unresponsive_notified: false, ..Self::new(false) }
    }

    /// Ensure a live sidecar, or record that we cannot have one.
    fn ensure(&mut self) -> Option<&mut Sidecar> {
        if self.disabled || self.spawn_failed {
            return None;
        }
        if self.sidecar.is_none() {
            match Sidecar::spawn() {
                Ok(sc) => self.sidecar = Some(sc),
                Err(_) => {
                    self.spawn_failed = true;
                    if !self.notified {
                        // The one place a *library* crate writes to a user-facing
                        // stream, and it obeys the CLI's output seam rule (issue
                        // #44): `eprintln!` panics when the write fails, and
                        // `steins check 2>&1 | head` closes stderr like any other
                        // pipe. A lost notice is not a reason to abort a run, so
                        // the error is dropped rather than propagated — the seam's
                        // stderr policy, stated in `steins-cli/src/out.rs`.
                        use std::io::Write;
                        let _ = writeln!(std::io::stderr(), "{SOUND_SUBSET_NOTICE}");
                        self.notified = true;
                    }
                    return None;
                }
            }
        }
        self.sidecar.as_mut()
    }

    /// Run one request against the live sidecar (spawning it first if needed),
    /// then check the issue #110 latch from the transport's OWN post-call state
    /// (`Sidecar::is_poisoned`) rather than `op`'s return value: a `fold` that
    /// legitimately widens (an out-of-range argument, a non-allowlisted callee,
    /// an exception result) is not a transport failure and must never arm the
    /// notice — only the child actually going silent or dying does. `None` when
    /// no sidecar can be had at all ([`Self::ensure`] already covers that case).
    fn call<T>(&mut self, op: impl FnOnce(&mut Sidecar) -> T) -> Option<T> {
        let sc = self.ensure()?;
        let result = op(sc);
        let poisoned = sc.is_poisoned();
        // The loss ledger (issue #245), read off the same post-call state the
        // notice latch is: an edge into poison is one answer this run will never
        // have. The notice says it happened; this counts how often, so the run's
        // own report can qualify the numbers it prints.
        if poisoned && !self.poisoned_seen {
            self.losses += 1;
        }
        self.poisoned_seen = poisoned;
        if poisoned {
            self.note_unresponsive();
        }
        Some(result)
    }

    /// The latch body: the request [`Self::call`] just ran left the sidecar
    /// poisoned. Prints [`SIDECAR_HANDSHAKE_NOTICE`] on the FIRST such event in
    /// the run, wherever it falls — the opening `env()` handshake or a request
    /// deep into an otherwise-healthy run — and never again after. There is
    /// deliberately no "but a request succeeded before this one" escape: a
    /// widened request stays widened regardless of what the sidecar does next
    /// (`Sidecar`'s own contract — a lost reply is never retried), so a mid-run
    /// failure is exactly as silent to the caller as one at the very start.
    fn note_unresponsive(&mut self) {
        if self.unresponsive_notified {
            return;
        }
        self.unresponsive_notified = true;
        // Same stderr policy as the spawn-failure notice above: a dropped write
        // is not a reason to abort a run (issue #44 / steins-cli/src/out.rs).
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{SIDECAR_HANDSHAKE_NOTICE}");
    }

    /// Send `method`/`params` verbatim to the child and return the raw `result`.
    /// The native answering half of an ADR-0066 replay request; `None` when no
    /// sidecar can be had or the request failed.
    pub fn call_raw(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        self.call(|sc| sc.call_raw(method, params)).flatten()
    }

    /// What this engine delivered over the whole run (issue #245).
    ///
    /// `engaged` is "a child was spawned", not "a request succeeded": a spawn that
    /// worked and then answered nothing is a *degraded* run with losses, which is
    /// a different story from the sound subset and must not be told as one.
    /// `abandoned` reads the respawn budget rather than the poison flag alone —
    /// poisoned-with-budget-left is a child about to be replaced, poisoned-with-
    /// none-left is the end of the fold surface for this run.
    #[must_use]
    pub fn posture(&self) -> FoldPosture {
        let Some(sc) = &self.sidecar else {
            return FoldPosture::default();
        };
        FoldPosture {
            engaged: true,
            losses: self.losses,
            restarts: sc.respawns(),
            abandoned: sc.is_poisoned() && sc.respawns() >= steins_sidecar::RESPAWN_CAP,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl FoldEngine for ProcessEngine {
    fn env(&mut self) -> Option<EnvInfo> {
        self.call(Sidecar::env).flatten()
    }

    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        self.call(|sc| sc.reflect(target)).flatten()
    }

    fn reflect_class(&mut self, target: &str) -> Option<ClassReflection> {
        self.call(|sc| sc.reflect_class(target)).flatten()
    }

    fn fold(&mut self, name: &str, args: &[FoldArg], strict: bool) -> FoldResult {
        self.call(|sc| sc.fold(name, args, strict))
            .unwrap_or_else(|| FoldResult::widen("no sidecar"))
    }

    fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile> {
        self.call(|sc| sc.preg_compile(pattern)).flatten()
    }

    fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined> {
        self.call(|sc| sc.constant_defined(name)).flatten()
    }

    fn restarts(&self) -> u32 {
        self.sidecar.as_ref().map_or(0, Sidecar::respawns)
    }
}

/// The default native folder: the shared policy over the process transport.
///
/// A type alias, not a wrapper — [`EngineFolder`] IS the policy, and giving the
/// native pairing a name keeps every existing call site (`SidecarFolder::new`,
/// `SidecarFolder::enabled`, `set_php_target`, `impl Folder`) spelled as before.
#[cfg(not(target_arch = "wasm32"))]
pub type SidecarFolder = EngineFolder<ProcessEngine>;

#[cfg(not(target_arch = "wasm32"))]
impl EngineFolder<ProcessEngine> {
    /// Create a folder. `disabled` (the CLI's `--no-php`) makes it a permanent
    /// no-op that never spawns PHP.
    #[must_use]
    pub fn new(disabled: bool) -> Self {
        Self::with_engine(ProcessEngine::new(disabled))
    }

    /// Create an enabled folder that will emit the sound-subset notice itself if
    /// it cannot spawn PHP.
    #[must_use]
    pub fn enabled() -> Self {
        Self::with_engine(ProcessEngine::enabled())
    }

    /// The fold surface this folder actually delivered (issue #245) — for a
    /// caller that prints a number and owes the reader its coverage posture.
    #[must_use]
    pub fn posture(&self) -> FoldPosture {
        self.engine.posture()
    }
}
