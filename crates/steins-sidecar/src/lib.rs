//! PHP sidecar IPC — the resident helper process that runs the project's own
//! PHP to fold literal calls (ADR-0004, ADR-0024).
//!
//! # Protocol
//!
//! JSON-RPC 2.0 with NDJSON framing over the child's stdin/stdout. The PHP side
//! is a single, dependency-free file (`runner.php`, embedded via [`include_str!`]
//! and written to a per-process temp dir), launched as `php <runner>`. `php` is
//! resolved from `PATH` at spawn time — the *project's own* PHP, per ADR-0004.
//!
//! # Zero-FP contract (ADR-0024, binding)
//!
//! Sidecar misbehavior must NEVER become a wrong diagnostic. Every failure mode
//! — spawn failure, IO error, per-request timeout, malformed response — maps to
//! [`FoldResult::Widen`], never a value. On any such failure the child is killed
//! and the instance is **poisoned**: later calls widen immediately instead of
//! hanging or reviving a half-dead process.
//!
//! # Concurrency model
//!
//! No async runtime. A single background thread drains the child's stdout into a
//! channel; each request writes a line and waits on the channel with a timeout
//! ([`std::sync::mpsc::Receiver::recv_timeout`]). Requests are strictly
//! serialized (`&mut self`) and stateless, so a restart would be transparent.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// The runner source, baked into the binary. Written to disk at spawn time.
const RUNNER_SRC: &str = include_str!("../runner.php");

/// Default per-request timeout (ADR-0024). Generous for a local `php` call;
/// anything slower is treated as misbehavior and widened.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// A JSON-encodable literal argument to a folded call. Only the scalar literals
/// the trace IR carries (ADR-0027) are representable.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldArg {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

/// A concrete value returned by a successful fold, tagged with its PHP type.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

/// The outcome of a `fold` request (ADR-0024). An exception is a *result*, not
/// an error — `1/0` yields `Throw { class: "DivisionByZeroError" }`.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldResult {
    /// The call returned a value we can carry as a literal.
    Value(FoldValue),
    /// The call threw; `class` is the Throwable's class name.
    Throw { class: String },
    /// Anything we cannot turn into type information: unknown function, wrong
    /// arity, unencodable result, or any sidecar failure (timeout/IO/poison).
    Widen { reason: String },
}

impl FoldResult {
    fn widen(reason: impl Into<String>) -> Self {
        FoldResult::Widen { reason: reason.into() }
    }
}

/// Environment facts reported by the `env` method — coverage-posture material.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvInfo {
    pub php_version: String,
    pub extensions: Vec<String>,
    pub sapi: String,
}

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
    /// path). Default is [`DEFAULT_TIMEOUT`].
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
        let value = self.request("env", serde_json::json!({}))?;
        let obj = value.get("result")?;
        Some(EnvInfo {
            php_version: obj.get("php_version")?.as_str()?.to_owned(),
            extensions: obj
                .get("extensions")?
                .as_array()?
                .iter()
                .filter_map(|e| e.as_str().map(ToOwned::to_owned))
                .collect(),
            sapi: obj.get("sapi")?.as_str()?.to_owned(),
        })
    }

    /// Fold one builtin call: send `fold(name, args)` and interpret the reply.
    /// Never panics; any failure widens and poisons.
    pub fn fold(&mut self, name: &str, args: &[FoldArg]) -> FoldResult {
        if self.poisoned {
            return FoldResult::widen("sidecar poisoned");
        }
        let params = serde_json::json!({
            "function": name,
            "args": args.iter().map(fold_arg_to_json).collect::<Vec<_>>(),
        });
        let Some(value) = self.request("fold", params) else {
            return FoldResult::widen("sidecar failure");
        };
        let Some(result) = value.get("result") else {
            self.poison();
            return FoldResult::widen("malformed response");
        };
        parse_fold_result(result)
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

/// Encode a [`FoldArg`] as JSON, preserving float-ness (`5.0`, not `5`).
fn fold_arg_to_json(arg: &FoldArg) -> serde_json::Value {
    match arg {
        FoldArg::Int(v) => serde_json::json!(v),
        FoldArg::Float(v) => serde_json::Number::from_f64(*v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        FoldArg::Str(v) => serde_json::json!(v),
        FoldArg::Bool(v) => serde_json::json!(v),
        FoldArg::Null => serde_json::Value::Null,
    }
}

/// Interpret a `result` object (`{kind, ...}`) as a [`FoldResult`]. Any shape we
/// do not recognize widens — never a wrong value.
fn parse_fold_result(result: &serde_json::Value) -> FoldResult {
    match result.get("kind").and_then(serde_json::Value::as_str) {
        Some("value") => parse_fold_value(result)
            .map_or_else(|| FoldResult::widen("unencodable value"), FoldResult::Value),
        Some("throw") => match result.get("class").and_then(serde_json::Value::as_str) {
            Some(class) => FoldResult::Throw { class: class.to_owned() },
            None => FoldResult::widen("throw without class"),
        },
        Some("widen") => FoldResult::widen(
            result.get("reason").and_then(serde_json::Value::as_str).unwrap_or("widen").to_owned(),
        ),
        _ => FoldResult::widen("unknown result kind"),
    }
}

/// Turn a `{kind:"value", value, type}` object into a typed [`FoldValue`]. The
/// `type` tag disambiguates cases JSON alone cannot (e.g. `1` as int vs. bool).
fn parse_fold_value(result: &serde_json::Value) -> Option<FoldValue> {
    let value = result.get("value")?;
    match result.get("type").and_then(serde_json::Value::as_str)? {
        "int" => value.as_i64().map(FoldValue::Int),
        "float" => value.as_f64().map(FoldValue::Float),
        "string" => value.as_str().map(|s| FoldValue::Str(s.to_owned())),
        "bool" => value.as_bool().map(FoldValue::Bool),
        "null" => Some(FoldValue::Null),
        // `array` (and anything else) has no literal in our IR yet — widen.
        _ => None,
    }
}
