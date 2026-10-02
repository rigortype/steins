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
//! Progress is cost-only: nothing here reads or changes a finding.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The default threshold above which one file's walk is named: 250 ms.
pub const DEFAULT_SLOW_FILE: Duration = Duration::from_millis(250);

/// A handle to the progress channel; cheap to clone, and off by default.
#[derive(Clone, Default)]
pub struct Progress {
    inner: Option<Arc<Channel>>,
}

struct Channel {
    sink: Box<dyn Fn(&str) + Send + Sync>,
    slow_file: Duration,
    started: Instant,
    /// When the previous phase boundary was crossed.
    lap: Mutex<Instant>,
}

impl Progress {
    /// The handle that reports nothing.
    #[must_use]
    pub fn off() -> Self {
        Self { inner: None }
    }

    /// A handle that hands each line to `sink`, naming a file whose walk took
    /// at least `slow_file`. The run's clock starts now.
    #[must_use]
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static, slow_file: Duration) -> Self {
        let now = Instant::now();
        Self {
            inner: Some(Arc::new(Channel {
                sink: Box::new(sink),
                slow_file,
                started: now,
                lap: Mutex::new(now),
            })),
        }
    }

    /// Whether this handle reports anything.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.inner.is_some()
    }

    /// A phase boundary: `phase` has just finished.
    pub fn phase(&self, phase: &str) {
        self.phase_with(phase, "");
    }

    /// [`Self::phase`] with a short `detail` appended in parentheses.
    pub fn phase_with(&self, phase: &str, detail: &str) {
        let Some(channel) = &self.inner else { return };
        let now = Instant::now();
        let took = {
            let mut lap = channel.lap.lock().unwrap_or_else(PoisonError::into_inner);
            let took = now.duration_since(*lap);
            *lap = now;
            took
        };
        let total = now.duration_since(channel.started);
        let detail = if detail.is_empty() { String::new() } else { format!(", {detail}") };
        (channel.sink)(&format!("{phase}: {} (elapsed {}{detail})", span(took), span(total)));
    }

    /// The clock a file's walk starts on, or `None` when nothing reports.
    pub(crate) fn file_clock(&self) -> Option<Instant> {
        self.inner.as_ref().map(|_| Instant::now())
    }

    /// A file's walk is done: name it if it was slow. `path` is the file's
    /// diagnostic path.
    pub(crate) fn file_done(&self, path: &str, started: Option<Instant>) {
        let (Some(channel), Some(started)) = (&self.inner, started) else { return };
        let took = started.elapsed();
        if took >= channel.slow_file {
            (channel.sink)(&format!("slow file: {path} walked in {}", span(took)));
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
        assert!(off.file_clock().is_none());
        off.file_done("a.php", None);
        assert!(!off.is_on());
    }

    #[test]
    fn phases_report_in_order_with_their_detail() {
        let (progress, lines) = collected(DEFAULT_SLOW_FILE);
        progress.phase("parse");
        progress.phase_with("walk", "3 file(s)");
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("parse: ") && lines[0].contains("(elapsed "));
        assert!(lines[1].starts_with("walk: ") && lines[1].ends_with(", 3 file(s))"));
    }

    #[test]
    fn only_a_file_at_or_over_the_threshold_is_named() {
        let (never, lines) = collected(Duration::from_secs(3600));
        never.file_done("quick.php", never.file_clock());
        assert!(lines.lock().unwrap().is_empty());
        let (always, lines) = collected(Duration::ZERO);
        always.file_done("src/Slow.php", always.file_clock());
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("slow file: src/Slow.php walked in "));
    }
}
