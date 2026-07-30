//! The **process transport**: a resident `php` child speaking the wire format over
//! NDJSON. Native-only (`cfg(not(target_arch = "wasm32"))`) — no wasm runtime can
//! spawn a process, and gating the module rather than the crate keeps
//! [`crate::wire`] available everywhere (ADR-0066).
//!
//! Every request/response shape here comes from [`crate::wire`]; this file owns
//! only the framing, the timeout, and the poison discipline.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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

/// A resident PHP sidecar process plus its request loop.
///
/// Spawned lazily by the caller (only when the first foldable call is actually
/// encountered). Dropping it closes the child's stdin, so the runner's read loop
/// ends and the process exits; [`Drop`] also kills the child defensively.
pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    /// Lines drained from the child's stdout by the reader thread.
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
    timeout: Duration,
    /// Once poisoned, every request widens immediately (ADR-0024).
    poisoned: bool,
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
        // dir and removes only its own on drop.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!("steins-sidecar-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let runner_path = dir.join("steins-runner.php");
        std::fs::write(&runner_path, RUNNER_SRC)?;

        let mut child = Command::new("php")
            .arg(&runner_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Discard PHP's stderr: warnings/notices must never reach us, and we
            // treat any real failure as a widen anyway.
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

        Ok(Self {
            child,
            stdin,
            lines: rx,
            reader: Some(reader),
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            poisoned: false,
            runner_path,
        })
    }

    /// Override the per-request timeout (mainly for tests exercising the timeout
    /// path). The default is 2 seconds (ADR-0024): generous for a local `php`
    /// call, and anything slower is treated as misbehavior and widened.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Whether this instance has been poisoned by a prior failure.
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
        if self.poisoned {
            return None;
        }
        let value = self.request("reflect", reflect_params(target))?;
        parse_reflection_result(value.get("result")?, target)
    }

    /// Fold one builtin call: send `fold(name, args)` and interpret the reply.
    /// Never panics; any failure widens and poisons.
    pub fn fold(&mut self, name: &str, args: &[FoldArg]) -> FoldResult {
        if self.poisoned {
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
    fn request(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        if self.poisoned {
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

        if self.stdin.write_all(line.as_bytes()).is_err() || self.stdin.flush().is_err() {
            self.poison();
            return None;
        }

        match self.lines.recv_timeout(self.timeout) {
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
            // Timeout or a dead channel: the child is misbehaving.
            Ok(Err(_)) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                self.poison();
                None
            }
        }
    }

    /// Poison the instance and kill the child so later calls widen fast.
    fn poison(&mut self) {
        self.poisoned = true;
        let _ = self.child.kill();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Closing stdin lets a healthy runner exit on its own; kill covers a
        // hung or poisoned child. Then join the reader and clean the temp dir.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(parent) = self.runner_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
