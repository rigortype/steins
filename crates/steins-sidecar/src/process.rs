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
use std::time::{Duration, Instant};

use crate::wire::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
    defined_params, env_params, fold_params, parse_class_reflection_result, parse_defined_result,
    parse_env_result, parse_fold_result, parse_preg_compile_result, parse_reflection_result,
    preg_compile_params, reflect_class_params, reflect_params,
};

/// The runner source, baked into the binary. Passed to `php -r` as an argv
/// element (see `Channel::open`) — never written to disk, so there is no
/// per-instance or per-process temp file to leak or clean up.
///
/// Public because it is half of what an answer depends on: `env` describes
/// the PHP that answers, and this is the program answering on it, so a store
/// of recorded answers has to key on both.
pub const RUNNER_SRC: &str = include_str!("../runner.php");

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

/// How long a fresh child may take to answer its opening `env` handshake
/// (issue #891), charged to [`Channel::open`] and to nothing else.
///
/// The per-request budget cannot be the boot budget. [`DEFAULT_TIMEOUT`] starts
/// when a request is written, and a fresh child's first request waits out PHP's
/// own startup: about 60 ms of CPU on an idle machine, but wall-clock time, so a
/// loaded CI runner can stretch it past 2 s. That cost three respawns, a run
/// degraded on stderr, and (ADR-0092's amendment for #784) a generation that was
/// never published, where a missing `php` costs none of that.
///
/// 20 s is a few hundred times the idle boot, so only an interpreter that is
/// wedged rather than starved fails it, and it is still short enough that the
/// worst case stays bounded: one wait on the first spawn (the engine goes off,
/// the sound subset), and [`RESPAWN_CAP`] waits across a revive storm, one minute
/// per storm (the strikes restart at every answer, so not per run). Real
/// requests keep [`DEFAULT_TIMEOUT`], so hang detection on a running child is
/// unchanged.
const BOOT_TIMEOUT: Duration = Duration::from_secs(20);

/// The environment variable that overrides [`BOOT_TIMEOUT`], in milliseconds.
/// Read once per process; unset, unparsable or zero means the default.
///
/// A knob and a test seam: a CI runner starved harder than 20 s can raise it,
/// and a test that wants to see a hung boot fail does not have to wait out the
/// default.
const BOOT_TIMEOUT_ENV: &str = "STEINS_SIDECAR_BOOT_TIMEOUT_MS";

fn boot_timeout() -> Duration {
    static TIMEOUT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        std::env::var(BOOT_TIMEOUT_ENV)
            .ok()
            .and_then(|ms| ms.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map_or(BOOT_TIMEOUT, Duration::from_millis)
    })
}

/// Why a child that started did not finish booting: the payload of the
/// [`std::io::Error`] [`Sidecar::spawn_with`] returns for it.
///
/// A distinct payload because the two ways a spawn fails mean different things
/// to a caller (issue #110, #891). `php` that cannot be started at all (absent,
/// not executable) is the sound subset, announced as such. `php` that started
/// and then never answered, or died, or spoke garbage, is a *degraded* run:
/// the engine exists and failed, which is not the same report as it being
/// missing. Read it with [`is_boot_failure`].
#[derive(Debug)]
struct BootFailure(&'static str);

impl std::fmt::Display for BootFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BootFailure {}

fn boot_failure(kind: std::io::ErrorKind, what: &'static str) -> std::io::Error {
    std::io::Error::new(kind, BootFailure(what))
}

/// Whether `error`, from [`Sidecar::spawn`] or [`Sidecar::spawn_with`], is a
/// child that started but did not complete its boot handshake, as opposed to
/// one that could not be started at all.
#[must_use]
pub fn is_boot_failure(error: &std::io::Error) -> bool {
    error.get_ref().is_some_and(|inner| inner.is::<BootFailure>())
}

/// How long [`Channel::close`] waits for the reader thread after the child is
/// killed, before it gives the thread up (issue #894).
///
/// A killed child's stdout closes at once, so the reader normally ends in well
/// under a millisecond. It outlives that only when something outside the child
/// still holds the pipe: a descendant that escaped the kill, which on Unix means
/// one that left the process group, and elsewhere any descendant at all. Waiting
/// on that is waiting on a stranger, so the thread is detached instead, blocked on
/// a pipe nobody reads, and the run goes on.
///
/// A detached thread keeps its pipe and its stack until the stranger exits,
/// which may be never. One per close would add up over a run of timeouts, each
/// costing a file descriptor, until the process ran out of them. So the first
/// detach is also the last: the [`Sidecar`] that had to give up a reader stops
/// replacing its child for the rest of the run ([`Sidecar::revive`]), like an
/// engine past [`RESPAWN_CAP`].
const READER_GRACE: Duration = Duration::from_millis(500);

/// The id the opening handshake carries. [`Sidecar`] numbers its requests from
/// 1, so a reply that is not the handshake's can never pass for one.
const HANDSHAKE_ID: u64 = 0;

/// How many times in a row one [`Sidecar`] will replace a dead child with no
/// answer in between before giving up.
///
/// The storm brake: three replacements that each died before answering
/// anything mean the engine itself is broken, and each respawn costs a PHP
/// startup. Past the cap the instance stays poisoned and every later request
/// widens immediately.
///
/// The count restarts at every answered request (issue #783). It was a
/// lifetime budget, and a lifetime budget is not a storm brake: four `range()`
/// literals in one analysed file each killed a child that had answered
/// hundreds of requests, spent the budget inside that file, and left every
/// file after it on the sound subset. A child that answers is not a storm. What
/// stops one bomb from recurring is its caller's business — the fold seam
/// stops asking a callee that killed a child — and the transport only has to
/// stop respawning into an engine that cannot answer at all.
///
/// Public so a run's coverage report can say which side of the brake it ended
/// on (issue #245) — see [`Sidecar::strikes`]. A reporting input, never a gate.
pub const RESPAWN_CAP: u32 = 3;

/// One NDJSON request line: the JSON-RPC envelope, newline included.
fn frame(id: u64, method: &str, params: serde_json::Value) -> String {
    let mut line = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string();
    line.push('\n');
    line
}

/// Attempts at a spawn that `exec` refuses with `ETXTBSY`, the first included.
const SPAWN_BUSY_ATTEMPTS: u32 = 5;

/// The wait after the first busy refusal; it doubles after each later one, so
/// the four waits total 150 ms, far inside [`BOOT_TIMEOUT`]'s budget.
const SPAWN_BUSY_BACKOFF: Duration = Duration::from_millis(10);

/// Runs `spawn`, retrying while it fails with
/// [`std::io::ErrorKind::ExecutableFileBusy`] (issue #910).
///
/// On Linux `execve` refuses an executable that some process still holds open
/// for writing. A thread that has just written a script and closed it can still
/// lose to a concurrent `fork` on another thread: the forked child inherits the
/// write descriptor and drops it only at its own `exec`, so for that window the
/// script is busy. The same refusal meets a package manager that is replacing
/// the `php` binary. Both clear within milliseconds, so a few short retries turn
/// a spurious engine-off into a slightly later start. Any other error, and a
/// busy one that outlasts the attempts, is returned as it came, so a spawn that
/// really fails still fails exactly as before.
///
/// Generic over the spawner so the policy is testable without a real `exec`.
fn spawn_retrying_busy<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
    first_backoff: Duration,
) -> std::io::Result<T> {
    let mut backoff = first_backoff;
    for _ in 1..SPAWN_BUSY_ATTEMPTS {
        match spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
            outcome => return outcome,
        }
    }
    spawn()
}

/// One live child and the thread draining it — everything a respawn replaces.
///
/// Grouped so replacing a dead child is a single assignment: there is no state
/// where the [`Child`] is fresh but the [`Receiver`] still belongs to the corpse.
struct Channel {
    child: Child,
    /// `None` once the channel is closed. Dropping it is what tells a process the
    /// kill did not reach, such as an interpreter behind a wrapper that did not
    /// `exec` it, that no more requests are coming (issue #894).
    stdin: Option<ChildStdin>,
    /// Lines drained from the child's stdout by the reader thread.
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
    /// The interpreter this channel was opened with — `"php"` for the resident
    /// engine, an explicit path for a mining run that names its minor. A respawn
    /// must reach the SAME build: a replacement resolved from `PATH` would answer
    /// the next request as a different PHP.
    bin: String,
    /// Whether `child` has been waited on. Its pid, which is also its process
    /// group's id, can be recycled from then on, so the group is never signalled
    /// after it.
    reaped: bool,
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
    ///
    /// # The boot handshake
    ///
    /// Returns only a child that has answered an `env` request, under
    /// [`boot_timeout`] rather than the request budget (issue #891). A child that
    /// does not answer is killed and reaped here and reported as an `Err`, so a
    /// slow boot is a failed spawn (the engine-off posture) or a strike on a
    /// revive, never a lost real request.
    fn open(bin: &str) -> Result<Self, OpenFailure> {
        let mut chan = Self::launch(bin).map_err(|error| OpenFailure { error, stranded: false })?;
        match chan.handshake() {
            Ok(()) => Ok(chan),
            Err(error) => {
                let stranded = !chan.close();
                Err(OpenFailure { error, stranded })
            }
        }
    }

    /// Start the child and its reader thread, without waiting for it to answer.
    ///
    /// On Unix the child leads a process group of its own, so [`Self::kill`] can
    /// reach whatever it starts. `php` is often a wrapper script (a version
    /// manager, a container shim), and one that runs the interpreter without
    /// `exec` leaves the interpreter a grandchild that killing the child alone
    /// would miss (issue #894).
    ///
    /// The group kill reaches only descendants that stay in the group. An
    /// interpreter that leaves it, as behind `exec setsid php "$@"` on Linux
    /// (which forks, because the wrapper leads its group), survives the kill and
    /// keeps stdout open. [`READER_GRACE`] keeps that from hanging a close, and
    /// the strand it triggers keeps it from costing more than one.
    ///
    /// Its own group is not the terminal's foreground group, so a Ctrl-C reaches
    /// steins and not the child. Nothing is lost by that: steins does not catch
    /// the signal, its death closes the child's stdin, and the runner's read
    /// loop ends at that EOF, after the request in flight if there is one.
    fn launch(bin: &str) -> std::io::Result<Self> {
        let mut command = Command::new(bin);
        command
            .arg("-r")
            .arg(runner_code())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Discard stderr: warnings must never reach us, real failures widen
            // anyway; this is also where an uncatchable fatal prints before death.
            .stderr(Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = spawn_retrying_busy(|| command.spawn(), SPAWN_BUSY_BACKOFF)?;

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
            stdin: Some(stdin),
            lines: rx,
            reader: Some(reader),
            bin: bin.to_owned(),
            reaped: false,
        })
    }

    /// Write one request line. A closed channel fails like a closed pipe.
    fn send(&mut self, line: &str) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// Send the `env` request and wait [`boot_timeout`] for a well-formed answer.
    /// The reply is checked and dropped: `env` is asked again, by the caller who
    /// wants it, so the handshake feeds no state back into the [`Sidecar`].
    ///
    /// Every failure here is a [`BootFailure`], the write's included: a child
    /// that exited before reading its request is a boot that failed, not a
    /// `php` that could not be started.
    fn handshake(&mut self) -> std::io::Result<()> {
        use std::io::ErrorKind;
        let request = frame(HANDSHAKE_ID, "env", env_params());
        self.send(&request)
            .map_err(|e| boot_failure(e.kind(), "php closed its input during its boot handshake"))?;
        // One deadline for the whole handshake: noise lines do not extend it.
        let deadline = Instant::now() + boot_timeout();
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(wait) {
                Ok(line) => {
                    line.map_err(|e| boot_failure(e.kind(), "php's output failed at boot"))?
                }
                Err(RecvTimeoutError::Timeout) => {
                    let what = "php did not answer its boot handshake";
                    return Err(boot_failure(ErrorKind::TimedOut, what));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let what = "php exited during its boot handshake";
                    return Err(boot_failure(ErrorKind::UnexpectedEof, what));
                }
            };
            // A line that is not JSON is startup noise (a `php.ini` that prints,
            // a deprecation notice on stdout), not the runner: skip it. The
            // first JSON line is the runner's, and it must be the handshake's.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            let answered = value.get("id").and_then(serde_json::Value::as_u64)
                == Some(HANDSHAKE_ID)
                && value.get("result").and_then(parse_env_result).is_some();
            if answered {
                return Ok(());
            }
            let what = "php answered its boot handshake with something else";
            return Err(boot_failure(ErrorKind::InvalidData, what));
        }
    }

    /// Kill the child and, on Unix, everything in its process group.
    ///
    /// Safe to call at any point: once the child is reaped the group is left
    /// alone, and std's own kill is a no-op on a reaped child.
    fn kill(&mut self) {
        if !self.reaped {
            kill_group(self.child.id());
        }
        let _ = self.child.kill();
    }

    /// Close stdin, kill the child, **reap** it, and wait a bounded time for the
    /// reader thread. Idempotent. `false` when the reader had to be detached
    /// (see [`READER_GRACE`] for what that obliges the caller to do).
    ///
    /// The reaping is the point: a respawn that only killed would leave a zombie
    /// per dead child. The rest is so that no close can block a run (issue #894).
    /// Killing the child closes its stdout, which ends the reader's `read_line`
    /// loop, but only if nothing else holds the pipe: behind a wrapper that did
    /// not `exec`, the interpreter does. The group kill reaches it on Unix, the
    /// closed stdin lets it exit wherever the kill does not, and
    /// [`READER_GRACE`] bounds the wait when neither works.
    #[must_use]
    fn close(&mut self) -> bool {
        drop(self.stdin.take());
        self.kill();
        let _ = self.child.wait();
        self.reaped = true;
        self.finish_reader()
    }

    /// Join the reader thread once it has ended, or detach it after
    /// [`READER_GRACE`]. The thread ends by dropping its sender, so the
    /// disconnect is the signal; a line still in flight is discarded, since no
    /// one is waiting for it. `false` only for the call that detached it.
    fn finish_reader(&mut self) -> bool {
        let Some(reader) = self.reader.take() else { return true };
        let deadline = Instant::now() + READER_GRACE;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(wait) {
                Ok(_) if Instant::now() < deadline => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = reader.join();
                    return true;
                }
                // Out of grace: dropping the handle detaches the thread.
                Ok(_) | Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }
}

/// Why [`Channel::open`] failed, and whether closing the child that failed had
/// to detach its reader, which strands a [`Sidecar`] (see [`READER_GRACE`]).
struct OpenFailure {
    error: std::io::Error,
    stranded: bool,
}

/// `SIGKILL` to the process group `pid` leads (see [`Channel::launch`]).
///
/// Called only before the leader is reaped, so the id cannot have been
/// recycled: an unreaped child keeps its pid, and with it the group's id. A
/// group that is already empty fails with `ESRCH`, which is ignored like
/// every other kill failure here.
#[cfg(unix)]
fn kill_group(pid: u32) {
    // A group id of 0 would name this process's own group.
    let Some(group) = libc::pid_t::try_from(pid).ok().filter(|&group| group > 0) else {
        return;
    };
    // SAFETY: `killpg` takes two integers and touches no memory of ours.
    unsafe { libc::killpg(group, libc::SIGKILL) };
}

/// Elsewhere the child leads no group, so there is nothing more to signal: the
/// closed stdin and [`READER_GRACE`] are what keep a descendant from holding a
/// close there.
#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

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
/// revives the instance (`Sidecar::revive`), up to `RESPAWN_CAP` times in a
/// row without an answer, so one poisoned fold costs one answer, not the whole
/// run.
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
    /// Respawns attempted over the instance's life. Reporting only.
    respawns: u32,
    /// Respawns attempted since the last answered request — the number
    /// `RESPAWN_CAP` bounds.
    strikes: u32,
    /// Children lost over the instance's life: one per [`Sidecar::poison`].
    deaths: u32,
}

impl Sidecar {
    /// Spawn the sidecar: launch `php -r <runner source>`, resolving `php`
    /// from `PATH`. Returns an error when the process cannot be started
    /// (missing `php`, IO failure) or does not answer its boot handshake within
    /// its 20-second boot timeout (issue #891) — the caller turns either into the
    /// sound-subset posture.
    pub fn spawn() -> std::io::Result<Self> {
        Self::spawn_with("php")
    }

    /// The same, against a NAMED interpreter rather than the one on `PATH`.
    ///
    /// Analysis always uses [`Self::spawn`]: the project's own engine is the one
    /// boot-surface truth, and choosing a different one would make a finding a
    /// property of the machine. The named form is for the generators, which ask
    /// several minors the same question and record which answered
    /// (`cargo xtask mine-function-map --php PATH`, issue #714).
    pub fn spawn_with(bin: &str) -> std::io::Result<Self> {
        // A spawn that fails is the engine off for the run, so a detached
        // reader here needs no strand: nothing will open another child.
        let chan = Channel::open(bin).map_err(|failure| failure.error)?;

        Ok(Self {
            chan,
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            poisoned: false,
            respawns: 0,
            strikes: 0,
            deaths: 0,
        })
    }

    /// Make sure a live child is available, replacing a dead one if the cap
    /// allows. `true` means a request may be sent; `false` means every caller
    /// must widen. The *only* place `poisoned` is cleared; charges attempts,
    /// not successes — a respawn that fails to start `php` is what the cap
    /// exists to bound. Only an answered request ([`Self::request`]) clears
    /// the strikes, so a replacement that dies before answering counts
    /// against the next one.
    ///
    /// A close that had to detach its reader (issue #894) strands the instance:
    /// the strikes jump to the cap, so it reads, and reports, as an engine
    /// abandoned for the run. A child is only closed here after a poison, so
    /// the run has already lost an answer and is degraded either way; the strand
    /// only stops it from leaking one thread and one descriptor per timeout.
    fn revive(&mut self) -> bool {
        if !self.poisoned {
            return true;
        }
        if self.strikes >= RESPAWN_CAP {
            return false;
        }
        if !self.chan.close() {
            self.strikes = RESPAWN_CAP;
            return false;
        }
        self.respawns += 1;
        self.strikes += 1;
        match Channel::open(&self.chan.bin) {
            Ok(chan) => {
                self.chan = chan;
                self.poisoned = false;
                true
            }
            Err(OpenFailure { stranded: true, .. }) => {
                self.strikes = RESPAWN_CAP;
                false
            }
            // Still poisoned, one attempt poorer. `php` was on `PATH` moments
            // ago, so this is a transient failure worth another try later. A
            // child that started but never finished booting (issue #891) is the
            // same strike as one that would not start: the cap bounds how many
            // boot waits a broken interpreter can cost.
            Err(OpenFailure { stranded: false, .. }) => false,
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

    /// Respawns since the last answered request — the count [`RESPAWN_CAP`]
    /// bounds, so `is_poisoned() && strikes() >= RESPAWN_CAP` is a transport
    /// that has stopped replacing its child (issue #783). Reporting only, for
    /// the same reason as [`Self::respawns`].
    #[must_use]
    pub fn strikes(&self) -> u32 {
        self.strikes
    }

    /// Children this instance has lost — every request that ended with the
    /// child dead, silent or desynced, counted where it happens.
    ///
    /// A caller watching [`Self::is_poisoned`] across one request cannot count
    /// these (issue #783): a request that revives a dead child whose
    /// replacement dies too reads poisoned both before and after, and the
    /// second death is invisible to it. Reporting only.
    #[must_use]
    pub fn deaths(&self) -> u32 {
        self.deaths
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
        // An argument with no JSON spelling (a non-finite float) is not a
        // question this transport can ask, and poisoning is not warranted: the
        // child is fine, the request was never askable.
        let Some(params) = fold_params(name, args, strict) else {
            return FoldResult::widen("unrepresentable argument");
        };
        let Some(value) = self.request("fold", params) else {
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

        let line = frame(id, method, params);

        if self.chan.send(&line).is_err() {
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
                // An answer: whatever killed the children before this one, the
                // engine is not in a storm.
                self.strikes = 0;
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
        self.deaths += 1;
        self.chan.kill();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Closing stdin lets a healthy runner exit; killing covers a hung or
        // poisoned child and, on Unix, anything it started. `Channel::close`
        // also reaps it and waits a bounded time for the reader. A reader given
        // up here strands nothing: the instance is gone.
        let _ = self.chan.close();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use super::*;

    fn busy() -> Error {
        Error::from(ErrorKind::ExecutableFileBusy)
    }

    #[test]
    fn a_busy_spawn_is_retried_until_it_succeeds() {
        let mut calls = 0;
        let outcome = spawn_retrying_busy(
            || {
                calls += 1;
                if calls <= 3 { Err(busy()) } else { Ok(calls) }
            },
            Duration::ZERO,
        );
        assert_eq!(outcome.expect("the fourth attempt succeeds"), 4);
    }

    #[test]
    fn a_spawn_that_stays_busy_fails_after_the_attempt_cap() {
        let mut calls = 0;
        let outcome: std::io::Result<()> = spawn_retrying_busy(
            || {
                calls += 1;
                Err(busy())
            },
            Duration::ZERO,
        );
        assert_eq!(outcome.expect_err("still busy").kind(), ErrorKind::ExecutableFileBusy);
        assert_eq!(calls, SPAWN_BUSY_ATTEMPTS);
    }

    #[test]
    fn any_other_error_is_not_retried() {
        let mut calls = 0;
        let outcome: std::io::Result<()> = spawn_retrying_busy(
            || {
                calls += 1;
                Err(Error::from(ErrorKind::NotFound))
            },
            Duration::ZERO,
        );
        assert_eq!(outcome.expect_err("not found").kind(), ErrorKind::NotFound);
        assert_eq!(calls, 1);
    }
}
