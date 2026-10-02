//! `steins check --progress` (issue #885): the opt-in progress channel on
//! stderr, so a slow run can be told apart from a hung one.
//!
//! The pipeline reports through [`steins_infer::Progress`] and owns no output;
//! this module is the one place the CLI decides whether the channel is on, how
//! slow a file must be to be named, and where a line goes — through `errln!`,
//! which writes a whole line under the stderr lock, so the walk's parallel
//! workers never interleave inside one. Off, the handle reads no clock and
//! writes nothing, and a run is byte-identical to one that never had the flag.

use std::time::Duration;

use steins_infer::Progress;

/// The walk time at or above which a file is named when
/// [`SLOW_MS_ENV`] does not say otherwise.
const DEFAULT_SLOW_FILE: Duration = Duration::from_millis(250);

/// Setting this to `1` turns the channel on, as `--progress` does. Only `1`
/// does, as for `STEINS_GENERATIONS_PARANOID`; any other value leaves it off.
pub(crate) const PROGRESS_ENV: &str = "STEINS_PROGRESS";

/// The walk time, in milliseconds, at or above which a file is named
/// (default 250). `0` names every file.
pub(crate) const SLOW_MS_ENV: &str = "STEINS_PROGRESS_SLOW_MS";

/// The handle for one `check` run: on when `flag` is set or the environment
/// asks, off otherwise. The environment is read here, once.
pub(crate) fn progress_for(flag: bool) -> Progress {
    if !flag && !std::env::var(PROGRESS_ENV).is_ok_and(|v| v == "1") {
        return Progress::off();
    }
    let slow = std::env::var(SLOW_MS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_SLOW_FILE, Duration::from_millis);
    Progress::new(|line| errln!("steins: progress: {line}"), slow)
}
