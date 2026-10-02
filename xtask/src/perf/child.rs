//! The child-process half of the cold measurement (issue #913).
//!
//! `perf` ran its N cold runs in one process, so `ru_maxrss` — a lifetime
//! high-water mark that includes the harness itself — stopped moving after the
//! first run. Each cold run now executes in a child: the xtask re-invokes itself
//! as the hidden `perf-child <dir> [--no-php]` command (not in `COMMANDS`, so
//! the usage text does not offer it), which does exactly one [`cold_run`] and
//! prints the result on stdout; the parent reads the child's peak from the
//! `rusage` that `wait4` returns with its exit status.
//!
//! Timing stays inside the child, around the same load and analyze phases as
//! before, so process startup is never in the numbers — the harness still does
//! not time a shelled-out binary. The child is the xtask itself, not
//! `steins check`, for the same reason: the measured pipeline is the library
//! path `fp-gate` drives.
//!
//! The stream is one header line, `steins-perf-child 1 <json>`, then the
//! canonical findings serialization verbatim. The parent needs the text, not
//! only its hash, because the determinism oracle diffs two runs' text when
//! they differ. The header carries the byte length and the SHA-256 of the body,
//! so a truncated or interleaved stream is an error rather than a hash that
//! happens to differ.
//!
//! What `ru_maxrss` covers: the child's own peak, and — Linux and macOS both
//! fold a reaped descendant's peak in as a maximum, never a sum — the PHP
//! sidecar's, which is a few tens of MB and never the larger of the two.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};

use serde::{Deserialize, Serialize};

use super::{ColdRun, Posture, RunTiming, WORKER_STACK_SIZE, cold_run};
use crate::sha256;

/// The hidden subcommand [`spawn_cold`] re-invokes the xtask as.
pub const CHILD_COMMAND: &str = "perf-child";

/// The first line's prefix; the version moves when the header's meaning does.
const MAGIC: &str = "steins-perf-child 1";

/// What the child reports besides the findings text.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    files: usize,
    skipped_links: usize,
    load_ms: f64,
    analyze_ms: f64,
    /// SHA-256 of the body, hex — the stream's own integrity check.
    findings_sha256: String,
    serialized_bytes: usize,
    id_counts: BTreeMap<String, usize>,
}

/// The stream a child prints for `run`.
fn encode(run: &ColdRun) -> Vec<u8> {
    let header = Header {
        files: run.files,
        skipped_links: run.skipped_links,
        load_ms: run.timing.load_ms,
        analyze_ms: run.timing.analyze_ms,
        findings_sha256: sha256::hex(run.serialized.as_bytes()),
        serialized_bytes: run.serialized.len(),
        id_counts: run.id_counts.clone(),
    };
    let json = serde_json::to_string(&header).expect("the header serializes");
    let mut out = format!("{MAGIC} {json}\n").into_bytes();
    out.extend_from_slice(run.serialized.as_bytes());
    out
}

/// A child's stream back into a [`ColdRun`] (without its peak RSS, which the
/// parent reads from the exit status, not the stream).
fn decode(stream: &[u8]) -> Result<ColdRun, String> {
    let newline = stream
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("the perf-child stream has no header line")?;
    let line = std::str::from_utf8(&stream[..newline])
        .map_err(|e| format!("the perf-child header is not UTF-8: {e}"))?;
    let json = line.strip_prefix(MAGIC).and_then(|rest| rest.strip_prefix(' ')).ok_or_else(|| {
        let begins: String = line.chars().take(40).collect();
        format!("not a `{MAGIC}` stream; it begins `{begins}`")
    })?;
    let header: Header = serde_json::from_str(json)
        .map_err(|e| format!("the perf-child header is malformed: {e}"))?;
    let body = &stream[newline + 1..];
    if body.len() != header.serialized_bytes {
        return Err(format!(
            "the perf-child stream is truncated or padded: the header promises {} bytes of findings, {} arrived",
            header.serialized_bytes,
            body.len()
        ));
    }
    if sha256::hex(body) != header.findings_sha256 {
        return Err(
            "the perf-child findings do not hash to the header's findings_sha256".to_owned()
        );
    }
    let serialized = String::from_utf8(body.to_vec())
        .map_err(|e| format!("the perf-child findings are not UTF-8: {e}"))?;
    Ok(ColdRun {
        files: header.files,
        skipped_links: header.skipped_links,
        timing: RunTiming {
            load_ms: header.load_ms,
            analyze_ms: header.analyze_ms,
            total_ms: header.load_ms + header.analyze_ms,
        },
        serialized,
        id_counts: header.id_counts,
        peak_rss_bytes: None,
    })
}

/// `perf-child <dir> [--no-php]`: one cold run, its stream on stdout. Runs on a
/// worker thread sized per [`WORKER_STACK_SIZE`], as `perf` always has.
pub fn run_child(args: &[String]) -> Result<(), String> {
    let mut dir = None;
    let mut posture = Posture::Php;
    for a in args {
        match a.as_str() {
            "--no-php" => posture = Posture::NoPhp,
            other if other.starts_with("--") => {
                return Err(format!("{CHILD_COMMAND}: unknown flag `{other}`"));
            }
            d if dir.is_none() => dir = Some(d.to_owned()),
            _ => return Err(format!("{CHILD_COMMAND}: expected one target directory")),
        }
    }
    let dir = dir.ok_or_else(|| format!("usage: {CHILD_COMMAND} <dir> [--no-php]"))?;
    let run = std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || cold_run(Path::new(&dir), posture))
        .expect("failed to spawn the perf-child worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&encode(&run))
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("{CHILD_COMMAND}: writing the result: {e}"))
}

/// One cold run of `dir` in a child process, its peak RSS attached.
pub fn spawn_cold(dir: &Path, posture: Posture) -> Result<ColdRun, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the xtask binary: {e}"))?;
    let mut command = Command::new(exe);
    command.arg(CHILD_COMMAND).arg(dir);
    if posture == Posture::NoPhp {
        command.arg("--no-php");
    }
    collect_run(command)
}

/// Run `command` to completion and decode its stdout as a [`ColdRun`]; split
/// from [`spawn_cold`] so the protocol is testable against a stand-in child.
fn collect_run(command: Command) -> Result<ColdRun, String> {
    let done = collect(command)?;
    if !done.status.success() {
        return Err(format!(
            "the perf-child process failed ({}); its stderr is above",
            done.status
        ));
    }
    let mut run = decode(&done.stdout)?;
    run.peak_rss_bytes = done.peak_rss_bytes;
    Ok(run)
}

/// A finished child: its status, everything it wrote to stdout, and its peak
/// resident set where the platform reports one.
struct Collected {
    status: ExitStatus,
    stdout: Vec<u8>,
    peak_rss_bytes: Option<u64>,
}

/// Spawn `command` with stdout piped (stderr inherited), drain the pipe, then
/// reap the child *ourselves* so the `rusage` survives: `Child::wait` discards
/// it, and `ru_maxrss` is only there.
fn collect(mut command: Command) -> Result<Collected, String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("cannot spawn the perf-child: {e}"))?;
    let mut stdout = Vec::new();
    let drained = child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_end(&mut stdout);
    // Reap before reporting a read error, so a failed drain leaves no zombie.
    let (status, peak_rss_bytes) =
        wait_with_peak(&mut child).map_err(|e| format!("waiting for the perf-child: {e}"))?;
    drained.map_err(|e| format!("reading the perf-child's stdout: {e}"))?;
    Ok(Collected { status, stdout, peak_rss_bytes })
}

/// Reap `child` with `wait4`. The `Child` is spent afterwards: the pid is
/// already reaped, so it must not be waited on again (dropping it is fine).
#[cfg(unix)]
fn wait_with_peak(child: &mut Child) -> std::io::Result<(ExitStatus, Option<u64>)> {
    use std::os::unix::process::ExitStatusExt;

    let pid = child.id() as libc::pid_t;
    let mut raw_status: libc::c_int = 0;
    // SAFETY: an all-zero `rusage` is a valid value (plain integers and
    // timevals), and `wait4` only writes through the two pointers it is given,
    // both of which outlive the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let reaped = unsafe { libc::wait4(pid, &mut raw_status, 0, &mut usage) };
        if reaped == pid {
            break;
        }
        let err = std::io::Error::last_os_error();
        if reaped < 0 && err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err);
    }
    let peak = maxrss_bytes(usage.ru_maxrss as u64, RSS_UNIT_BYTES);
    Ok((ExitStatus::from_raw(raw_status), Some(peak)))
}

/// Without `wait4` there is no peak to read; the run is still measured.
#[cfg(not(unix))]
fn wait_with_peak(child: &mut Child) -> std::io::Result<(ExitStatus, Option<u64>)> {
    child.wait().map(|status| (status, None))
}

/// How many bytes one unit of `ru_maxrss` is: macOS reports bytes, Linux and the
/// other Unixes report KiB.
const RSS_UNIT_BYTES: u64 =
    if cfg!(any(target_os = "macos", target_os = "ios")) { 1 } else { 1024 };

/// `ru_maxrss` as a byte count, given the platform's unit.
fn maxrss_bytes(raw: u64, unit_bytes: u64) -> u64 {
    raw.saturating_mul(unit_bytes)
}

/// Bytes as the harness's `MB`: 10⁶ bytes, the decimal unit its `GB` figures use.
pub fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ColdRun {
        let mut id_counts = BTreeMap::new();
        id_counts.insert("type.mismatch".to_owned(), 2);
        ColdRun {
            files: 7,
            skipped_links: 1,
            timing: RunTiming { load_ms: 12.5, analyze_ms: 100.25, total_ms: 112.75 },
            serialized: "{\"id\":\"type.mismatch\",\"path\":\"a.php\"}\n{\"id\":\"x\"}\n"
                .to_owned(),
            id_counts,
            peak_rss_bytes: None,
        }
    }

    #[test]
    fn a_stream_round_trips_exactly() {
        let run = sample();
        let back = decode(&encode(&run)).expect("decodes");
        assert_eq!(back.files, run.files);
        assert_eq!(back.skipped_links, run.skipped_links);
        assert_eq!(back.serialized, run.serialized);
        assert_eq!(back.id_counts, run.id_counts);
        assert_eq!(back.timing.load_ms, 12.5);
        assert_eq!(back.timing.analyze_ms, 100.25);
        assert_eq!(back.timing.total_ms, 112.75);
        assert_eq!(back.peak_rss_bytes, None);
    }

    #[test]
    fn an_empty_findings_body_round_trips() {
        let mut run = sample();
        run.serialized.clear();
        run.id_counts.clear();
        let back = decode(&encode(&run)).expect("decodes");
        assert!(back.serialized.is_empty());
    }

    #[test]
    fn a_damaged_stream_is_an_error_not_a_different_hash() {
        let good = encode(&sample());
        assert!(decode(b"").unwrap_err().contains("no header"));
        let noise = decode(b"warning: noise\n{}\n").unwrap_err();
        assert!(noise.contains("not a `steins-perf-child 1`"), "{noise}");
        assert!(decode(b"steins-perf-child 1 {\n").unwrap_err().contains("malformed"));
        // Truncated: the header promises more bytes than arrived.
        let cut = &good[..good.len() - 3];
        assert!(decode(cut).unwrap_err().contains("truncated"));
        // Same length, different bytes: the hash catches it.
        let mut flipped = good.clone();
        let last = flipped.len() - 2;
        flipped[last] ^= 1;
        assert!(decode(&flipped).unwrap_err().contains("do not hash"));
        // A newer protocol version is refused, not misread.
        let newer = String::from_utf8(good).unwrap().replacen("perf-child 1", "perf-child 2", 1);
        assert!(decode(newer.as_bytes()).unwrap_err().contains("not a `steins-perf-child 1`"));
    }

    #[test]
    fn the_units_normalise_to_bytes() {
        // macOS: the raw figure is already bytes.
        assert_eq!(maxrss_bytes(123_456_789, 1), 123_456_789);
        // Linux: the raw figure is KiB.
        assert_eq!(maxrss_bytes(120_563, 1024), 123_456_512);
        assert_eq!(maxrss_bytes(u64::MAX, 1024), u64::MAX);
        assert_eq!(bytes_to_mb(123_456_789), 123.456789);
    }

    /// The reap path against a stand-in child that speaks the protocol: the
    /// stdout arrives intact, the exit status is the child's, and a peak is read.
    #[cfg(unix)]
    #[test]
    fn collect_run_decodes_a_child_and_reads_its_peak() {
        let stream = encode(&sample());
        let dir = std::env::temp_dir().join(format!("steins-perf-child-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("stream");
        std::fs::write(&file, &stream).expect("write the stream");
        let mut command = Command::new("cat");
        command.arg(&file);
        let run = collect_run(command).expect("a well-formed child decodes");
        assert_eq!(run.serialized, sample().serialized);
        let peak = run.peak_rss_bytes.expect("wait4 reports a peak");
        assert!(peak > 0, "cat has a resident set");

        // A failing child is an error naming its status, whatever it printed.
        let mut failing = Command::new("sh");
        failing.arg("-c").arg("cat \"$0\"; exit 3").arg(&file);
        let err = collect_run(failing).unwrap_err();
        assert!(err.contains("failed"), "{err}");

        // A child that prints something else is a protocol error.
        let mut chatty = Command::new("echo");
        chatty.arg("hello");
        assert!(collect_run(chatty).unwrap_err().contains("not a `steins-perf-child 1`"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
