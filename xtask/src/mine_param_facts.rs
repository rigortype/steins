//! `mine-param-facts`: build the committed per-parameter facts table from the
//! **engines' own arginfo** (issue #382).
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
//! `ReflectionFunction` off a resident engine rather than re-parsing the same
//! stubs a second time: a second transcription would agree with the first
//! wherever the first is wrong. The engine's arginfo is what PHP itself
//! dispatches on.
//!
//! # What is mined, and what is kept
//!
//! Every internal function the engines have is mined, and **every one gets a full
//! row**. "This name was mined and carries nothing" has to be a recorded fact —
//! otherwise the completeness tests read absence as agreement, which is the
//! vacuity this table exists to remove.
//!
//! An earlier cut kept full rows only for names carrying a hazard or sitting on
//! the folding allowlist, and recorded the rest as bare names. That was enough
//! for those tests and not enough for the table's other consumer:
//! `cargo xtask fold-probe --names <name>` generates its tuples from these
//! facts, so a name with no row cannot be probed — and the names worth probing
//! are exactly the ones not yet admitted. A table that answers only about what
//! is already decided is no use for deciding.
//!
//! # One universe per PLATFORM, and the union across them (issue #703)
//!
//! The mined universe used to be one build's, and one build is one operating
//! system: `chroot` is a Linux builtin, no Darwin PHP has it at any minor, and a
//! macOS-mined table therefore answered nothing for it — 29 nsrt rows downstream
//! of one `is_string` guard stayed `unknown` because `by_value_arg('chroot', …)`
//! had no row to answer from.
//!
//! So the run takes several engines (`--php PATH`, repeatable) and **unions**
//! their rows, each row recording the `PHP_OS_FAMILY` values that have it. Three
//! rules keep the union from inventing a fact no engine stated:
//!
//! * **A by-ref disagreement is refused, never merged** ([`Refusal`]).
//!   Which positions are `&$x` is the fact `out_params` is checked against, and
//!   two engines that answer differently are two different functions wearing one
//!   name — a merged row would be a claim neither engine made. Recorded by name
//!   with what each said.
//! * **The other hazards union.** A `callable` or variadic position any engine
//!   declares is a position the fold seam must not touch, and the sound direction
//!   for a hazard is the wider set.
//! * **The spellings come from the newest engine that has the name.** `params`,
//!   `param_names`, `optional` and `params_required` describe a signature, and
//!   half of one engine's signature spliced onto another's describes nothing.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-param-facts [--php PATH]… [--merge TOML]…
//! ```
//!
//! `--php PATH` (repeatable) names the engines to mine; with none given the run
//! asks the `php` on `PATH` alone, which is the single-engine run this command
//! has always been. `--merge TOML` reads a previously-mined `param_facts.toml`
//! as one more source — which is how a **Linux** engine reaches this table from a
//! machine that has none: `.github/workflows/ci.yml`'s `param-facts-linux` job
//! mines one on `ubuntu-latest` and uploads the TOML, and
//! `docs/agents/mining.md` says how to bring it down and merge it.
//!
//! Output: `docs/research/phpsrc-mining/param_facts.toml` (source of record).
//! `cargo xtask gen-catalog` turns it into the shipped Rust table. Rerun both
//! alongside a `PINNED_PHP` bump, the way `hierarchy.toml` is regenerated.
//!
//! `[meta] engines` records which builds answered, with their host families, and
//! names the catalog knows but no build has are listed rather than silently
//! missing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::Command;

use crate::corpus::repo_root;

/// The miner's JSON shape.
#[derive(serde::Deserialize)]
struct Mined {
    php: String,
    /// `PHP_OS_FAMILY` — the host family this build's universe belongs to.
    #[serde(default = "unknown_os")]
    os: String,
    extensions: Vec<String>,
    internal_total: usize,
    unreflectable: Vec<String>,
    absent: Vec<String>,
    rows: BTreeMap<String, Row>,
}

/// The family a source that predates the `os` field belongs to. Only a merged
/// TOML mined before issue #703 can reach this, and calling it `Unknown` is the
/// honest reading — php-src's own sixth `PHP_OS_FAMILY` value, for a host it
/// could not classify.
fn unknown_os() -> String {
    "Unknown".to_owned()
}

#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
struct Row {
    by_ref: Vec<usize>,
    callable: Vec<usize>,
    variadic: Vec<usize>,
    optional: Vec<usize>,
    params: Vec<String>,
    param_names: Vec<String>,
    params_required: usize,
}

/// One source's answers: an engine the run asked, or a TOML a previous run wrote.
struct Source {
    php: String,
    os: String,
    mined: Mined,
}

impl Source {
    /// How `[meta] engines` and a row's `platforms` name this source.
    fn label(&self) -> String {
        format!("{} ({})", self.php, self.os)
    }
}

/// One admitted row of the union, and which host families stated it.
struct Merged {
    row: Row,
    platforms: BTreeSet<String>,
}

/// Why a mined name is not in the table. One variant today, and it is the point
/// of the union having a refusal at all: a merge that cannot refuse is a merge
/// that invents.
struct Refusal {
    /// What each source said the by-ref positions were, in the order asked.
    seen: Vec<(String, Vec<usize>)>,
}

/// Entry point for `cargo xtask mine-param-facts`.
pub fn run(php_bins: &[String], merges: &[String]) -> Result<(), String> {
    let mut sources = Vec::new();
    for bin in php_binaries(php_bins) {
        let mined = run_miner(&bin, "[]")?;
        println!(
            "mine-param-facts: PHP {} on {} — {} internal functions, {} rows",
            mined.php,
            mined.os,
            mined.internal_total,
            mined.rows.len(),
        );
        if !mined.unreflectable.is_empty() {
            return Err(format!(
                "{} names PHP {} lists but cannot reflect: {:?} — refusing to mine a partial table",
                mined.unreflectable.len(),
                mined.php,
                mined.unreflectable
            ));
        }
        sources.push(Source { php: mined.php.clone(), os: mined.os.clone(), mined });
    }
    for path in merges {
        let mined = read_merge(path)?;
        println!(
            "mine-param-facts: merged PHP {} on {} from {path} — {} rows",
            mined.php,
            mined.os,
            mined.rows.len()
        );
        sources.push(Source { php: mined.php.clone(), os: mined.os.clone(), mined });
    }
    if sources.is_empty() {
        return Err("no engine to mine and nothing to merge".to_owned());
    }

    let (rows, refused) = union(&sources);
    for (name, r) in &refused {
        println!(
            "mine-param-facts: `{name}` is refused — the sources disagree about its by-ref \
             positions: {:?}",
            r.seen
        );
    }
    // A name no source has is absent from the WHOLE union, which is a different
    // fact from "absent on the build that happened to answer".
    let absent: Vec<String> = sources
        .iter()
        .flat_map(|s| s.mined.absent.iter().cloned())
        .filter(|n| !rows.contains_key(n))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !absent.is_empty() {
        println!("mine-param-facts: {} catalog names no build has: {absent:?}", absent.len());
    }

    let hazardous = rows.values().filter(|m| m.row.hazardous()).count();
    let out = render(&sources, &rows, &refused, &absent, hazardous);
    let dst = repo_root().join("docs/research/phpsrc-mining/param_facts.toml");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!(
        "mine-param-facts: {} rows ({hazardous} carrying a hazard, {} refused) → {}",
        rows.len(),
        refused.len(),
        dst.display()
    );
    Ok(())
}

/// **The union of every source's rows**, and the names they could not be unioned
/// for.
///
/// Sources are read in the order given, and the LAST one that has a name supplies
/// its spellings — the same "top engine answers" rule `mine-constants` uses, for
/// the same reason: a signature is one engine's or it is nobody's.
fn union(sources: &[Source]) -> (BTreeMap<String, Merged>, BTreeMap<String, Refusal>) {
    let mut rows: BTreeMap<String, Merged> = BTreeMap::new();
    let mut by_ref_seen: BTreeMap<String, Vec<(String, Vec<usize>)>> = BTreeMap::new();
    let mut refused: BTreeMap<String, Refusal> = BTreeMap::new();
    for s in sources {
        for (name, row) in &s.mined.rows {
            by_ref_seen
                .entry(name.clone())
                .or_default()
                .push((s.label(), row.by_ref.clone()));
            match rows.get_mut(name) {
                None => {
                    rows.insert(
                        name.clone(),
                        Merged {
                            row: row.clone(),
                            platforms: BTreeSet::from([s.os.clone()]),
                        },
                    );
                }
                Some(have) => {
                    if have.row.by_ref != row.by_ref {
                        // Refused, not merged: `out_params` is checked against
                        // exactly this column, and two answers is no answer.
                        refused.insert(
                            name.clone(),
                            Refusal { seen: by_ref_seen[name].clone() },
                        );
                        continue;
                    }
                    // A hazard any engine declares is a hazard; the spellings are
                    // the newest engine's, whole.
                    let callable = union_positions(&have.row.callable, &row.callable);
                    let variadic = union_positions(&have.row.variadic, &row.variadic);
                    have.row = Row { callable, variadic, ..row.clone() };
                    have.platforms.insert(s.os.clone());
                }
            }
        }
    }
    for name in refused.keys() {
        rows.remove(name);
    }
    (rows, refused)
}

/// Two position lists, merged and sorted. Small and by hand: the lists are a
/// handful of entries and a `BTreeSet` round trip reads worse than this does.
fn union_positions(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = a.iter().chain(b).copied().collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// **The engines to ask**: every `--php PATH`, else the `php` on `PATH` alone.
fn php_binaries(php_bins: &[String]) -> Vec<String> {
    if php_bins.is_empty() { vec!["php".to_owned()] } else { php_bins.to_vec() }
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

/// Run the PHP miner on one engine with the catalog's name list.
fn run_miner(bin: &str, keep_json: &str) -> Result<Mined, String> {
    let script = repo_root().join("docs/research/phpsrc-mining/mine_param_facts.php");
    let out = Command::new(bin)
        .arg(&script)
        .arg(keep_json)
        .output()
        .map_err(|e| format!("run {bin} {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!("miner failed on {bin}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse miner JSON: {e}"))
}

/// Read a previously-mined `param_facts.toml` as one more source — the seam a
/// Linux engine reaches this table through from a machine that has none.
///
/// The TOML's own shape is read back, not a second format: whatever a mining run
/// wrote is what a merge takes, so the CI job runs the same command this one does
/// and its output needs no conversion.
fn read_merge(path: &str) -> Result<Mined, String> {
    #[derive(serde::Deserialize)]
    struct Doc {
        meta: Meta,
        counts: Counts,
        #[serde(default)]
        r#fn: BTreeMap<String, Row>,
    }
    #[derive(serde::Deserialize)]
    struct Meta {
        php: String,
        #[serde(default = "unknown_os")]
        os: String,
        #[serde(default)]
        extensions: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct Counts {
        internal_functions: usize,
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let doc: Doc = toml::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
    Ok(Mined {
        php: doc.meta.php,
        os: doc.meta.os,
        extensions: doc.meta.extensions,
        internal_total: doc.counts.internal_functions,
        unreflectable: Vec::new(),
        absent: Vec::new(),
        rows: doc.r#fn,
    })
}

/// A TOML-safe quoted key or string: the only characters an internal function
/// name can carry beyond `[a-z0-9_]` are namespace separators.
fn toml_key(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the TOML source of record.
fn render(
    sources: &[Source],
    rows: &BTreeMap<String, Merged>,
    refused: &BTreeMap<String, Refusal>,
    absent: &[String],
    hazardous: usize,
) -> String {
    let top = sources.last().expect("at least one source");
    let mut s = String::new();
    s.push_str(
        "# Builtin PER-PARAMETER FACTS — the independent source `out_params` and\n\
         # `invocation_shape` are checked against (issue #382).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-param-facts`, which runs\n\
         # `mine_param_facts.php` against every engine it is given and reads each\n\
         # internal function's own arginfo through `ReflectionFunction`. Regenerate\n\
         # alongside a `PINNED_PHP` bump, the way `hierarchy.toml` is — never by hand.\n\
         #\n\
         # WHY THE ENGINE AND NOT THE STUBS: the two tables this checks were transcribed\n\
         # from php-src's stubs by hand, and a second transcription of the same stubs\n\
         # would agree with them wherever they are wrong. Arginfo is what PHP dispatches\n\
         # on.\n\
         #\n\
         # SCOPE. Every internal function of the builds named in `[meta] engines`, each\n\
         # with a full row. `Mined, and carrying nothing` is a recorded fact rather than\n\
         # an absence — a completeness test that read absence as agreement is the vacuity\n\
         # this table was built to remove — and a row for every name is also what lets\n\
         # `cargo xtask fold-probe --names <name>` probe a CANDIDATE, which is the whole\n\
         # point of having a candidate.\n\
         #\n\
         # ONE UNIVERSE PER PLATFORM (issue #703). A build is one operating system, and\n\
         # `chroot` is a Linux builtin no Darwin PHP has at any minor — so the rows are\n\
         # the UNION across the engines given, and each records the `PHP_OS_FAMILY`\n\
         # values that stated it. Where the sources disagree: a by-ref disagreement is\n\
         # REFUSED rather than merged (`[refused.by_ref]`), since that column is exactly\n\
         # what `out_params` is checked against and a merged answer would be a claim\n\
         # neither engine made; `callable` and `variadic` union, because the sound\n\
         # direction for a hazard is the wider set; the signature spellings come whole\n\
         # from the last source that has the name.\n\
         #\n\
         # A `callable` position is one whose DECLARED type admits a callable. It is a\n\
         # sound marker, not a complete one: `array_udiff` takes its comparator at a\n\
         # variadic `mixed` tail, and `preg_replace_callback_array` takes its callables\n\
         # as array VALUES. Both are caught here by their other hazards (variadic,\n\
         # by-ref), which is why the fold-side rule reads all three columns.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "php = \"{}\"", top.php);
    let _ = writeln!(s, "os = \"{}\"", top.os);
    let _ = writeln!(s, "miner = \"docs/research/phpsrc-mining/mine_param_facts.php\"");
    let _ = writeln!(s, "generator = \"cargo xtask mine-param-facts\"");
    // The builds' VERSIONS and host families, never their paths: a nix store path
    // or a Homebrew cellar is a directory of the mining machine, and the version
    // with the family is the whole of what a reader needs.
    let _ = writeln!(s, "# Every build the union asked, in the order asked; `php`/`os` above are");
    let _ = writeln!(s, "# the LAST of them, whose signature spellings the shared rows carry.");
    let _ = writeln!(s, "engines = [");
    for src in sources {
        let _ = writeln!(s, "  [\"{}\", \"{}\"],", src.php, src.os);
    }
    let _ = writeln!(s, "]");
    // The extension set of the top build: `mine-constants` reads it back as its
    // own allowlist, so the two tables cannot drift into covering different
    // builds.
    let _ = writeln!(s, "extensions = [");
    for e in &top.mined.extensions {
        let _ = writeln!(s, "  \"{e}\",");
    }
    let _ = writeln!(s, "]\n");

    let platforms: BTreeSet<&str> = sources.iter().map(|s| s.os.as_str()).collect();
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "# internal_functions  what the LAST build had, before any filtering");
    let _ = writeln!(s, "# rows                names kept with their full parameter facts");
    let _ = writeln!(s, "# hazardous           of those, the ones carrying by-ref/callable/variadic");
    let _ = writeln!(s, "# platforms           distinct `PHP_OS_FAMILY` values the union covers");
    let _ = writeln!(s, "# refused_by_ref      names the sources disagree about, left out entirely");
    let _ = writeln!(s, "# catalog_absent      names the catalog knows and NO build has");
    let _ = writeln!(s, "internal_functions = {}", top.mined.internal_total);
    let _ = writeln!(s, "rows = {}", rows.len());
    let _ = writeln!(s, "hazardous = {hazardous}");
    let _ = writeln!(s, "platforms = {}", platforms.len());
    let _ = writeln!(s, "refused_by_ref = {}", refused.len());
    let _ = writeln!(s, "catalog_absent = {}", absent.len());
    if !absent.is_empty() {
        let _ = writeln!(s, "catalog_absent_names = {absent:?}");
    }
    s.push('\n');

    // A table and not a list, because the POSITIONS are the evidence: a reviewer
    // who wants to know whether a refusal was right reads what each build said.
    let _ = writeln!(s, "[refused.by_ref]");
    let _ = writeln!(s, "# name = [[\"<php version> (<os>)\", [<position>, …]], …] — every build that");
    let _ = writeln!(s, "# HAS the name, in the order asked. Two spellings here are what refused it.");
    for (name, r) in refused {
        let _ = write!(s, "{} = [", toml_key(name));
        for (i, (label, positions)) in r.seen.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            let _ = write!(s, "{sep}[\"{label}\", {positions:?}]");
        }
        let _ = writeln!(s, "]");
    }
    s.push('\n');

    for (name, m) in rows {
        // Quoted and escaped: an extension can declare a NAMESPACED internal
        // function (`ast\\get_kind_name`), and a bare TOML key would read its
        // backslash as an escape.
        let _ = writeln!(s, "[fn.{}]", toml_key(name));
        let _ = writeln!(s, "by_ref = {:?}", m.row.by_ref);
        let _ = writeln!(s, "callable = {:?}", m.row.callable);
        let _ = writeln!(s, "variadic = {:?}", m.row.variadic);
        let _ = writeln!(s, "optional = {:?}", m.row.optional);
        let _ = writeln!(s, "params = {:?}", m.row.params);
        let _ = writeln!(s, "param_names = {:?}", m.row.param_names);
        let _ = writeln!(s, "params_required = {}", m.row.params_required);
        // The host families that stated this row. A name every build has carries
        // them all; `chroot` carries `Linux` alone, which is what tells a reader
        // that the absence of a Darwin answer is the platform and not the mining.
        let _ = writeln!(s, "platforms = {:?}", m.platforms.iter().collect::<Vec<_>>());
        s.push('\n');
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One source's answers, spelled the way the miner's JSON does.
    fn source(php: &str, os: &str, rows: &[(&str, &[usize], &[usize])]) -> Source {
        let mined = Mined {
            php: php.to_owned(),
            os: os.to_owned(),
            extensions: Vec::new(),
            internal_total: rows.len(),
            unreflectable: Vec::new(),
            absent: Vec::new(),
            rows: rows
                .iter()
                .map(|(name, by_ref, callable)| {
                    (
                        (*name).to_owned(),
                        Row {
                            by_ref: by_ref.to_vec(),
                            callable: callable.to_vec(),
                            variadic: Vec::new(),
                            optional: Vec::new(),
                            params: vec!["string".to_owned()],
                            param_names: vec![(*os).to_owned()],
                            params_required: 1,
                        },
                    )
                })
                .collect(),
        };
        Source { php: mined.php.clone(), os: mined.os.clone(), mined }
    }

    /// **A name only one platform has still gets a row** — the whole of issue
    /// #703. `chroot` is a Linux builtin, and a macOS-only run left
    /// `by_value_arg('chroot', 0)` with nothing to answer from.
    #[test]
    fn a_name_one_platform_has_joins_the_union_with_that_platform_recorded() {
        let sources = vec![
            source("8.5.10", "Darwin", &[("strlen", &[], &[])]),
            source("8.5.10", "Linux", &[("strlen", &[], &[]), ("chroot", &[], &[])]),
        ];
        let (rows, refused) = union(&sources);
        assert!(refused.is_empty());
        assert_eq!(
            rows["chroot"].platforms.iter().map(String::as_str).collect::<Vec<_>>(),
            ["Linux"]
        );
        assert_eq!(
            rows["strlen"].platforms.iter().map(String::as_str).collect::<Vec<_>>(),
            ["Darwin", "Linux"]
        );
    }

    /// **A by-ref disagreement is refused, never merged.** That column is exactly
    /// what `out_params` (ADR-0077) is checked against, so a merged answer would
    /// be a claim neither engine made — and whichever way the merge went, half the
    /// platforms would be reading a wrong row.
    ///
    /// Delete the `by_ref` comparison in [`union`] and this name silently keeps
    /// the first source's positions on every platform.
    #[test]
    fn sources_that_disagree_about_by_ref_refuse_the_name() {
        let sources = vec![
            source("8.5.10", "Darwin", &[("two_faced", &[1], &[])]),
            source("8.5.10", "Linux", &[("two_faced", &[], &[])]),
        ];
        let (rows, refused) = union(&sources);
        assert!(!rows.contains_key("two_faced"), "a refused name is left out entirely");
        assert_eq!(
            refused["two_faced"].seen,
            vec![
                ("8.5.10 (Darwin)".to_owned(), vec![1]),
                ("8.5.10 (Linux)".to_owned(), Vec::new()),
            ]
        );
    }

    /// The other hazards go the sound way: a position any engine declares
    /// callable is a position the fold seam must not touch, so they union rather
    /// than refuse.
    #[test]
    fn a_hazard_either_source_declares_survives_the_union() {
        let sources = vec![
            source("8.5.10", "Darwin", &[("cb", &[], &[1])]),
            source("8.5.10", "Linux", &[("cb", &[], &[2])]),
        ];
        let (rows, refused) = union(&sources);
        assert!(refused.is_empty());
        assert_eq!(rows["cb"].row.callable, vec![1, 2]);
    }

    /// The signature spellings come whole from the LAST source that has the name:
    /// half of one engine's signature spliced onto another's describes nothing.
    #[test]
    fn the_last_source_supplies_the_signature() {
        let sources = vec![
            source("8.2.33", "Darwin", &[("f", &[], &[])]),
            source("8.5.10", "Linux", &[("f", &[], &[])]),
        ];
        let (rows, _) = union(&sources);
        assert_eq!(rows["f"].row.param_names, vec!["Linux".to_owned()]);
    }
}
