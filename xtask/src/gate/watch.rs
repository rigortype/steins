//! What `fp-gate` says while it runs, and the deadline that ends a stuck
//! project (issue #658).
//!
//! Until this existed the gate was all-or-nothing: one project that never
//! finished left the whole run silent until CI killed it, with no word on which
//! project or which file. Two things here fix that.
//!
//! # Progress
//!
//! Each project says so on **stderr** when it starts and when it finishes, with
//! its wall time and the phase split the analyzer's [`Progress`] channel
//! reported for each pass (cold, warm). A file whose walk takes longer than the
//! slow-file threshold (5 s by default, `STEINS_PROGRESS_SLOW_MS` overrides) is
//! named as it is found. The gate's **stdout** is the report, parsed and
//! compared by people and CI, and is not touched.
//!
//! # The deadline
//!
//! `cargo xtask fp-gate --deadline SECS` (default [`DEFAULT_DEADLINE_SECS`],
//! `0` disables) bounds how long any one project may run, cold and warm pass
//! together. A watchdog thread checks the projects in flight; one past its
//! deadline is reported — project, elapsed time, the pass, the last phase the
//! analyzer finished and **every file whose walk has begun and not ended** — and
//! the process exits with [`EXIT_DEADLINE`].
//!
//! The analysis runs in-process on rayon workers, so there is nothing to kill
//! but the process: a thread stuck in a pure-CPU loop cannot be interrupted, and
//! waiting for it to return is exactly the hang the deadline exists to end. The
//! watchdog therefore flushes stdout and calls [`std::process::exit`], which
//! does not unwind the stuck workers and leaves the scratch stores under
//! `target/fp-gate-stores/` in place (the next run wipes them before its cold
//! pass, so nothing depends on their removal).
//!
//! The deadline lives here and nowhere in the engine: the engine's cutoffs are
//! structural, and a wall-clock budget inside it would make a finding depend on
//! the machine. A harness may fail a run for being slow; it must say why.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use steins_infer::{Progress, ProgressSnapshot};

/// The per-project deadline when `--deadline` does not say otherwise, in
/// seconds. The pinned public corpus finishes each project in well under a
/// minute and the largest private one in minutes; this is the wall past which a
/// run is stuck rather than slow.
pub const DEFAULT_DEADLINE_SECS: u64 = 1200;

/// The exit status of a gate ended by its deadline. Distinct from a red verdict
/// (1) and from a command that could not run (2), so CI can tell a stall from a
/// finding.
pub const EXIT_DEADLINE: u8 = 3;

/// The walk time at which a slow file is named when
/// [`SLOW_MS_ENV`] does not say otherwise.
const DEFAULT_SLOW_FILE: Duration = Duration::from_secs(5);

/// The walk time, in milliseconds, at or above which a file is named (the same
/// variable `steins check --progress` reads). `0` names every file.
const SLOW_MS_ENV: &str = "STEINS_PROGRESS_SLOW_MS";

/// How often the watchdog looks, at most. Enough that a deadline is enforced to
/// within about a second without a thread that wakes constantly.
const MAX_POLL: Duration = Duration::from_secs(1);

/// The deadline `args` ask for: `--deadline SECS` or `--deadline=SECS`, where
/// `SECS` may be fractional and `0` means no deadline.
///
/// # Errors
///
/// An argument that is not `--deadline`, a missing or unreadable value, or a
/// negative or non-finite one.
pub fn deadline_from_args(args: &[String]) -> Result<Option<Duration>, String> {
    let mut secs = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let value = if arg == "--deadline" {
            rest.next().ok_or("`--deadline` needs a number of seconds")?.as_str()
        } else if let Some(v) = arg.strip_prefix("--deadline=") {
            v
        } else {
            return Err(format!(
                "unknown fp-gate argument `{arg}` (usage: fp-gate [--deadline SECS])"
            ));
        };
        let parsed: f64 = value
            .parse()
            .map_err(|_| format!("`--deadline {value}` is not a number of seconds"))?;
        secs = Some(Duration::try_from_secs_f64(parsed).map_err(|_| {
            format!("`--deadline {value}` must be a finite, non-negative number of seconds")
        })?);
    }
    let deadline = secs.unwrap_or(Duration::from_secs(DEFAULT_DEADLINE_SECS));
    Ok((!deadline.is_zero()).then_some(deadline))
}

/// A project past its deadline, as the watchdog read it.
#[derive(Debug)]
pub struct Expiry {
    pub project: String,
    /// `cold` or `warm`, or `setup` before the first pass began.
    pub pass: &'static str,
    pub elapsed: Duration,
    pub deadline: Duration,
    /// What the pass's progress channel held; `None` before a pass began.
    pub snapshot: Option<ProgressSnapshot>,
}

impl Expiry {
    /// The report the gate prints before it exits: what ran too long, where it
    /// last was, and what to run next.
    pub fn render(&self) -> String {
        let mut out = format!(
            "fp-gate: DEADLINE EXCEEDED: project `{}` has run for {} (deadline {}); \
             the gate is failed, not hung",
            self.project,
            secs(self.elapsed),
            secs(self.deadline),
        );
        let Some(snapshot) = &self.snapshot else {
            out.push_str(&format!(
                "\n  pass: {} (no phase finished yet, so the stall is before the analysis began)",
                self.pass
            ));
            return out;
        };
        let phase = snapshot.last_phase.as_deref().map_or_else(
            || "no phase finished yet".to_owned(),
            |p| format!("last phase finished: `{p}`, {} ago", secs(snapshot.since_phase)),
        );
        out.push_str(&format!(
            "\n  pass: {} ({} in); {phase} (phases are named as they end, so the running one \
             is the next)",
            self.pass,
            secs(snapshot.elapsed),
        ));
        if snapshot.in_flight.is_empty() {
            out.push_str(
                "\n  files in flight: none, so the stall is outside the per-file walk (see the \
                 phase above)",
            );
        } else {
            out.push_str(&format!("\n  files in flight ({}):", snapshot.in_flight.len()));
            for file in &snapshot.in_flight {
                out.push_str(&format!("\n    {} (walking for {})", file.path, secs(file.running)));
            }
            out.push_str(
                "\n  next step: re-run `steins check --progress --no-cache --profile strict \
                 <file>` on the longest-running file (paths are relative to the project root)",
            );
        }
        out.push_str(
            "\n  `--deadline SECS` changes the limit and `--deadline 0` removes it; the \
             engine itself has no wall-clock budget",
        );
        out
    }
}

/// Seconds, to a hundredth: the one spelling the gate's progress lines use.
fn secs(d: Duration) -> String {
    format!("{:.2} s", d.as_secs_f64())
}

/// A project the watchdog is timing.
struct Entry {
    id: u64,
    project: String,
    started: Instant,
    pass: &'static str,
    progress: Option<Progress>,
}

/// The thread that ends a project that outstays its deadline.
pub struct Watchdog {
    deadline: Duration,
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Watchdog {
    /// Start watching with `deadline`, looking every `poll`; `on_expiry` gets
    /// the first project past its deadline and the thread ends. In the gate
    /// `on_expiry` exits the process; a test collects the report instead.
    fn spawn(
        deadline: Duration,
        poll: Duration,
        on_expiry: impl Fn(Expiry) + Send + 'static,
    ) -> Arc<Self> {
        let dog = Arc::new(Self {
            deadline,
            entries: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let watching = Arc::clone(&dog);
        std::thread::Builder::new()
            .name("fp-gate-watchdog".to_owned())
            .spawn(move || watching.keep_watch(poll, &on_expiry))
            .expect("the OS refused a thread for the fp-gate watchdog");
        dog
    }

    fn keep_watch(&self, poll: Duration, on_expiry: &dyn Fn(Expiry)) {
        loop {
            {
                let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
                let (stopped, _) = self
                    .wake
                    .wait_timeout_while(stopped, poll, |stopped| !*stopped)
                    .unwrap_or_else(PoisonError::into_inner);
                if *stopped {
                    return;
                }
            }
            if let Some(expiry) = self.check(Instant::now()) {
                on_expiry(expiry);
                return;
            }
        }
    }

    /// The oldest project that has run for the deadline as of `now`, if any.
    pub(super) fn check(&self, now: Instant) -> Option<Expiry> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .filter(|e| now.saturating_duration_since(e.started) >= self.deadline)
            .min_by_key(|e| e.started)
            .map(|e| Expiry {
                project: e.project.clone(),
                pass: e.pass,
                elapsed: now.saturating_duration_since(e.started),
                deadline: self.deadline,
                snapshot: e.progress.as_ref().and_then(Progress::snapshot),
            })
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    fn register(&self, project: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = Entry {
            id,
            project: project.to_owned(),
            started: Instant::now(),
            pass: "setup",
            progress: None,
        };
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).push(entry);
        id
    }

    fn set_pass(&self, id: u64, pass: &'static str, progress: &Progress) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.pass = pass;
            entry.progress = Some(progress.clone());
        }
    }

    fn deregister(&self, id: u64) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).retain(|e| e.id != id);
    }
}

/// The phase lines one pass's progress handle reported.
type PhaseLines = Arc<Mutex<Vec<String>>>;

/// Where the gate's progress lines go, one whole line per call.
pub(super) type Out = Arc<dyn Fn(&str) + Send + Sync>;

/// The gate's progress reporting and deadline, shared by every project.
pub struct Watch {
    dog: Option<Arc<Watchdog>>,
    slow_file: Duration,
    out: Out,
}

impl Watch {
    /// The gate's own: lines on stderr, and a deadline that ends the process
    /// with [`EXIT_DEADLINE`] (`None`: no deadline).
    pub fn start(deadline: Option<Duration>) -> Self {
        let slow_file = std::env::var(SLOW_MS_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(DEFAULT_SLOW_FILE, Duration::from_millis);
        let out: Out = Arc::new(|line| eprintln!("{line}"));
        Self::with(deadline, slow_file, out, |expiry| {
            eprintln!("{}", expiry.render());
            // The workers are stuck, so this cannot wait for them: see the
            // module docs. stdout is flushed so nothing already printed is lost.
            let _ = std::io::stdout().flush();
            std::process::exit(i32::from(EXIT_DEADLINE));
        })
    }

    /// [`Self::start`] with the output and the expiry action chosen.
    pub(super) fn with(
        deadline: Option<Duration>,
        slow_file: Duration,
        out: Out,
        on_expiry: impl Fn(Expiry) + Send + 'static,
    ) -> Self {
        let dog = deadline.map(|d| {
            let poll = (d / 10).clamp(Duration::from_millis(1), MAX_POLL);
            Watchdog::spawn(d, poll, on_expiry)
        });
        Self { dog, slow_file, out }
    }

    /// The watchdog, for a test that reads it at a time of its choosing.
    #[cfg(test)]
    pub(super) fn watchdog(&self) -> Option<&Arc<Watchdog>> {
        self.dog.as_ref()
    }

    /// A project starts: say so, and begin timing it against the deadline. The
    /// returned guard stops the timing when it drops.
    pub fn begin(&self, project: &str, files: usize) -> Running<'_> {
        (self.out)(&format!("fp-gate: {project}: start ({files} file(s))"));
        Running {
            watch: self,
            project: project.to_owned(),
            started: Instant::now(),
            id: self.dog.as_ref().map(|dog| dog.register(project)),
            passes: Mutex::new(Vec::new()),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(dog) = &self.dog {
            dog.stop();
        }
    }
}

/// One project under watch: hands out the progress handle of each pass and
/// reports the project's end.
pub struct Running<'w> {
    watch: &'w Watch,
    project: String,
    started: Instant,
    id: Option<u64>,
    /// Each pass's label and the phase lines its handle reported.
    passes: Mutex<Vec<(&'static str, PhaseLines)>>,
}

impl Running<'_> {
    /// The progress handle for the pass called `label`. Its phase lines are
    /// kept for [`Self::finish`]; a slow file is said at once; and the deadline
    /// watches it from here on.
    pub fn pass(&self, label: &'static str) -> Progress {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let kept = (label, Arc::clone(&phases));
        self.passes.lock().unwrap_or_else(PoisonError::into_inner).push(kept);
        let out = Arc::clone(&self.watch.out);
        let project = self.project.clone();
        let progress = Progress::new(
            move |line| {
                if line.starts_with("slow file: ") {
                    out(&format!("fp-gate: {project}: {label}: {line}"));
                } else {
                    phases.lock().unwrap_or_else(PoisonError::into_inner).push(line.to_owned());
                }
            },
            self.watch.slow_file,
        );
        if let (Some(dog), Some(id)) = (&self.watch.dog, self.id) {
            dog.set_pass(id, label, &progress);
        }
        progress
    }

    /// The project is done: its wall time and each pass's phase split.
    pub fn finish(&self, cold: Duration, warm: Duration) {
        let passes = self.passes.lock().unwrap_or_else(PoisonError::into_inner);
        let times = |label: &str| if label == "cold" { cold } else { warm };
        let split: Vec<String> = passes
            .iter()
            .map(|(label, lines)| {
                let lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
                format!("{label} {}: {}", secs(times(label)), phase_split(&lines))
            })
            .collect();
        (self.watch.out)(&format!(
            "fp-gate: {}: done in {} ({})",
            self.project,
            secs(self.started.elapsed()),
            split.join("; ")
        ));
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if let (Some(dog), Some(id)) = (&self.watch.dog, self.id) {
            dog.deregister(id);
        }
    }
}

/// The phase lines a [`Progress`] reported, as `name 1.2 s, name 0.3 s`: each
/// line's own phase and duration, without the running total and details.
fn phase_split(lines: &[String]) -> String {
    let phases: Vec<String> = lines
        .iter()
        .map(|line| {
            let head = line.split(" (").next().unwrap_or(line);
            head.split_once(": ")
                .and_then(|(name, took)| {
                    let ms: f64 = took.strip_suffix(" ms")?.parse().ok()?;
                    Some(format!("{name} {}", secs(Duration::from_secs_f64(ms / 1000.0))))
                })
                .unwrap_or_else(|| head.to_owned())
        })
        .collect();
    if phases.is_empty() { "no phases reported".to_owned() } else { phases.join(", ") }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::corpus_local::LocalProject;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_deadline_flag() {
        let default = Some(Duration::from_secs(DEFAULT_DEADLINE_SECS));
        assert_eq!(deadline_from_args(&[]), Ok(default));
        assert_eq!(
            deadline_from_args(&args(&["--deadline", "30"])),
            Ok(Some(Duration::from_secs(30)))
        );
        assert_eq!(
            deadline_from_args(&args(&["--deadline=0.5"])),
            Ok(Some(Duration::from_millis(500)))
        );
        assert_eq!(deadline_from_args(&args(&["--deadline", "0"])), Ok(None));
        for bad in [&["--deadline"][..], &["--deadline", "soon"], &["--deadline=-1"], &["--dead"]] {
            assert!(deadline_from_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_phase_split_drops_totals_and_details() {
        let lines = vec![
            "parse: 216.4 ms (elapsed 233.5 ms)".to_owned(),
            "walk: 2500.0 ms (elapsed 4000.0 ms, 270 of 270 file(s) walked)".to_owned(),
        ];
        assert_eq!(phase_split(&lines), "parse 0.22 s, walk 2.50 s");
        assert_eq!(phase_split(&[]), "no phases reported");
    }

    /// The split is read off what a real handle says, so a change to the
    /// channel's spelling fails here rather than printing raw lines.
    #[test]
    fn a_phase_split_reads_what_the_channel_reports() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let progress = Progress::new(
            move |line| sink.lock().unwrap().push(line.to_owned()),
            Duration::from_secs(3600),
        );
        progress.phase("parse");
        progress.phase_with("walk", || "3 file(s)".to_owned());
        let split = phase_split(&lines.lock().unwrap());
        assert_eq!(split, "parse 0.00 s, walk 0.00 s");
    }

    /// A watch whose expiries are handed to the returned receiver instead of
    /// ending the process, and whose lines are collected.
    fn collecting(
        deadline: Duration,
        slow_file: Duration,
    ) -> (Watch, mpsc::Receiver<Expiry>, Arc<Mutex<Vec<String>>>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let out: Out = Arc::new(move |line| sink.lock().unwrap().push(line.to_owned()));
        let watch = Watch::with(Some(deadline), slow_file, out, move |expiry| {
            let _ = tx.lock().unwrap().send(expiry);
        });
        (watch, rx, lines)
    }

    #[test]
    fn a_project_past_its_deadline_is_reported_with_its_pass_and_phase() {
        let (watch, expired, _) = collecting(Duration::from_millis(5), DEFAULT_SLOW_FILE);
        let running = watch.begin("acme/widgets", 3);
        let progress = running.pass("cold");
        progress.phase("parse");
        let expiry = expired.recv_timeout(Duration::from_secs(60)).expect("the deadline fires");
        assert_eq!(expiry.project, "acme/widgets");
        assert_eq!(expiry.pass, "cold");
        assert!(expiry.elapsed >= expiry.deadline);
        let text = expiry.render();
        assert!(text.contains("DEADLINE EXCEEDED: project `acme/widgets`"), "{text}");
        assert!(text.contains("last phase finished: `parse`"), "{text}");
        assert!(text.contains("files in flight: none"), "{text}");
    }

    #[test]
    fn a_finished_project_is_not_reported() {
        let (watch, expired, _) = collecting(Duration::from_millis(5), DEFAULT_SLOW_FILE);
        drop(watch.begin("acme/widgets", 1));
        // The watchdog polls every millisecond; let it look many times.
        assert!(expired.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn the_check_names_the_oldest_project_at_the_deadline_and_no_other() {
        let (watch, _, _) = collecting(Duration::from_secs(3600), DEFAULT_SLOW_FILE);
        let dog = watch.dog.as_ref().unwrap();
        let first = watch.begin("first", 1);
        let _second = watch.begin("second", 1);
        let now = Instant::now();
        assert!(dog.check(now).is_none(), "nothing has run for an hour");
        let later = dog.check(now + Duration::from_secs(3601)).expect("both are past it");
        assert_eq!(later.project, "first", "the one that began first");
        drop(first);
        let later = dog.check(now + Duration::from_secs(3601)).unwrap();
        assert_eq!(later.project, "second");
        assert_eq!(later.snapshot, None, "no pass began, so nothing to read");
        assert!(later.render().contains("before the analysis began"));
    }

    #[test]
    fn a_finished_project_says_its_wall_time_and_phase_split() {
        let (watch, _, lines) = collecting(Duration::from_secs(3600), DEFAULT_SLOW_FILE);
        let running = watch.begin("acme/widgets", 2);
        running.pass("cold").phase("parse");
        running.pass("warm").phase("walk");
        running.finish(Duration::from_millis(1500), Duration::from_millis(500));
        let lines = lines.lock().unwrap();
        assert_eq!(lines[0], "fp-gate: acme/widgets: start (2 file(s))");
        assert!(lines[1].starts_with("fp-gate: acme/widgets: done in "), "{}", lines[1]);
        assert!(lines[1].ends_with("(cold 1.50 s: parse 0.00 s; warm 0.50 s: walk 0.00 s)"));
    }

    /// The deadline path end to end, through the gate's own project driver: a
    /// real analysis of a one-file project, with the output sink reading the
    /// watchdog while the file is in flight, as of a time past the deadline.
    /// The report must name the project, the pass and the file. Nothing here
    /// waits on a clock.
    #[test]
    fn a_project_held_in_a_file_is_named_with_that_file_by_the_deadline() {
        use std::sync::OnceLock;

        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("steins-fp-gate-deadline-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("composer.json"), r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#)
            .unwrap();
        let source = "<?php\nnamespace App;\nfunction f(): int { return 1; }\n";
        std::fs::write(dir.join("src/Stuck.php"), source).unwrap();
        let name = format!("fixture/stuck-{pid}");
        let path = toml::Value::from(dir.display().to_string());
        let project: LocalProject =
            toml::from_str(&format!("name = \"{name}\"\npath = {path}\n")).unwrap();

        // The watchdog's own thread must not get there first: its deadline is
        // an hour. The sink reads it as of two hours on.
        let dog: Arc<OnceLock<Arc<Watchdog>>> = Arc::new(OnceLock::new());
        let reader = Arc::clone(&dog);
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let said = Arc::clone(&lines);
        let held = Arc::new(Mutex::new(None::<Expiry>));
        let holder = Arc::clone(&held);
        let out: Out = Arc::new(move |line| {
            said.lock().unwrap().push(line.to_owned());
            if line.contains(": slow file: ") {
                let at = Instant::now() + Duration::from_secs(7200);
                // The first file only: the warm pass names its own later.
                holder.lock().unwrap().get_or_insert_with(|| {
                    reader.get().and_then(|dog| dog.check(at)).expect("past the deadline")
                });
            }
        });
        let watch = Watch::with(Some(Duration::from_secs(3600)), Duration::ZERO, out, |_| ());
        dog.set(Arc::clone(watch.watchdog().unwrap())).ok();

        let report = super::super::analyze_local(&project, &[], &watch);
        let _ = std::fs::remove_dir_all(&dir);
        let store = super::super::store_root(&crate::corpus::repo_root(), &name);
        let _ = std::fs::remove_dir_all(store);

        assert_eq!(report.file_count, 1);
        let expiry = held.lock().unwrap().take().expect("a file was named while in flight");
        assert_eq!(expiry.project, name);
        assert_eq!(expiry.pass, "cold");
        let text = expiry.render();
        assert!(text.contains("files in flight (1):\n    src/Stuck.php (walking for "), "{text}");
        assert!(text.contains("last phase finished: `purity oracle`"), "{text}");
        assert!(text.contains("steins check --progress"), "{text}");
        let lines = lines.lock().unwrap();
        assert_eq!(lines[0], format!("fp-gate: {name}: start (1 file(s))"));
        let done = lines.last().unwrap();
        assert!(done.starts_with(&format!("fp-gate: {name}: done in ")), "{done}");
        assert!(done.contains("(cold ") && done.contains("; warm "), "{done}");
    }
}
