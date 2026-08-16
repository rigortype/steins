//! `fold-probe`: the ADR-0066 differential width probe, as a command (issue #382).
//!
//! # What it is
//!
//! Every row of the folding allowlist claims that a 32-bit engine returns the
//! identical value and type tag a 64-bit one does, or declines. That claim is
//! earned by probing, and until now the probing was done by hand-written tuple
//! lists in an uncommitted scratch directory — which is how the ADR's own
//! evidence tables were produced, and also how the tuple families drifted from
//! the parameter facts they were supposed to cover.
//!
//! This generates the tuples from the **mined parameter facts** (the engine's
//! own arginfo, ADR-0077's 2026-08-16 amendment) rather than a hand-written
//! per-name mapping, runs them through both engines, and prints the per-name
//! disposition in the shape the ADR's amendments tabulate:
//!
//! ```text
//! name             probes (silent/reverse/decline)
//! strpos           23 (0/0/0)
//! ```
//!
//! # Usage
//!
//! ```text
//! cargo xtask fold-probe [--names a,b,c] [--strict] [--json OUT]
//! ```
//!
//! `--names` probes exactly those (a candidate under consideration); with no
//! `--names` it probes **every name on the allowlist**, which is the regression
//! mode: a row whose engines have started to disagree fails the command.
//! `--strict` names the other calling convention — a portability verdict has to
//! hold for whichever mode the request names (#390), so a row deserves both.
//!
//! Needs `php` on `PATH` and php-wasm vendored by `sh apps/playground/build.sh`
//! (a gitignored build product — the exact engine the browser gets).
//!
//! # The four properties that keep a run honest
//!
//! Each of these, dropped, produces a **false clean** — a run reporting an
//! agreement it never measured. Three live in `probe.mjs`, one lives here.
//!
//! 1. **Compare the response BYTES, not parsed JSON.** Array elements cross the
//!    seam with no per-element type tag, so an `int` on one engine and a `float`
//!    on the other differ only as `3000000000` versus `3000000000.0`, which
//!    JavaScript's single number type erases on parse. This is how `range`'s
//!    divergence was found after a parsed comparison called it clean.
//! 2. **A float argument cannot be a JavaScript number.** `3000000000.0`
//!    round-trips through `JSON.stringify` as `3000000000` and reaches the
//!    runner as an int — an argument the range guard refuses, so the tuple is
//!    not a probe at all. Float arguments travel as the raw token `@@…@@`.
//! 3. **Refuse inadmissible tuples.** A tuple carrying an integer outside
//!    ±(2^31 − 1) is one the fold gate would never send, so counting it would
//!    inflate the evidence with cases no fold can reach.
//! 4. **Name the calling convention**, and probe a row both ways.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::corpus::repo_root;

/// A generated probe tuple, in the runner's own wire form.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Tuple {
    name: String,
    args: Vec<serde_json::Value>,
    /// What this tuple is hunting — carried through to the output so a
    /// divergence arrives with the reason it was looked for.
    note: String,
}

/// One probed tuple's verdict, as `probe.mjs` classifies it.
#[derive(serde::Deserialize)]
struct Probed {
    name: String,
    verdict: String,
    #[serde(default)]
    wide: String,
    #[serde(default)]
    narrow: String,
    #[serde(default)]
    args: Vec<serde_json::Value>,
}

/// Entry point for `cargo xtask fold-probe`.
pub fn run(args: &[String]) -> Result<(), String> {
    let strict = args.iter().any(|a| a == "--strict");
    let names = flag_value(args, "--names").map(|v| {
        v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
    });
    let json_out = flag_value(args, "--json").map(PathBuf::from);

    // No `--names` is the regression mode: every row the catalog claims.
    let (names, regression) = match names {
        Some(n) => (n, false),
        None => {
            let mut all: Vec<String> = steins_catalog::portable_names()
                .iter()
                .chain(steins_catalog::refused_names())
                .chain(steins_catalog::unverified_names())
                .map(|n| (*n).to_ascii_lowercase())
                .collect();
            all.sort();
            (all, true)
        }
    };

    let mut tuples = Vec::new();
    let mut unprobeable = Vec::new();
    for name in &names {
        match generate(name) {
            Ok(mut t) => tuples.append(&mut t),
            Err(why) => unprobeable.push(format!("{name}: {why}")),
        }
    }
    if !unprobeable.is_empty() {
        // A name with no mined row, or with a parameter no literal can fill, is
        // not "clean" — it is unmeasured, and saying so is the whole discipline.
        return Err(format!(
            "cannot generate probes for {} name(s):\n  {}",
            unprobeable.len(),
            unprobeable.join("\n  ")
        ));
    }
    println!(
        "fold-probe: {} tuples over {} names, {} convention",
        tuples.len(),
        names.len(),
        if strict { "strict" } else { "weak" }
    );

    let dir = repo_root().join("target/fold-probe");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tuples_path = dir.join("tuples.json");
    let results_path = json_out.unwrap_or_else(|| dir.join("results.json"));
    std::fs::write(
        &tuples_path,
        serde_json::to_string_pretty(&tuples).map_err(|e| format!("encode tuples: {e}"))?,
    )
    .map_err(|e| format!("write {}: {e}", tuples_path.display()))?;

    let script = repo_root().join("docs/research/fold-probe/probe.mjs");
    let mut cmd = Command::new("node");
    cmd.arg(&script).arg(&tuples_path).arg(repo_root()).arg("--json").arg(&results_path);
    if strict {
        cmd.arg("--strict");
    }
    let status = cmd.status().map_err(|e| format!("run node {}: {e}", script.display()))?;
    if !status.success() {
        return Err("the probe harness failed — see its output above".to_owned());
    }

    let text = std::fs::read_to_string(&results_path)
        .map_err(|e| format!("read {}: {e}", results_path.display()))?;
    let probed: Vec<Probed> =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", results_path.display()))?;
    report(&probed, regression)
}

/// The per-name disposition, in the shape ADR-0066's amendments tabulate — and,
/// in regression mode, the verdict.
fn report(probed: &[Probed], regression: bool) -> Result<(), String> {
    let mut by_name: BTreeMap<&str, [usize; 4]> = BTreeMap::new();
    for p in probed {
        let acc = by_name.entry(p.name.as_str()).or_default();
        acc[0] += 1;
        match p.verdict.as_str() {
            "silent" => acc[1] += 1,
            "reverse" => acc[2] += 1,
            "decline" => acc[3] += 1,
            _ => {}
        }
    }
    println!("\n| name | probes (silent/reverse/decline) |");
    println!("| --- | --- |");
    for (name, a) in &by_name {
        println!("| `{name}` | {} ({}/{}/{}) |", a[0], a[1], a[2], a[3]);
    }

    // An engine that died answered nothing, and an unmeasured tuple is not a
    // clean one — the whole point of a productized probe is that it cannot
    // quietly stop being evidence.
    let died = probed.iter().filter(|p| p.verdict == "engine-died").count();
    if died > 0 {
        return Err(format!(
            "{died} tuple(s) killed an engine — the run measured nothing for them. \
             A size-shaped `int` parameter whose name the generator does not know is the \
             usual cause (see `arm_family`'s allocating list)."
        ));
    }

    // A silent divergence is two values that differ; a reverse is the narrow
    // engine answering where the wide one declines. Both are unsound for a
    // PORTABLE row — and for a REFUSED one they are the evidence, so the verdict
    // is per row rather than global.
    let mut broken = Vec::new();
    for p in probed {
        if p.verdict != "silent" && p.verdict != "reverse" {
            continue;
        }
        if steins_catalog::portable(&p.name) {
            broken.push(format!(
                "{}({}) — {}\n      64: {}\n      32: {}",
                p.name,
                p.args.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                p.verdict,
                p.wide,
                p.narrow
            ));
        }
    }
    if !broken.is_empty() {
        return Err(format!(
            "{} divergence(s) on names the catalog calls PORTABLE:\n    {}",
            broken.len(),
            broken.join("\n    ")
        ));
    }
    if regression {
        println!("\nfold-probe: no PORTABLE row diverges.");
    }
    {
        // The other half of the truth, and the reason this is a report rather
        // than a pass/fail: a REFUSED row claims a divergence, and generation is
        // ONE-AT-A-TIME, so a hazard needing two arguments at once is not
        // generated. `version_compare("2147483647", "2147483648")` is exactly
        // that shape — both arguments oversized, neither alone enough — and it
        // shows here as clean while its recorded witness stands. Saying so is
        // the difference between a limitation and a false clean.
        let mut unreproduced: Vec<&str> = by_name
            .iter()
            .filter(|(name, a)| steins_catalog::refusal(name).is_some() && a[1] + a[2] == 0)
            .map(|(name, _)| *name)
            .collect();
        unreproduced.sort_unstable();
        if !unreproduced.is_empty() {
            println!(
                "fold-probe: {} refused row(s) whose divergence the generated families do NOT \
                 reproduce — one-at-a-time generation cannot reach a hazard that needs two \
                 arguments at once, so these keep the hand-written witness ADR-0066 records: {}",
                unreproduced.len(),
                unreproduced.join(", ")
            );
        }
    }
    Ok(())
}

/// `--flag value` lookup.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

// ---------------------------------------------------------------------------
// Tuple generation: parameter shape → probe family.
// ---------------------------------------------------------------------------

/// A float argument spelled as a raw JSON token (property 2 above).
fn raw(t: &str) -> serde_json::Value {
    serde_json::Value::String(format!("@@{t}@@"))
}

/// An array literal in the runner's wire form: `{"__steins_array": [[k, v], …]}`.
fn arr(entries: Vec<(serde_json::Value, serde_json::Value)>) -> serde_json::Value {
    serde_json::json!({
        "__steins_array": entries.into_iter().map(|(k, v)| serde_json::json!([k, v])).collect::<Vec<_>>()
    })
}

/// A list literal (absent keys, so PHP's next-int rule assigns them).
fn list(values: Vec<serde_json::Value>) -> serde_json::Value {
    arr(values.into_iter().map(|v| (serde_json::Value::Null, v)).collect())
}

/// The family a declared parameter type earns, and what that family is hunting.
///
/// This is the mapping ADR-0066's wave 2 wrote by hand and called "the
/// generator's specification". It is the generator now, and it reads the
/// engine's own spelling of the type rather than a per-name table.
///
/// A declared type is a **union**, and the arms are treated one at a time: an
/// arm no literal can fill contributes nothing, and the parameter is only
/// unprobeable when *no* arm can be filled. That distinction is the difference
/// between refusing `iterator_apply` (a `Traversable`, with no literal spelling
/// at all) and probing `count` (`Countable|array` — the array arm is exactly
/// what an all-literal call passes). `?T` adds `null`, which PHP's own optional
/// parameters spell that way.
fn family(spell: &str, param_name: &str) -> Option<(Vec<serde_json::Value>, String)> {
    let lower = spell.to_ascii_lowercase();
    let nullable = lower.starts_with('?');
    let mut values: Vec<serde_json::Value> = Vec::new();
    let mut notes: Vec<&'static str> = Vec::new();
    let mut fillable = false;
    for arm in lower.trim_start_matches('?').split('|') {
        let Some((vs, note)) = arm_family(arm.trim(), param_name) else { continue };
        fillable = true;
        for v in vs {
            if !values.contains(&v) {
                values.push(v);
            }
        }
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    if !fillable {
        return None;
    }
    if (nullable || lower.contains("null")) && !values.contains(&serde_json::Value::Null) {
        values.push(serde_json::Value::Null);
    }
    Some((values, notes.join("; ")))
}

/// One arm of a declared union, or `None` when no literal can fill it — an
/// object, a resource, an enum, `iterable`.
fn arm_family(arm: &str, param_name: &str) -> Option<(Vec<serde_json::Value>, &'static str)> {
    // A size-shaped `int` is a memory bomb from the POSITIVE side only. ADR-0066
    // left `str_pad("abc", "3000000000")` out of the hand-written families for
    // exactly this: a target width cannot be neutralised with an empty subject,
    // so on the 64-bit engine it is a three-gigabyte allocation, a PHP fatal,
    // and a resident runner that dies mid-NDJSON taking the rest of the run
    // with it. The identical coercion path is probed from the negative side,
    // which declines at zero bytes — so that is what is generated here.
    // Kept TIGHT on purpose: only parameters that multiply the OUTPUT size.
    // A first cut included `$num` and `$size`, and it silently disarmed `abs`'s
    // own recorded witness — `abs("3000000000")` — which is how a generator
    // stops measuring what the catalog claims. An engine death is caught
    // separately and fails the run, so this list is an optimisation, not the
    // safety net.
    let allocating = matches!(param_name, "length" | "times" | "count");
    let ints = || {
        vec![
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!(-1),
            serde_json::json!(2_147_483_647i64),
            serde_json::json!(-2_147_483_647i64),
            serde_json::json!("2"),
            raw("2.0"),
            serde_json::json!("3000000000"),
            raw("3000000000.0"),
            serde_json::json!(true),
        ]
    };
    // Two parameters are keyed by NAME rather than type, because `string` is
    // what the engine declares for both and the hazard is entirely in the
    // content: a PCRE `$pattern` carries the inline limit verbs one build JITs
    // past and the other honours — the divergence that refused `preg_split`.
    if arm == "string" && param_name == "pattern" {
        return Some((
            vec![
                serde_json::json!("/a/"),
                serde_json::json!("/z/"),
                serde_json::json!("/[/"),
                serde_json::json!("/(*LIMIT_MATCH=1)a/"),
                serde_json::json!("/(*LIMIT_MATCH=1)(*NO_JIT)a/"),
                serde_json::json!("/(*LIMIT_RECURSION=1)(?:a)+/"),
                serde_json::json!("/\\p{L}/u"),
                serde_json::json!("/(?<=a)b/"),
            ],
            "the inline limit verbs a JIT build ignores and an interpreter honours",
        ));
    }
    Some(match arm {
        // An `int` parameter never coerces a VALUE by the machine word: an
        // oversized argument is a `TypeError` on the narrow engine, which is a
        // decline and therefore sound. A *value* there would not be.
        //
        // In a union with `string`, these same arguments are the `range` route:
        // a numeric string on a parameter that also admits int/float is typed by
        // the MACHINE, not by the argument — the one place the width reaches a
        // result's type tag.
        "int" if allocating => (
            ints()
                .into_iter()
                .filter(|v| {
                    *v != serde_json::json!(2_147_483_647i64)
                        && *v != serde_json::json!("3000000000")
                        && *v != raw("3000000000.0")
                })
                .collect(),
            "an oversized argument from the NEGATIVE side only — the positive one \
             allocates gigabytes and kills the engine (ADR-0066)",
        ),
        "int" => (ints(), "an oversized argument: a decline is sound, a value is not"),
        // Rendering and rounding edges, and the `TypeError` a string earns on
        // such a parameter since PHP 8.
        "float" => (
            vec![
                raw("1.5"),
                raw("-1.5"),
                raw("2.5"),
                raw("-0.0"),
                raw("0.0"),
                raw("1.0e+15"),
                raw("1.0e+20"),
                raw("5.0e-324"),
                raw("0.285"),
                raw("1.005"),
                serde_json::json!("1.5"),
            ],
            "rounding and rendering edges, and a numeric string's TypeError",
        ),
        "string" => (
            vec![
                // The base value is FIRST, and one-at-a-time holds every other
                // position at its base — so a degenerate subject here would
                // disarm every other parameter's family. `"aaa"` matches,
                // repeats and splits.
                serde_json::json!("aaa"),
                serde_json::json!(""),
                serde_json::json!("abc"),
                serde_json::json!("abcabc"),
                // Multibyte: byte offsets, not character ones.
                serde_json::json!("ábc"),
                // A numeric string stays a string — unless something retypes it.
                serde_json::json!("3000000000"),
                serde_json::json!("0"),
            ],
            "byte work, multibyte subjects, and numeric strings that must stay strings",
        ),
        "bool" => (vec![serde_json::json!(true), serde_json::json!(false)], "both arms"),
        "array" => (
            vec![
                list(vec![]),
                list(vec![
                    serde_json::json!(1),
                    serde_json::json!(0),
                    serde_json::json!(2),
                    serde_json::json!(""),
                    serde_json::json!("0"),
                    serde_json::Value::Null,
                ]),
                arr(vec![
                    (serde_json::json!("a"), serde_json::json!(1)),
                    (serde_json::json!("b"), serde_json::json!(0)),
                ]),
                // An explicit key at the NARROW `PHP_INT_MAX`: the next-int rule
                // has nowhere to go on one engine and somewhere on the other.
                arr(vec![
                    (serde_json::json!(2_147_483_647i64), serde_json::json!(1)),
                    (serde_json::Value::Null, serde_json::json!(0)),
                ]),
                list(vec![list(vec![]), list(vec![serde_json::json!(1)])]),
                // Mixed keys: a merge RENUMBERS integer keys and keeps string
                // ones, so an array carrying both is where that rule is
                // visible — and a negative integer key is where PHP 8.3 changed
                // what "next" means.
                arr(vec![
                    (serde_json::json!(-3), serde_json::json!("x")),
                    (serde_json::json!("k"), serde_json::json!("y")),
                    (serde_json::Value::Null, serde_json::json!(1)),
                ]),
                // The same string key twice across two arrays is last-wins, and
                // which one wins is the engine's rule, not ours.
                arr(vec![
                    (serde_json::json!("k"), serde_json::json!("second")),
                    (serde_json::json!(0), serde_json::json!("also zero")),
                ]),
            ],
            "PHP's own falsiness, key preservation, integer renumbering and last-wins string \
             keys, and the next-int rule at the narrow max",
        ),
        // A callable position is one the seam refuses to fill (issue #382's
        // shape gate), so the only probe that exists for it is the one the gate
        // admits.
        a if a.contains("callable") || a.contains("closure") => (
            vec![serde_json::Value::Null],
            "the only callback argument a fold may carry: a literal null",
        ),
        // `mixed` is every literal at once, which is the honest reading of an
        // undeclared parameter.
        "mixed" => (
            vec![
                serde_json::json!(1),
                serde_json::json!("x"),
                raw("1.5"),
                serde_json::json!(true),
                serde_json::Value::Null,
                list(vec![serde_json::json!(1)]),
            ],
            "an undeclared parameter is every literal at once",
        ),
        "null" => (vec![serde_json::Value::Null], "the null arm"),
        // An object, resource, enum or `iterable` arm cannot be filled by a
        // literal. Not an error by itself — another arm may still be probeable.
        _ => return None,
    })
}

/// Every tuple for one name: a base call with its required arguments, then each
/// position varied across its whole family with the others held at their base.
///
/// A by-ref position is filled like any other: the seam passes by value, so the
/// write is lost either way, and what is being measured here is the width of the
/// RESULT. ADR-0077's row is what makes the lost write sound.
///
/// **One-at-a-time, not a cartesian product.** The product over even four
/// parameters is thousands of engine round trips, and the hazards this hunts are
/// per-parameter (a width-typed numeric string, an oversized `int`). A
/// cross-parameter interaction is therefore NOT probed by this, and a row whose
/// hazard needs two arguments at once still needs a hand-written tuple — which
/// `--names` exists to run.
fn generate(name: &str) -> Result<Vec<Tuple>, String> {
    let facts = steins_catalog::param_facts(name)
        .ok_or("no mined parameter facts — run `cargo xtask mine-param-facts`")?;
    let mut families = Vec::with_capacity(facts.params.len());
    for (i, spell) in facts.params.iter().enumerate() {
        let pname = facts.param_names.get(i).copied().unwrap_or("");
        let (values, note) = family(spell, pname).ok_or_else(|| {
            format!("parameter {i} is `{spell}`, and no arm of it can be filled by a literal")
        })?;
        families.push((values, note));
    }
    let base: Vec<serde_json::Value> =
        families.iter().map(|(v, _)| v[0].clone()).collect();

    let mut out = Vec::new();
    // The required-arity call, which is the one most source writes.
    out.push(Tuple {
        name: name.to_owned(),
        args: base[..facts.params_required.min(base.len())].to_vec(),
        note: "the required-arity call".to_owned(),
    });
    // A VARIADIC position takes as many arguments as the call cares to pass, and
    // one of them is not a probe of the name: `array_merge`'s whole job is what
    // happens BETWEEN its arrays — integer keys renumbered, string keys
    // last-wins. So a variadic tail is called at three arities with values drawn
    // from its own family.
    for &v in facts.variadic {
        let Some((values, note)) = families.get(v) else { continue };
        for arity in 2..=3usize {
            for chunk in values.chunks(arity) {
                if chunk.len() < arity {
                    continue;
                }
                let mut args = base[..v].to_vec();
                args.extend(chunk.iter().cloned());
                out.push(Tuple {
                    name: name.to_owned(),
                    args,
                    note: format!("{note} — {arity} arguments in the variadic tail"),
                });
            }
        }
    }
    for (i, (values, note)) in families.iter().enumerate() {
        // Every REQUIRED position stays filled. Truncating at the varied one
        // instead made an under-arity call whenever `i` sat before the last
        // required parameter — an `ArgumentCountError` on both engines, which
        // agrees trivially and measures nothing. It hid the PCRE witnesses:
        // varying `preg_match`'s `$pattern` dropped its `$subject`.
        let end = (i + 1).max(facts.params_required.min(base.len()));
        let mut args = base[..end].to_vec();
        for v in values {
            args[i] = v.clone();
            out.push(Tuple { name: name.to_owned(), args: args.clone(), note: note.clone() });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generator reads the engine's own spelling, which is the point: the
    /// per-name mapping it replaces is what drifted from the facts.
    /// A variation never drops a REQUIRED argument: an under-arity call is an
    /// `ArgumentCountError` on both engines, which agrees trivially and
    /// measures nothing.
    #[test]
    fn every_generated_call_keeps_its_required_arguments() {
        for name in ["preg_match", "strpos", "str_replace"] {
            let facts = steins_catalog::param_facts(name).expect("mined");
            for t in generate(name).expect("generated") {
                assert!(
                    t.args.len() >= facts.params_required,
                    "{name} generated a {}-argument call, and {} are required: {:?}",
                    t.args.len(),
                    facts.params_required,
                    t.args
                );
            }
        }
    }

    #[test]
    fn the_families_follow_the_declared_types() {
        let t = generate("strpos").expect("strpos is mined");
        assert_eq!(t[0].args.len(), 2, "the required-arity call takes haystack and needle");
        // `$offset` is `int`, so the oversized numeric string is in there — the
        // argument whose answer must be a decline and not a value.
        assert!(
            t.iter().any(|x| x.args.len() == 3 && x.args[2] == serde_json::json!("3000000000")),
            "the int family reaches $offset"
        );
        // …and the float family does not, because no parameter is a float.
        assert!(!t.iter().any(|x| x.args.iter().any(|a| a == &raw("0.285"))));
    }

    /// A size-shaped `int` loses its POSITIVE oversized probes and keeps the
    /// negative twin: the same coercion path, at zero bytes rather than three
    /// gigabytes. This is ADR-0066's "deliberately absent" probe, generated.
    #[test]
    fn an_allocating_int_parameter_is_probed_from_the_negative_side() {
        let t = generate("str_pad").expect("str_pad is mined");
        assert!(
            !t.iter().any(|x| x.args.get(1) == Some(&serde_json::json!(2_147_483_647i64))),
            "a 2GB allocation is not a probe, it is a dead runner"
        );
        assert!(
            t.iter().any(|x| x.args.get(1) == Some(&serde_json::json!(-2_147_483_647i64))),
            "the negative twin exercises the same coercion"
        );
        // An OFFSET-shaped int keeps both sides: nothing is allocated by it.
        let t = generate("strpos").expect("strpos is mined");
        assert!(t.iter().any(|x| x.args.get(2) == Some(&serde_json::json!(2_147_483_647i64))));
    }

    /// A callable position is probed with the only argument the seam's shape
    /// gate admits, so a generated run cannot execute a callback.
    #[test]
    fn a_callable_position_is_only_ever_null() {
        let t = generate("array_filter").expect("array_filter is mined");
        for tuple in &t {
            if let Some(cb) = tuple.args.get(1) {
                assert_eq!(*cb, serde_json::Value::Null, "a generated callback argument is null");
            }
        }
    }

    /// A parameter no literal can fill is a REFUSAL to generate, not an empty
    /// run that reads as clean — but only when NO arm of it can be filled.
    /// `count(Countable|array)` is probed through its array arm.
    #[test]
    fn an_unfillable_parameter_refuses_to_generate() {
        // `iterator_apply` takes a `Traversable`: there is no literal for it.
        let err = generate("iterator_apply").expect_err("an object parameter cannot be probed");
        assert!(err.contains("no arm of it can be filled"), "{err}");
        let err = generate("no_such_builtin_anywhere").expect_err("an unmined name");
        assert!(err.contains("mine-param-facts"), "{err}");
        // …and a union with one fillable arm is probed through it.
        let t = generate("count").expect("Countable|array is probeable through `array`");
        assert!(t.iter().any(|x| x.args.first().is_some_and(|a| a.get("__steins_array").is_some())));
    }
}
