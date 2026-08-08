//! The **process transport**: a resident `php` child speaking the wire format over
//! NDJSON. Native-only (`cfg(not(target_arch = "wasm32"))`) — no wasm runtime can
//! spawn a process, and gating the module rather than the crate keeps
//! [`crate::wire`] available everywhere (ADR-0066).
//!
//! Every request/response shape here comes from [`crate::wire`]; this file owns
//! only the framing, the timeout, and the poison-and-respawn discipline.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::wire::{
    ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection, defined_params,
    env_params, fold_params, parse_defined_result, parse_env_result, parse_fold_result,
    parse_preg_compile_result, parse_reflection_result, preg_compile_params, reflect_params,
};

/// The runner source, baked into the binary.
///
/// Passed to `php -r` as an argv element (see [`Channel::open`]) — nothing is
/// ever written to disk, so there is no per-instance or per-process temp file
/// to leak or to clean up.
const RUNNER_SRC: &str = include_str!("../runner.php");

/// [`RUNNER_SRC`] with its leading `<?php` tag line removed, ready for `-r`
/// (which forbids the open tag — the code it runs is implicitly PHP already).
/// Stripped by prefix rather than a hardcoded byte offset, so a change to the
/// tag line (e.g. trailing whitespace) fails loudly here instead of silently
/// mis-slicing the program.
fn runner_code() -> &'static str {
    RUNNER_SRC.strip_prefix("<?php\n").expect(
        "runner.php must start with the literal \"<?php\\n\" tag line: `-r` runs its \
         argument as already-PHP code and rejects an explicit open tag, so the tag is \
         stripped here rather than passed through",
    )
}

/// Default per-request timeout (ADR-0024). Generous for a local `php` call;
/// anything slower is treated as misbehavior and widened.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// How many times one [`Sidecar`] will replace a dead child before giving up.
///
/// The storm brake. A run that kills three children is being fed input that
/// kills children, and each respawn costs a PHP startup. Past the cap the
/// instance stays poisoned and every later request widens immediately.
///
/// Public so a run's **coverage report** can say which side of the brake it
/// ended on (issue #245) — see [`Sidecar::respawns`]. It is a reporting input,
/// never a request gate.
pub const RESPAWN_CAP: u32 = 3;

/// One live child and the thread draining it — everything a respawn replaces.
///
/// Grouped so replacing a dead child is a single assignment: there is no state
/// where the [`Child`] is fresh but the [`Receiver`] still belongs to the corpse.
struct Channel {
    child: Child,
    stdin: ChildStdin,
    /// Lines drained from the child's stdout by the reader thread.
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl Channel {
    /// Launch `php -r <code>` — the runner source passed as a single argv
    /// element, never touching disk — and start draining its stdout.
    ///
    /// # Why argv, not a file, and not stdin
    ///
    /// stdin is already spoken for: it *is* the NDJSON request stream this
    /// `Channel` writes to below. `php < script.php` (or piping the source in)
    /// would have PHP consume stdin to EOF as the program text before running
    /// anything, which would eat the protocol stream instead of the source.
    /// argv is the channel with no such conflict, and `runner.php` qualifies:
    /// it uses none of `__FILE__`/`__DIR__`/`$argv` and has no closing `?>`,
    /// so it means the same thing whether PHP reads it from a file or from
    /// `-r`. At ~16 KB it sits far under both `ARG_MAX` (~1 MB on macOS) and
    /// Linux's `MAX_ARG_STRLEN` (128 KB) — see `runner_size_stays_under_the_argv_limit`.
    ///
    /// Two trade-offs are accepted: the source is visible in `ps`/`/proc`
    /// (it is not a secret — the same file ships readable in the binary), and
    /// a PHP parse error would report against "Command line code" rather than
    /// a filename (moot in practice: stderr is discarded below regardless).
    fn open() -> std::io::Result<Self> {
        let mut child = Command::new("php")
            .arg("-r")
            .arg(runner_code())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Discard PHP's stderr: warnings/notices must never reach us, and we
            // treat any real failure as a widen anyway. It is also where an
            // uncatchable fatal prints its message before the process dies.
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match buf.read_line(&mut line) {
                    Ok(0) => break, // EOF: child closed stdout.
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            break; // receiver gone.
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        Ok(Self { child, stdin, lines: rx, reader: Some(reader) })
    }

    /// Kill the child, **reap** it, and join the reader thread.
    ///
    /// The reaping is the point: a respawn that only killed would leave a zombie
    /// per dead child. Killing closes the child's stdout, which ends the reader's
    /// `read_line` loop, so the join cannot hang on a live process.
    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A resident PHP sidecar process plus its request loop.
///
/// Spawned lazily by the caller (only when the first foldable call is actually
/// encountered). Dropping it closes the child's stdin, so the runner's read loop
/// ends and the process exits; [`Drop`] also kills and reaps the child.
///
/// # Surviving a dead child
///
/// Not every way a child can die is catchable in PHP. An allocation that exhausts
/// `memory_limit` is a fatal, not a `Throwable`; so is a stack overflow or a
/// segfault in an extension. `str_repeat('x', 2000000000)` is an ordinary literal
/// call on the folding allowlist, and before the runner pinned `memory_limit` it
/// could claim a gigabyte before dying. The runner cannot defend against this
/// class from the inside, so the transport defends from the outside: it replaces
/// the child.
///
/// The discipline is deliberately asymmetric.
///
/// * The request whose reply never arrived **still fails**. It widens, exactly as
///   before. It is never retried on the fresh child: that request is the likely
///   bomb, and retrying it would re-arm the same fatal.
/// * The *next* request revives the instance (`Sidecar::revive`), up to
///   `RESPAWN_CAP` times per instance. So one poisoned fold costs one answer,
///   not the whole run.
///
/// Nothing is replayed, because there is nothing to replay: the runner is a pure
/// per-request dispatcher with no cross-request state — no session, no accumulated
/// definitions, no cursor. A fresh child answers the next request identically to
/// the one it replaced, which is what makes a respawn transparent rather than a
/// resynchronization problem.
///
pub struct Sidecar {
    chan: Channel,
    next_id: u64,
    timeout: Duration,
    /// The child is dead and no request can be sent until it is replaced
    /// (ADR-0024). See [`Sidecar::is_poisoned`] for what this does *not* mean.
    poisoned: bool,
    /// Respawns already attempted, against `RESPAWN_CAP`.
    respawns: u32,
}

impl Sidecar {
    /// Spawn the sidecar: launch `php -r <runner source>`, resolving `php`
    /// from `PATH`. Returns an error only when the process cannot be started
    /// (missing `php`, IO failure) — the caller turns that into the
    /// sound-subset posture.
    pub fn spawn() -> std::io::Result<Self> {
        let chan = Channel::open()?;

        Ok(Self { chan, next_id: 1, timeout: DEFAULT_TIMEOUT, poisoned: false, respawns: 0 })
    }

    /// Make sure a live child is available, replacing a dead one if the cap
    /// allows. `true` means a request may be sent; `false` means this instance is
    /// finished and every caller must widen.
    ///
    /// Cheap on the healthy path (one bool), and the *only* place `poisoned` is
    /// cleared. The counter charges attempts, not successes: a respawn that fails
    /// to start `php` is exactly the situation the cap exists to bound.
    fn revive(&mut self) -> bool {
        if !self.poisoned {
            return true;
        }
        if self.respawns >= RESPAWN_CAP {
            return false;
        }
        self.respawns += 1;
        self.chan.close();
        match Channel::open() {
            Ok(chan) => {
                self.chan = chan;
                self.poisoned = false;
                true
            }
            // Still poisoned, one attempt poorer. `php` was on `PATH` moments
            // ago, so this is a transient failure worth another try later.
            Err(_) => false,
        }
    }

    /// Override the per-request timeout (mainly for tests exercising the timeout
    /// path). The default is 2 seconds (ADR-0024): generous for a local `php`
    /// call, and anything slower is treated as misbehavior and widened.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Whether the child is dead **right now** — not whether this instance is
    /// finished.
    ///
    /// `true` says a prior request failed and killed the child; it says nothing
    /// about the next request, which will revive the instance if the
    /// `RESPAWN_CAP` allows. The predicate means "a transport failure just
    /// happened", not "a value was widened".
    ///
    /// The permanent state — cap exhausted, every later request widens — is
    /// deliberately not exposed as a predicate: no caller has a use for it that
    /// is not better served by simply making the request and widening on the
    /// answer, and a "permanently dead" flag invites exactly the run-long
    /// disabling this recovery exists to prevent.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Respawn attempts charged against [`RESPAWN_CAP`] so far — how many children
    /// this instance has already buried (issue #245).
    ///
    /// **Reporting only.** It exists so a long run can state its coverage posture
    /// in its own output — "the child died twice and was replaced twice" reads very
    /// differently from "the budget is spent and every later request widens" — and
    /// a reader who is comparing counts needs to know which one happened. It is
    /// deliberately NOT the "permanently dead" predicate [`Self::is_poisoned`]'s
    /// doc declines to offer: gating a request on `respawns() >= RESPAWN_CAP`
    /// re-creates exactly the run-long disabling the respawn discipline exists to
    /// prevent. Make the request and widen on the answer, as before; read this only
    /// to describe what happened.
    #[must_use]
    pub fn respawns(&self) -> u32 {
        self.respawns
    }

    /// Query the child's PHP environment. Returns `None` on any failure (the
    /// instance is poisoned, matching the fold contract).
    pub fn env(&mut self) -> Option<EnvInfo> {
        let value = self.request("env", env_params())?;
        parse_env_result(value.get("result")?)
    }

    /// Ask the project's own PHP whether `target` exists among builtins and loaded
    /// extensions (ADR-0024 surface / ADR-0049 §1 oracle (b)). A definitive
    /// *not-found* is `Some(Reflection)` with `exists() == false`; any sidecar
    /// failure (poison, timeout, malformed/`widen` reply) is `None` — "unknown",
    /// never a wrong not-found (the zero-FP contract). Older runners without the
    /// `reflect` method reply `widen`, which maps to `None` as well.
    pub fn reflect(&mut self, target: &str) -> Option<Reflection> {
        if !self.revive() {
            return None;
        }
        let value = self.request("reflect", reflect_params(target))?;
        parse_reflection_result(value.get("result")?, target)
    }

    /// Ask the project's own PCRE whether it accepts `pattern` (issue #189 /
    /// ADR-0078, ADR-0004's ask-the-real-thing). `Some(PregCompile::Refuses{..})`
    /// is the only answer that licenses a finding; `Some(Compiles)` and `None` —
    /// poison, timeout, a `widen`, a runner too old to implement the method — are
    /// both silence at the consumer.
    pub fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile> {
        if !self.revive() {
            return None;
        }
        let value = self.request("preg_compile", preg_compile_params(pattern))?;
        parse_preg_compile_result(value.get("result")?)
    }

    /// Ask the project's own PHP whether the global constant `name` is defined
    /// (issue #198 / ADR-0078) — the existence oracle for extension constants and
    /// anything an already-loaded bootstrap defined. `name` is the resolved FQN,
    /// case as written. `Some(NotDefined)` is the only answer that lets the
    /// `constant.undefined` ladder continue; `Some(Defined)` and `None` — poison,
    /// timeout, a `widen`, a runner too old to implement the method — are both
    /// silence at the consumer.
    pub fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined> {
        if !self.revive() {
            return None;
        }
        let value = self.request("defined", defined_params(name))?;
        parse_defined_result(value.get("result")?)
    }

    /// Fold one builtin call: send `fold(name, args)` and interpret the reply.
    /// Never panics; any failure widens and poisons.
    pub fn fold(&mut self, name: &str, args: &[FoldArg]) -> FoldResult {
        if !self.revive() {
            return FoldResult::widen("sidecar poisoned");
        }
        let Some(value) = self.request("fold", fold_params(name, args)) else {
            return FoldResult::widen("sidecar failure");
        };
        let Some(result) = value.get("result") else {
            self.poison();
            return FoldResult::widen("malformed response");
        };
        parse_fold_result(result)
    }

    /// Send `method`/`params` **verbatim** and return the raw `result` value.
    ///
    /// The answering primitive of the ADR-0066 replay loop: a pending request is a
    /// canonical `{"method", "params"}` object, and answering it means handing that
    /// object to a real engine and putting the `result` back in the table. Doing so
    /// through this method — rather than re-deriving the params from a typed call —
    /// is what makes a replay run and a direct run the *same* dispatch.
    pub fn call_raw(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        self.request(method, params)?.get("result").cloned()
    }

    /// Send one JSON-RPC request and read its response, honoring the timeout.
    /// Returns the parsed response object, or `None` after poisoning on any
    /// IO/timeout/parse failure.
    ///
    /// A dead child is replaced here, before the write — never after the read
    /// failed, which would mean retrying the request that killed it.
    fn request(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        if !self.revive() {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = req.to_string();
        line.push('\n');

        if self.chan.stdin.write_all(line.as_bytes()).is_err() || self.chan.stdin.flush().is_err() {
            self.poison();
            return None;
        }

        match self.chan.lines.recv_timeout(self.timeout) {
            Ok(Ok(line)) => {
                let value: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => {
                        self.poison();
                        return None;
                    }
                };
                // Responses are strictly ordered; a mismatched id means the
                // stream desynced — poison rather than trust it.
                if value.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                    self.poison();
                    return None;
                }
                Some(value)
            }
            // A timeout, or a channel whose sender is gone. The latter is how an
            // uncatchable fatal announces itself: the child dies, its stdout
            // EOFs, the reader thread ends, and the receiver disconnects — so a
            // dead child is noticed at once rather than after the full timeout.
            Ok(Err(_)) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                self.poison();
                None
            }
        }
    }

    /// Poison the instance and kill the child so later calls widen fast.
    ///
    /// Kill only — the reaping happens in [`Channel::close`], on the respawn or
    /// the drop that follows. This request is already lost either way.
    fn poison(&mut self) {
        self.poisoned = true;
        let _ = self.chan.child.kill();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Closing stdin lets a healthy runner exit; killing covers a hung or
        // poisoned child. `Channel::close` also reaps it and joins the reader.
        self.chan.close();
    }
}
