//! The **process transport**: a resident `php` child speaking the wire format over
//! NDJSON. Native-only (`cfg(not(target_arch = "wasm32"))`) — no wasm runtime can
//! spawn a process, and gating the module rather than the crate keeps
//! [`crate::wire`] available everywhere (ADR-0066).
//!
//! Every request/response shape here comes from [`crate::wire`]; this file owns
//! only the framing, the timeout, and the poison-and-respawn discipline.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::wire::{
    EnvInfo, FoldArg, FoldResult, Reflection, env_params, fold_params, parse_env_result,
    parse_fold_result, parse_reflection_result, reflect_params,
};

/// The runner source, baked into the binary. Written to disk at spawn time.
const RUNNER_SRC: &str = include_str!("../runner.php");

/// Default per-request timeout (ADR-0024). Generous for a local `php` call;
/// anything slower is treated as misbehavior and widened.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// How many times one [`Sidecar`] will replace a dead child before giving up.
///
/// The storm brake. A run that kills three children is not hitting an accident;
/// it is being fed input that kills children, and each respawn costs a PHP
/// startup. Past the cap the instance behaves exactly as it did before respawn
/// existed — permanently poisoned, every later request widens immediately.
const RESPAWN_CAP: u32 = 3;

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
    /// Launch `php <runner_path>` and start draining its stdout.
    fn open(runner_path: &Path) -> std::io::Result<Self> {
        let mut child = Command::new("php")
            .arg(runner_path)
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
/// A per-function result-size budget on the Rust side (bounding `str_repeat`'s
/// `count × strlen` and so on) was considered and rejected: it demands resource
/// knowledge of every builtin, is silently incomplete for every future allowlist
/// member, and is helpless against the non-memory death classes. Respawn is
/// indifferent to *why* the child died.
pub struct Sidecar {
    chan: Channel,
    next_id: u64,
    timeout: Duration,
    /// The child is dead and no request can be sent until it is replaced
    /// (ADR-0024). See [`Sidecar::is_poisoned`] for what this does *not* mean.
    poisoned: bool,
    /// Respawns already attempted, against `RESPAWN_CAP`.
    respawns: u32,
    /// Temp file holding the runner; removed (with its dir) on drop.
    runner_path: PathBuf,
}

impl Sidecar {
    /// Spawn the sidecar: write `runner.php` to a fresh temp dir and launch
    /// `php <runner>`, resolving `php` from `PATH`. Returns an error only when
    /// the process cannot be started (missing `php`, IO failure) — the caller
    /// turns that into the sound-subset posture.
    pub fn spawn() -> std::io::Result<Self> {
        // A unique per-*instance* temp dir avoids collisions between concurrent
        // sidecars (rayon workers in the gate, parallel tests): each owns its
        // dir and removes only its own on drop. A respawn reuses this same dir,
        // so a revived child runs the identical runner from the identical path.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!("steins-sidecar-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let runner_path = dir.join("steins-runner.php");
        std::fs::write(&runner_path, RUNNER_SRC)?;

        let chan = Channel::open(&runner_path)?;

        Ok(Self {
            chan,
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            poisoned: false,
            respawns: 0,
            runner_path,
        })
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
        // Rewrite the runner: the dir is ours, but a long-lived run has no
        // guarantee a temp cleaner left the file alone.
        if std::fs::write(&self.runner_path, RUNNER_SRC).is_err() {
            return false;
        }
        match Channel::open(&self.runner_path) {
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
    /// The chosen reading of the two available ones. `true` says a prior request
    /// failed and killed the child; it says nothing about the next request, which
    /// will revive the instance if the `RESPAWN_CAP` allows. That keeps the
    /// predicate meaning the same thing it always did (a failure just happened,
    /// and it was a transport failure rather than a widened answer), which is how
    /// every caller reads it today.
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
        // Closing stdin lets a healthy runner exit on its own; kill covers a
        // hung or poisoned child. Then join the reader and clean the temp dir —
        // the dir a respawn kept reusing, so there is still exactly one.
        self.chan.close();
        if let Some(parent) = self.runner_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
