//! Stamp the short git revision and the build date into the binary for the
//! `version` banner (#43).
//!
//! Both are best-effort by design. A build from a published tarball has no
//! working tree and reports `unknown`; a reproducible build pins the date
//! through `SOURCE_DATE_EPOCH` rather than reading the wall clock. Neither
//! failure is worth breaking a build over — the banner is informational, and
//! the license obligations the same command carries do not depend on it.
//!
//! No dependency is added: the civil-date conversion is a few lines below, so
//! this behaves identically on every release target.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=STEINS_GIT_REV={rev}");

    // `SOURCE_DATE_EPOCH` is the reproducible-build contract: honor it when set,
    // and only then fall back to the wall clock.
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
        });
    let (y, m, d) = civil_from_days((epoch / 86_400) as i64);
    println!("cargo:rustc-env=STEINS_BUILD_DATE={y:04}-{m:02}-{d:02}");

    // Re-run when the checked-out commit changes, so the stamped revision cannot
    // silently describe an older tree than the binary was built from.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

/// Days since the Unix epoch to a civil `(year, month, day)` — Howard Hinnant's
/// `civil_from_days`. Inline so no crate and no `date` binary is involved, and
/// so cross-compiled targets stamp the same value as a native build.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}
