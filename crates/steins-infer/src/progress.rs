//! The opt-in progress channel of `steins check` (issue #885): phase
//! boundaries and slow files, reported while the run is still going.
//!
//! A [`Progress`] is a handle the caller builds and the pipeline reports
//! through. It owns no output: the caller supplies a sink that receives one
//! finished line at a time (without a trailing newline), so the library never
//! writes to a stream the caller has not chosen — the CLI's output seam stays
//! the only writer. The default handle is off, and an off handle reads no
//! clock, takes no lock and formats nothing, which is what keeps a run without
//! the flag byte-identical to a run before the channel existed.
//!
//! Two kinds of line come through:
//!
//! - **Phase boundaries**, said as each is crossed: the elapsed time of the
//!   phase just finished (measured from the previous boundary) and the time
//!   since the run began. Phases are strictly sequential, so one lap clock
//!   shared by the handle is enough.
//! - **Slow files**, said when one file's walk took at least the handle's
//!   threshold. The walk fans out over workers, so these arrive from several
//!   threads; every line is handed to the sink whole, and the sink is
//!   responsible for writing it whole.
//!
//! A third thing is **readable from another thread** (issue #658): a
//! [`Progress::snapshot`] names the last phase crossed and every file whose
//! walk has started and not yet ended, so a watchdog can say *where* a run is
//! stuck without the stuck thread's cooperation. A slow-file line only prints
//! once a walk returns; the snapshot is the signal for a walk that never does.
//! The library still writes nothing: the reader is the caller's.
//!
//! Progress is cost-only: nothing here reads or changes a finding.

use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// A handle to the progress channel; cheap to clone, and off by default.
#[derive(Clone, Default)]
pub struct Progress {
    inner: Option<Arc<Channel>>,
}

struct Channel {
    sink: Box<dyn Fn(&str) + Send + Sync>,
    slow_file: Duration,
    started: Instant,
    /// When the previous phase boundary was crossed, and which one it was.
    lap: Mutex<Lap>,
    /// The files being walked right now, one entry per walk in progress. A
    /// parallel walk has one per worker, so an entry is keyed by the worker's
    /// thread and the list is as long as the fan-out is wide.
    in_flight: Mutex<Vec<Walking>>,
}

struct Lap {
    at: Instant,
    /// The phase that ended at [`Self::at`]; `None` before the first boundary.
    phase: Option<String>,
}

struct Walking {
    thread: ThreadId,
    path: String,
    since: Instant,
}

/// What a run is doing, read from another thread (issue #658).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// The last phase the run finished, or `None` if it has not finished one.
    /// Phases are said as they *end*, so the phase running now is the one
    /// after it: this names where the run was last known to be.
    pub last_phase: Option<String>,
    /// How long ago that boundary was crossed (or the run began, if none was).
    pub since_phase: Duration,
    /// Time since the run began.
    pub elapsed: Duration,
    /// Every file whose walk has begun and not ended, longest-running first.
    pub in_flight: Vec<InFlightFile>,
}

/// One file being walked at the moment of a [`ProgressSnapshot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlightFile {
    /// The file's diagnostic path.
    pub path: String,
    /// How long its walk has been running.
    pub running: Duration,
}

impl Progress {
    /// The handle that reports nothing.
    #[must_use]
    pub fn off() -> Self {
        Self { inner: None }
    }

    /// A handle that hands each line to `sink`, naming a file whose walk took
    /// at least `slow_file`. The run's clock starts now.
    ///
    /// Not available on the browser build, which has no clock to start and
    /// never reports progress.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static, slow_file: Duration) -> Self {
        let now = Instant::now();
        Self {
            inner: Some(Arc::new(Channel {
                sink: Box::new(sink),
                slow_file,
                started: now,
                lap: Mutex::new(Lap { at: now, phase: None }),
                in_flight: Mutex::new(Vec::new()),
            })),
        }
    }

    /// A phase boundary: `phase` has just finished.
    pub fn phase(&self, phase: &str) {
        self.phase_with(phase, String::new);
    }

    /// [`Self::phase`] with a short `detail` appended in parentheses. The
    /// detail is built only when the handle reports, so an off handle formats
    /// nothing.
    pub fn phase_with(&self, phase: &str, detail: impl FnOnce() -> String) {
        let Some(channel) = &self.inner else { return };
        let now = Instant::now();
        let took = {
            let mut lap = channel.lap.lock().unwrap_or_else(PoisonError::into_inner);
            let took = now.duration_since(lap.at);
            *lap = Lap { at: now, phase: Some(phase.to_owned()) };
            took
        };
        let total = now.duration_since(channel.started);
        let detail = detail();
        let detail = if detail.is_empty() { detail } else { format!(", {detail}") };
        (channel.sink)(&format!("{phase}: {} (elapsed {}{detail})", span(took), span(total)));
    }

    /// A file's walk starts: record it as in flight until the returned guard
    /// drops. With nothing reporting the guard holds nothing (no clock, no
    /// lock). `path` is the file's diagnostic path.
    pub(crate) fn file_start<'a>(&'a self, path: &'a str) -> FileWalk<'a> {
        let started = self.inner.as_ref().map(|channel| {
            let since = Instant::now();
            let thread = std::thread::current().id();
            let walking = Walking { thread, path: path.to_owned(), since };
            channel.in_flight.lock().unwrap_or_else(PoisonError::into_inner).push(walking);
            since
        });
        FileWalk { progress: self, path, started }
    }

    /// What the run is doing now, readable from any thread; `None` for an off
    /// handle. It takes the two short locks a walk takes at a file's start and
    /// end, so it never waits on a walk itself, only on another reader or a
    /// boundary.
    #[must_use]
    pub fn snapshot(&self) -> Option<ProgressSnapshot> {
        let channel = self.inner.as_ref()?;
        let now = Instant::now();
        let (last_phase, lap_at) = {
            let lap = channel.lap.lock().unwrap_or_else(PoisonError::into_inner);
            (lap.phase.clone(), lap.at)
        };
        let mut in_flight: Vec<InFlightFile> = channel
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|w| InFlightFile { path: w.path.clone(), running: now.duration_since(w.since) })
            .collect();
        in_flight.sort_by(|a, b| b.running.cmp(&a.running).then_with(|| a.path.cmp(&b.path)));
        Some(ProgressSnapshot {
            last_phase,
            since_phase: now.duration_since(lap_at),
            elapsed: now.duration_since(channel.started),
            in_flight,
        })
    }
}

/// A file whose walk is in progress, from [`Progress::file_start`]. The file
/// is in flight until this drops, so an unwind through the walk cannot leave a
/// stale entry behind.
pub(crate) struct FileWalk<'a> {
    progress: &'a Progress,
    path: &'a str,
    started: Option<Instant>,
}

impl FileWalk<'_> {
    /// The walk returned: name the file if it was slow, then let it leave the
    /// in-flight list (on drop).
    ///
    /// The file leaves *after* its slow-file line is handed over, so a reader
    /// that sees the line can still see the file, and a sink that waits for a
    /// reader (a watchdog's test) has something to read.
    pub(crate) fn done(self) {
        let (Some(channel), Some(started)) = (&self.progress.inner, self.started) else { return };
        let took = started.elapsed();
        if took >= channel.slow_file {
            (channel.sink)(&format!("slow file: {} walked in {}", self.path, span(took)));
        }
    }
}

impl Drop for FileWalk<'_> {
    fn drop(&mut self) {
        let (Some(channel), Some(_)) = (&self.progress.inner, self.started) else { return };
        let thread = std::thread::current().id();
        let mut walking = channel.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = walking.iter().position(|w| w.thread == thread && w.path == self.path) {
            walking.swap_remove(at);
        }
    }
}

/// A duration in the one spelling every progress line uses.
fn span(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collected(slow: Duration) -> (Progress, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let progress = Progress::new(move |line| sink.lock().unwrap().push(line.to_owned()), slow);
        (progress, lines)
    }

    #[test]
    fn an_off_handle_is_silent_and_clockless() {
        let off = Progress::off();
        off.phase("parse");
        off.phase_with("walk", || unreachable!("an off handle builds no detail"));
        off.file_start("a.php").done();
        assert!(off.snapshot().is_none(), "an off handle has nothing to read");
    }

    #[test]
    fn phases_report_in_order_with_their_detail() {
        let (progress, lines) = collected(Duration::from_millis(250));
        progress.phase("parse");
        progress.phase_with("walk", || "3 file(s)".to_owned());
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("parse: ") && lines[0].contains("(elapsed "));
        assert!(lines[1].starts_with("walk: ") && lines[1].ends_with(", 3 file(s))"));
    }

    #[test]
    fn only_a_file_at_or_over_the_threshold_is_named() {
        let (never, lines) = collected(Duration::from_secs(3600));
        never.file_start("quick.php").done();
        assert!(lines.lock().unwrap().is_empty());
        let (always, lines) = collected(Duration::ZERO);
        always.file_start("src/Slow.php").done();
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("slow file: src/Slow.php walked in "));
    }

    #[test]
    fn a_file_is_in_flight_between_its_start_and_its_end() {
        let (progress, _) = collected(Duration::from_secs(3600));
        let before = progress.snapshot().expect("an on handle reads");
        assert_eq!(before.last_phase, None);
        assert!(before.in_flight.is_empty());

        let walk = progress.file_start("src/Hot.php");
        let during = progress.snapshot().unwrap();
        let paths: Vec<&str> = during.in_flight.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["src/Hot.php"]);

        walk.done();
        assert!(progress.snapshot().unwrap().in_flight.is_empty());
    }

    /// A walk that unwinds leaves nothing behind (the guard drops).
    #[test]
    fn an_unwound_walk_is_no_longer_in_flight() {
        let (progress, _) = collected(Duration::from_secs(3600));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _walk = progress.file_start("src/Boom.php");
            assert_eq!(progress.snapshot().unwrap().in_flight.len(), 1);
            panic!("the walk panics");
        }));
        assert!(caught.is_err());
        assert!(progress.snapshot().unwrap().in_flight.is_empty());
    }

    #[test]
    fn the_last_phase_is_the_last_boundary_crossed() {
        let (progress, _) = collected(Duration::from_secs(3600));
        progress.phase("parse");
        progress.phase("universe");
        let seen = progress.snapshot().unwrap();
        assert_eq!(seen.last_phase.as_deref(), Some("universe"));
        assert!(seen.since_phase <= seen.elapsed);
    }

    /// The fleet shape: several workers each hold a file at once, and the
    /// reader names all of them, longest-running first, then sees each leave.
    #[test]
    fn every_workers_file_is_named_while_they_all_run() {
        use std::sync::Barrier;
        const WORKERS: usize = 4;
        let (progress, _) = collected(Duration::from_secs(3600));
        let all_started = Barrier::new(WORKERS + 1);
        let read = Barrier::new(WORKERS + 1);
        std::thread::scope(|scope| {
            for w in 0..WORKERS {
                let (progress, all_started, read) = (&progress, &all_started, &read);
                scope.spawn(move || {
                    let path = format!("src/W{w}.php");
                    let walk = progress.file_start(&path);
                    all_started.wait();
                    read.wait();
                    walk.done();
                });
            }
            all_started.wait();
            let during = progress.snapshot().unwrap();
            let mut paths: Vec<String> = during.in_flight.iter().map(|f| f.path.clone()).collect();
            assert!(
                during.in_flight.windows(2).all(|w| w[0].running >= w[1].running),
                "longest-running first"
            );
            paths.sort();
            let expected: Vec<String> = (0..WORKERS).map(|w| format!("src/W{w}.php")).collect();
            assert_eq!(paths, expected);
            read.wait();
        });
        assert!(progress.snapshot().unwrap().in_flight.is_empty());
    }

    #[test]
    fn two_walks_of_one_path_on_different_threads_end_independently() {
        let (progress, _) = collected(Duration::from_secs(3600));
        let here = progress.file_start("a.php");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                progress.file_start("a.php").done();
            });
        });
        assert_eq!(progress.snapshot().unwrap().in_flight.len(), 1, "this thread's walk remains");
        here.done();
        assert!(progress.snapshot().unwrap().in_flight.is_empty());
    }
}
