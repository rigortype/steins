//! `mine-param-facts`: build the committed per-parameter facts table from the
//! **running engine's own arginfo** (issue #382).
//!
//! # Why this table exists
//!
//! Two of the catalog's tables make claims about parameters — `out_params`
//! (which positions are by-ref, ADR-0077) and `invocation_shape` (which position
//! is a callback, ADR-0033) — and both were transcribed from php-src's stubs by
//! hand. Nothing checked them, and the check that was attempted could not work:
//! `by_value_arg` falls back to `out_params`, so a name with **no** row answers
//! "by value" at every position and a loop keyed on it skips precisely the
//! omission it is looking for. A test written that way passes vacuously.
//!
//! The fix is an independent source, which is why this reads
//! `ReflectionFunction` off the resident engine rather than re-parsing the same
//! stubs a second time: a second transcription would agree with the first
//! wherever the first is wrong. The engine's arginfo is what PHP itself
//! dispatches on.
//!
//! # What is mined, and what is kept
//!
//! Every internal function the engine has is *mined*. A name is **kept with its
//! full row** when it carries a hazard — a by-ref, declared-callable or variadic
//! position — or when the catalog reasons about it (the folding allowlist,
//! passed in by this command). Everything else is kept as a **name only**, in
//! the `plain` list, and that list is load-bearing: "this name was mined and
//! carries nothing" has to be a recorded fact, or the completeness tests are
//! back to reading absence as agreement.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-param-facts
//! ```
//!
//! Output: `docs/research/phpsrc-mining/param_facts.toml` (source of record).
//! `cargo xtask gen-catalog` turns it into the shipped Rust table. Rerun both
//! alongside a `PINNED_PHP` bump, the way `hierarchy.toml` is regenerated.
//!
//! The mined universe is **this build's** — an unloaded extension is a name that
//! is not there. `[meta] extensions` records which build answered, and names the
//! catalog knows but the build lacks are listed rather than silently missing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

use crate::corpus::repo_root;

/// The miner's JSON shape.
#[derive(serde::Deserialize)]
struct Mined {
    php: String,
    extensions: Vec<String>,
    internal_total: usize,
    unreflectable: Vec<String>,
    absent: Vec<String>,
    rows: BTreeMap<String, Row>,
    plain: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Row {
    by_ref: Vec<usize>,
    callable: Vec<usize>,
    variadic: Vec<usize>,
    optional: Vec<usize>,
    params: Vec<String>,
    params_required: usize,
}

/// Entry point for `cargo xtask mine-param-facts`.
pub fn run() -> Result<(), String> {
    // The names the catalog reasons about, so their rows are kept even when they
    // carry no hazard — which is the usual case for a foldable name and exactly
    // the fact the fold-side tests need to be able to read.
    let mut keep: Vec<&str> = Vec::new();
    keep.extend_from_slice(steins_catalog::portable_names());
    keep.extend_from_slice(steins_catalog::refused_names());
    keep.extend_from_slice(steins_catalog::unverified_names());
    let keep_json = serde_json::to_string(&keep).map_err(|e| format!("encode name list: {e}"))?;

    let mined = run_miner(&keep_json)?;
    println!(
        "mine-param-facts: PHP {} — {} internal functions, {} rows kept, {} plain",
        mined.php,
        mined.internal_total,
        mined.rows.len(),
        mined.plain.len(),
    );
    if !mined.unreflectable.is_empty() {
        return Err(format!(
            "{} names the engine lists but cannot reflect: {:?} — refusing to mine a partial table",
            mined.unreflectable.len(),
            mined.unreflectable
        ));
    }
    if !mined.absent.is_empty() {
        println!(
            "mine-param-facts: {} catalog names this build does not have: {:?}",
            mined.absent.len(),
            mined.absent
        );
    }

    let hazardous = mined.rows.values().filter(|r| r.hazardous()).count();
    let out = render(&mined, hazardous);
    let dst = repo_root().join("docs/research/phpsrc-mining/param_facts.toml");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!(
        "mine-param-facts: {} rows ({hazardous} carrying a hazard) + {} plain names → {}",
        mined.rows.len(),
        mined.plain.len(),
        dst.display()
    );
    Ok(())
}

impl Row {
    /// Whether the row carries something a fold argument cannot be: a by-ref
    /// position (the seam passes by value), a declared-callable one (the
    /// argument would be a second callee), or a variadic one (the comparator
    /// families put their callback in exactly that tail).
    fn hazardous(&self) -> bool {
        !self.by_ref.is_empty() || !self.callable.is_empty() || !self.variadic.is_empty()
    }
}

/// Run the PHP miner with the catalog's name list.
fn run_miner(keep_json: &str) -> Result<Mined, String> {
    let script = repo_root().join("docs/research/phpsrc-mining/mine_param_facts.php");
    let out = Command::new("php")
        .arg(&script)
        .arg(keep_json)
        .output()
        .map_err(|e| format!("run php {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!("miner failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse miner JSON: {e}"))
}

/// A TOML-safe quoted key or string: the only characters an internal function
/// name can carry beyond `[a-z0-9_]` are namespace separators.
fn toml_key(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the TOML source of record.
fn render(mined: &Mined, hazardous: usize) -> String {
    let mut s = String::new();
    s.push_str(
        "# Builtin PER-PARAMETER FACTS — the independent source `out_params` and\n\
         # `invocation_shape` are checked against (issue #382).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-param-facts`, which runs\n\
         # `mine_param_facts.php` against the resident engine and reads every internal\n\
         # function's own arginfo through `ReflectionFunction`. Regenerate alongside a\n\
         # `PINNED_PHP` bump, the way `hierarchy.toml` is — never by hand.\n\
         #\n\
         # WHY THE ENGINE AND NOT THE STUBS: the two tables this checks were transcribed\n\
         # from php-src's stubs by hand, and a second transcription of the same stubs\n\
         # would agree with them wherever they are wrong. Arginfo is what PHP dispatches\n\
         # on.\n\
         #\n\
         # SCOPE. Every internal function of the build named in `[meta]`. A name with a\n\
         # by-ref, declared-callable or variadic position gets a full row; so does every\n\
         # name on the folding allowlist, hazard or not. Everything else is a name in\n\
         # `[plain] names`, and that list is load-bearing: a completeness test that read\n\
         # absence as agreement is the vacuity this table was built to remove.\n\
         #\n\
         # A `callable` position is one whose DECLARED type admits a callable. It is a\n\
         # sound marker, not a complete one: `array_udiff` takes its comparator at a\n\
         # variadic `mixed` tail, and `preg_replace_callback_array` takes its callables\n\
         # as array VALUES. Both are caught here by their other hazards (variadic,\n\
         # by-ref), which is why the fold-side rule reads all three columns.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "php = \"{}\"", mined.php);
    let _ = writeln!(s, "miner = \"docs/research/phpsrc-mining/mine_param_facts.php\"");
    let _ = writeln!(s, "generator = \"cargo xtask mine-param-facts\"");
    let _ = writeln!(s, "extensions = [");
    for e in &mined.extensions {
        let _ = writeln!(s, "  \"{e}\",");
    }
    let _ = writeln!(s, "]\n");

    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "# internal_functions  what the build had, before any filtering");
    let _ = writeln!(s, "# rows                names kept with their full parameter facts");
    let _ = writeln!(s, "# hazardous           of those, the ones carrying by-ref/callable/variadic");
    let _ = writeln!(s, "# plain               names mined and recorded as carrying nothing");
    let _ = writeln!(s, "# catalog_absent      names the catalog knows and this build does not have");
    let _ = writeln!(s, "internal_functions = {}", mined.internal_total);
    let _ = writeln!(s, "rows = {}", mined.rows.len());
    let _ = writeln!(s, "hazardous = {hazardous}");
    let _ = writeln!(s, "plain = {}", mined.plain.len());
    let _ = writeln!(s, "catalog_absent = {}", mined.absent.len());
    if !mined.absent.is_empty() {
        let _ = writeln!(s, "catalog_absent_names = {:?}", mined.absent);
    }
    s.push('\n');

    for (name, r) in &mined.rows {
        // Quoted and escaped: an extension can declare a NAMESPACED internal
        // function (`ast\\get_kind_name`), and a bare TOML key would read its
        // backslash as an escape.
        let _ = writeln!(s, "[fn.{}]", toml_key(name));
        let _ = writeln!(s, "by_ref = {:?}", r.by_ref);
        let _ = writeln!(s, "callable = {:?}", r.callable);
        let _ = writeln!(s, "variadic = {:?}", r.variadic);
        let _ = writeln!(s, "optional = {:?}", r.optional);
        let _ = writeln!(s, "params = {:?}", r.params);
        let _ = writeln!(s, "params_required = {}", r.params_required);
        s.push('\n');
    }

    s.push_str("# Mined, and carrying no by-ref, callable or variadic position.\n");
    let _ = writeln!(s, "[plain]");
    let _ = writeln!(s, "names = [");
    for n in &mined.plain {
        let _ = writeln!(s, "  {},", toml_key(n));
    }
    let _ = writeln!(s, "]");
    s
}
