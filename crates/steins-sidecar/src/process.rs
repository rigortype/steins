//! The **process transport**: a resident `php` child speaking the wire format over
//! NDJSON. Native-only (`cfg(not(target_arch = "wasm32"))`) since no wasm runtime
//! can spawn a process; gating the module rather than the crate keeps
//! [`crate::wire`] available everywhere (ADR-0066). Every request/response shape
//! comes from [`crate::wire`]; this file owns only framing, timeout, and the
//! poison-and-respawn discipline.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::wire::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
    defined_params, env_params, fold_params, parse_class_reflection_result, parse_defined_result,
    parse_env_result, parse_fold_result, parse_preg_compile_result, parse_reflection_result,
    preg_compile_params, reflect_class_params, reflect_params,
};

/// The runner source, baked into the binary. Passed to `php -r` as an argv
/// element (see [`Channel::open`]) — never written to disk, so there is no
/// per-instance or per-process temp file to leak or clean up.
const RUNNER_SRC: &str = include_str!("../runner.php");

/// [`RUNNER_SRC`] with its leading `<?php` tag line removed, ready for `-r`
/// (which forbids the open tag). Stripped by prefix rather than a hardcoded
/// byte offset, so a tag-line change fails loudly here instead of silently
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
/// The storm brake: killing three children means the input itself kills
/// children, and each respawn costs a PHP startup. Past the cap the instance
/// stays poisoned and every later request widens immediately.
///
/// Public so a run's coverage report can say which side of the brake it ended
/// on (issue #245) — see [`Sidecar::respawns`]. A reporting input, never a gate.
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
    /// # Why argv, not a file or stdin
    ///
    /// stdin is already the NDJSON request stream `Channel` writes to below;
    /// `php < script.php` would consume it as program text first. argv has no
    /// such conflict, and `runner.php` qualifies: no `__FILE__`/`__DIR__`/
    /// `$argv`, no closing `?>`. At ~16 KB it sits far under `ARG_MAX` (~1 MB
    /// macOS) and Linux's `MAX_ARG_STRLEN` (128 KB) — see
    /// `runner_size_stays_under_the_argv_limit`.
    ///
    /// Trade-offs: source is visible in `ps`/`/proc` (not a secret), and a
    /// parse error reports against "Command line code" (moot: stderr discarded).
    fn open() -> std::io::Result<Self> {
        let mut child = Command::new("php")
            .arg("-r")
            .arg(runner_code())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Discard stderr: warnings must never reach us, real failures widen
            // anyway; this is also where an uncatchable fatal prints before death.
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
/// Spawned lazily, only when the first foldable call is encountered. Dropping
/// it closes the child's stdin, ending the runner's read loop; [`Drop`] also
/// kills and reaps the child.
///
/// # Surviving a dead child
///
/// Not every death is catchable in PHP: an allocation past `memory_limit`, a
/// stack overflow, or an extension segfault are fatal, not `Throwable`.
/// `str_repeat('x', 2000000000)` — an ordinary allowlisted call — could claim a
/// gigabyte before the runner pinned `memory_limit`. The runner can't defend
/// from the inside, so the transport defends from the outside: it replaces
/// the child.
///
/// Asymmetric discipline: the request whose reply never arrived **still
/// fails** (widens) and is never retried on the fresh child — it is the
/// likely bomb, and retrying would re-arm the fatal. The *next* request
/// revives the instance (`Sidecar::revive`), up to `RESPAWN_CAP` times, so one
/// poisoned fold costs one answer, not the whole run.
///
/// Nothing is replayed: the runner is a pure per-request dispatcher with no
/// cross-request state, so a fresh child answers identically — a respawn is
/// transparent, not a resynchronization problem.
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
    /// allows. `true` means a request may be sent; `false` means every caller
    /// must widen. The *only* place `poisoned` is cleared; charges attempts,
    /// not successes — a respawn that fails to start `php` is what the cap
    /// exists to bound.
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
    /// finished. `true` means a prior request killed the child; it says nothing
    /// about the next request, which revives the instance if `RESPAWN_CAP`
    /// allows ("a transport failure just happened", not "a value was widened").
    ///
    /// The permanent state (cap exhausted, every later request widens) is
    /// deliberately not a predicate: no caller needs it over simply requesting
    /// and widening, and such a flag would invite the run-long disabling this
    /// recovery exists to prevent.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Respawn attempts charged against [`RESPAWN_CAP`] so far — how many
    /// children this instance has already buried (issue #245).
    ///
    /// **Reporting only.** Lets a long run state its coverage posture — "died
    /// twice, replaced twice" reads differently from "budget spent". Not the
    /// "permanently dead" predicate [`Self::is_poisoned`] declines to offer:
    /// gating a request on `respawns() >= RESPAWN_CAP` would re-create the
    /// run-long disabling this discipline exists to prevent.
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
    /// failure (poison, timeout, malformed/`widen` reply, or an old runner
    /// without this method) is `None` — "unknown", never a wrong not-found (the
    /// zero-FP contract).
    pub fn reflect(&mut self, target: &str) -> Option<Reflection> {
        if !self.revive() {
            return None;
        }
        let value = self.request("reflect", reflect_params(target))?;
        parse_reflection_result(value.get("result")?, target)
    }

    /// Ask the project's own PHP for the **declaration** behind a resident
    /// class-like (issue #269) — the class-world half of the ADR-0024 `reflect`
    /// surface, and the only honest source for a class an installed extension
    /// provides (ADR-0049 §1). `Some(ClassReflection)` with `declaration: None`
    /// is a definitive not-found; any sidecar failure is `None`, "unknown" —
    /// never a wrong or half-read declaration.
    pub fn reflect_class(&mut self, target: &str) -> Option<ClassReflection> {
        if !self.revive() {
            return None;
        }
        let value = self.request("reflect_class", reflect_class_params(target))?;
        parse_class_reflection_result(value.get("result")?, target)
    }

    /// Ask the project's own PCRE whether it accepts `pattern` (issue #189 /
    /// ADR-0078, ADR-0004's ask-the-real-thing). Only
    /// `Some(PregCompile::Refuses{..})` licenses a finding; `Some(Compiles)` and
    /// any sidecar failure are both silence at the consumer.
    pub fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile> {
        if !self.revive() {
            return None;
        }
        let value = self.request("preg_compile", preg_compile_params(pattern))?;
        parse_preg_compile_result(value.get("result")?)
    }

    /// Ask the project's own PHP whether the global constant `name` (resolved
    /// FQN, case as written) is defined (issue #198 / ADR-0078) — the existence
    /// oracle for extension constants and bootstrap-defined names. Only
    /// `Some(NotDefined)` lets the `constant.undefined` ladder continue;
    /// `Some(Defined)` and any sidecar failure are both silence at the consumer.
    pub fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined> {
        if !self.revive() {
            return None;
        }
        let value = self.request("defined", defined_params(name))?;
        parse_defined_result(value.get("result")?)
    }

    /// Fold one builtin call: send `fold(name, args, strict)` and interpret the
    /// reply. `strict` is the CALL SITE's `declare(strict_types=1)`, not this
    /// process's — see [`fold_params`]. Never panics; any failure widens and
    /// poisons.
    pub fn fold(&mut self, name: &str, args: &[FoldArg], strict: bool) -> FoldResult {
        if !self.revive() {
            return FoldResult::widen("sidecar poisoned");
        }
        let Some(value) = self.request("fold", fold_params(name, args, strict)) else {
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
    /// The answering primitive of the ADR-0066 replay loop: a pending request is
    /// a canonical `{"method", "params"}` object, answered by handing it to a
    /// real engine and putting `result` back in the table. Going through this
    /// method, not re-deriving params from a typed call, makes replay and
    /// direct runs the *same* dispatch.
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
