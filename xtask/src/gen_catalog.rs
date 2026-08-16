//! `gen-catalog`: regenerate the builtin class-hierarchy table from the pinned
//! php-src mining data (ADR-0043 §3).
//!
//! # Source of record
//!
//! `docs/research/phpsrc-mining/hierarchy.toml`: 368 class/interface/enum
//! declarations mined from php-src `6bc7c26cf67a9480b5ef9d6191aebe87fa931183`,
//! cross-checked against PHP 8.5.8. Records **direct** `extends`/`implements`
//! edges; the is-a oracle walks `builtin_class_supers` for the transitive
//! closure ([crosscheck](docs/research/phpsrc-mining/crosscheck.txt) confirmed
//! it equals runtime `class_implements` for a sample).
//!
//! Parsed here via the xtask-only `toml` crate (shipped `steins-catalog` stays
//! dependency-free) into committed `hierarchy_generated.rs`: a sorted
//! `&[(&str, &[&str])]` table for binary search.
//!
//! # What is emitted, and what is not
//!
//! * `kind = 'class'`/`'interface'`: emitted, direct supers, lowercased key,
//!   declared casing kept, namespaces preserved.
//! * `kind = 'enum'`: SKIPPED — mining didn't capture implicit
//!   `UnitEnum`/`BackedEnum` interfaces or backing, so the (empty) recorded
//!   super-set is incomplete and would make the oracle return a spurious `No`
//!   against those interfaces. Absence → `None` → `Unknown` is the FP-safe
//!   verdict ADR-0043 §3 requires; re-mining backing data would allow `Some`.
//!
//! Two further tables ride the pipeline: curated return-fact refinements
//! ([`gen_return_facts`], `return_facts.toml`) and the ADR-0069 declared-return
//! floor ([`gen_declared_returns`], `phpstan-mining/declared_returns.toml`,
//! sourced by `cargo xtask mine-function-map`).
//!
//! A fourth, byproduct table: builtin-class **display names**
//! (`display_names_generated.rs`), lowercased key → php-src's declared casing
//! — needed because `ContractTy::Class` case-folds (`class_eq`), so the dump
//! surface would otherwise render `gmp` where PHPStan renders `GMP`
//! (ADR-0069 third-amendment residual). Unlike HIERARCHY it keeps enum rows:
//! display has no soundness gate to guard.
//!
//! Run `cargo xtask gen-catalog` after editing any of those TOMLs; a test
//! asserts the committed files stay sorted and self-consistent.

use std::collections::BTreeMap;

use crate::corpus::repo_root;

/// Entry point for `cargo xtask gen-catalog`.
pub fn run() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/hierarchy.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: Doc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    // Lowercase-keyed BTreeMap → deterministic binary-search table. Enums are
    // skipped (see module docs); classes/interfaces are kept.
    let mut table: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Keeps EVERY row, enums included (see module docs).
    let mut display: BTreeMap<String, String> = BTreeMap::new();
    let mut skipped_enums = 0usize;
    for c in &doc.class {
        let key = c.name.to_ascii_lowercase();
        if let Some(prev) = display.insert(key.clone(), c.name.clone())
            && prev != c.name
        {
            return Err(format!("conflicting declared casing for `{key}`: `{prev}` vs `{}`", c.name));
        }
        if c.kind == "enum" {
            skipped_enums += 1;
            continue;
        }
        let mut supers = c.extends.clone();
        supers.extend(c.implements.iter().cloned());
        if let Some(prev) = table.insert(key.clone(), supers.clone())
            && prev != supers
        {
            return Err(format!("conflicting duplicate declaration for `{key}`"));
        }
    }

    let out = render(&table);
    let dst = repo_root().join("crates/steins-catalog/src/hierarchy_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;

    println!(
        "gen-catalog: {} classes/interfaces emitted, {} enums skipped → {}",
        table.len(),
        skipped_enums,
        dst.display()
    );

    let out = render_display_names(&display);
    let dst = repo_root().join("crates/steins-catalog/src/display_names_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("gen-catalog: {} display-name rows emitted → {}", display.len(), dst.display());

    gen_return_facts()?;
    gen_resource_returns()?;
    gen_declared_returns()?;
    gen_param_facts()?;
    Ok(())
}

/// Regenerate the **resource-return** table (ADR-0056 §8) from
/// `resource_returns.toml` into `resource_returns_generated.rs`. Only two
/// fields survive transcription — name and whether the stub's `@return`
/// carries a `false` arm; the rest (stub path, probe transcript, confidence
/// grade) is evidence that belongs in the source of record, not here.
fn gen_resource_returns() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/resource_returns.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: ResourceDoc =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut table: BTreeMap<String, bool> = BTreeMap::new();
    for f in &doc.function {
        let key = f.name.to_ascii_lowercase();
        // The stub's `@return` vocabulary is only `resource` or `resource|false`;
        // anything else is a mis-transcribed row and must fail the build.
        let may_be_false = match f.arms.as_str() {
            "resource" => false,
            "resource|false" => true,
            other => {
                return Err(format!("resource-return row `{key}`: unexpected arms `{other}`"));
            }
        };
        if table.insert(key.clone(), may_be_false).is_some() {
            return Err(format!("duplicate resource-return row for `{key}`"));
        }
    }

    let out = render_resource_returns(&table);
    let dst = repo_root().join("crates/steins-catalog/src/resource_returns_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("gen-catalog: {} resource-return rows emitted → {}", table.len(), dst.display());
    Ok(())
}

/// The `[[function]]` shape of `resource_returns.toml`; evidence keys
/// (`stub`, `probe`, `confidence`) are documentation and ignored.
#[derive(serde::Deserialize)]
struct ResourceDoc {
    #[serde(default)]
    function: Vec<ResourceRow>,
}

#[derive(serde::Deserialize)]
struct ResourceRow {
    name: String,
    arms: String,
}

/// Render the committed resource-return table. Deterministic (BTreeMap order).
fn render_resource_returns(table: &BTreeMap<String, bool>) -> String {
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpsrc-mining/resource_returns.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // Builtins whose return is a legacy PHP RESOURCE (ADR-0056 §8): the one\n\
         // type PHP has no syntax to declare, so the reflected envelope every other\n\
         // return fact is anchored to can never carry it. A row is admitted at a\n\
         // call site only when all three §7 conditions hold — this table (the stub\n\
         // reading at the pin), the analyzing engine declaring NO return type for\n\
         // the name (the resource-to-object migration tripwire), and the project\n\
         // PHP minor equalling PINNED_PHP.\n\
         //\n\
         // Each row: (lowercased builtin name, whether the stub's `@return` carries\n\
         // a `false` arm). Sorted by key for binary search. Source of record is the\n\
         // TOML, which carries the per-row stub path and probe transcript.\n\n",
    );
    s.push_str("pub(crate) static RESOURCE_RETURNS: &[(&str, bool)] = &[\n");
    for (key, may_be_false) in table {
        s.push_str(&format!("    ({key:?}, {may_be_false}),\n"));
    }
    s.push_str("];\n");
    s
}

/// Regenerate the builtin **declared-return floor** (ADR-0069, issues #73/#79)
/// from `phpstan-mining/declared_returns.toml` into
/// `declared_returns_generated.rs`. Two tables come from one source of
/// record: the declared rows, and the A11-shaped change oracle
/// (`[version_sensitive]`) the target gate reads. The TOML is produced by
/// `cargo xtask mine-function-map` (mining, lowerability filter, engine
/// cross-check); this function only transcribes it, so mining (needs
/// phpstan-src + live `php`) and generation (needs neither) run
/// independently.
fn gen_declared_returns() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpstan-mining/declared_returns.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: EnvelopeDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut rows: BTreeMap<String, String> = BTreeMap::new();
    for (name, ty) in &doc.declared {
        rows.insert(name.to_ascii_lowercase(), ty.clone());
    }
    let mut sensitive: BTreeMap<String, (u16, u16)> = BTreeMap::new();
    for (name, minor) in &doc.version_sensitive {
        let parsed = parse_minor(minor)
            .ok_or_else(|| format!("unparseable version_sensitive minor `{minor}` for `{name}`"))?;
        sensitive.insert(name.to_ascii_lowercase(), parsed);
    }

    let out = render_declared_returns(&doc.meta, &doc.counts, &rows, &sensitive);
    let dst = repo_root().join("crates/steins-catalog/src/declared_returns_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!(
        "gen-catalog: {} declared-return rows + {} version-sensitive names emitted → {}",
        rows.len(),
        sensitive.len(),
        dst.display()
    );
    Ok(())
}

/// The per-parameter facts table (issue #382): `param_facts.toml`, mined off the
/// engine's own arginfo by `cargo xtask mine-param-facts`, into
/// `param_facts_generated.rs`.
///
/// Two shipped shapes come out of it. `ROWS` carries the positions for every
/// name that has a hazard or sits on the folding allowlist; `PLAIN` carries the
/// names mined and found to carry nothing. The second is not padding: the
/// catalog's completeness tests have to tell "mined, and empty" from "never
/// looked at", and a table with only the interesting rows cannot.
fn gen_param_facts() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/param_facts.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: ParamDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut plain: Vec<String> = doc.plain.names.iter().map(|n| n.to_ascii_lowercase()).collect();
    plain.sort();
    plain.dedup();
    let mut rows: BTreeMap<String, ParamRow> = BTreeMap::new();
    for (name, row) in &doc.r#fn {
        rows.insert(name.to_ascii_lowercase(), row.clone());
    }
    for name in &plain {
        if rows.contains_key(name) {
            return Err(format!("`{name}` is both a row and a plain name in param_facts.toml"));
        }
    }

    let out = render_param_facts(&doc.meta, &doc.counts, &rows, &plain);
    let dst = repo_root().join("crates/steins-catalog/src/param_facts_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!(
        "gen-catalog: {} parameter-fact rows + {} plain names emitted → {}",
        rows.len(),
        plain.len(),
        dst.display()
    );
    Ok(())
}

/// The shape of `param_facts.toml`.
#[derive(serde::Deserialize)]
struct ParamDoc {
    meta: ParamMeta,
    counts: ParamCounts,
    #[serde(default)]
    r#fn: BTreeMap<String, ParamRow>,
    plain: PlainNames,
}

#[derive(serde::Deserialize)]
struct ParamMeta {
    php: String,
    extensions: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ParamCounts {
    internal_functions: usize,
    rows: usize,
    hazardous: usize,
    plain: usize,
}

#[derive(serde::Deserialize)]
struct PlainNames {
    names: Vec<String>,
}

#[derive(Clone, serde::Deserialize)]
struct ParamRow {
    by_ref: Vec<usize>,
    callable: Vec<usize>,
    variadic: Vec<usize>,
    optional: Vec<usize>,
    params: Vec<String>,
    param_names: Vec<String>,
    params_required: usize,
}

/// Render `param_facts_generated.rs`.
fn render_param_facts(
    meta: &ParamMeta,
    counts: &ParamCounts,
    rows: &BTreeMap<String, ParamRow>,
    plain: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpsrc-mining/param_facts.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // Builtin PER-PARAMETER FACTS (issue #382), read off the engine's own arginfo\n\
         // through `ReflectionFunction` — the INDEPENDENT source `out_params` (ADR-0077)\n\
         // and `invocation_shape` (ADR-0033) are checked against. Both of those were\n\
         // transcribed from php-src's stubs by hand; a second transcription of the same\n\
         // stubs would agree with them wherever they are wrong.\n\
         //\n",
    );
    let _ = writeln!(s, "// Mined from PHP {} with these extensions loaded:", meta.php);
    let mut line = String::from("//   ");
    for e in &meta.extensions {
        if line.len() + e.len() + 2 > 96 {
            let _ = writeln!(s, "{}", line.trim_end());
            line = String::from("//   ");
        }
        line.push_str(e);
        line.push_str(", ");
    }
    if line.trim() != "//" {
        let _ = writeln!(s, "{}", line.trim_end().trim_end_matches(','));
    }
    s.push_str("//\n// Counts at the mining pin:\n");
    let _ = writeln!(s, "//   {:>5}  internal functions the build had", counts.internal_functions);
    let _ = writeln!(s, "//   {:>5}  rows kept (a hazard, or a name the catalog reasons about)", counts.rows);
    let _ = writeln!(s, "//   {:>5}    of those, carrying by-ref / declared-callable / variadic", counts.hazardous);
    let _ = writeln!(s, "//   {:>5}  names mined and recorded as carrying none of the three", counts.plain);
    s.push_str(
        "\n/// One internal function's parameter facts, as the engine's arginfo reports\n\
         /// them. Positions are 0-based and ascending.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct ParamFacts {\n\
         \x20   /// Positions declared `&$x` — what the fold seam cannot write back\n\
         \x20   /// through, and what `out_params` must have a row for (ADR-0077).\n\
         \x20   pub by_ref: &'static [usize],\n\
         \x20   /// Positions whose DECLARED type admits a callable. Sound, not complete:\n\
         \x20   /// a comparator at a `mixed` variadic tail (`array_udiff`) or callables\n\
         \x20   /// inside an array (`preg_replace_callback_array`) are not declared.\n\
         \x20   pub callable: &'static [usize],\n\
         \x20   /// Variadic positions — where the families this table cannot see by type\n\
         \x20   /// put their callback.\n\
         \x20   pub variadic: &'static [usize],\n\
         \x20   /// Optional positions, in the engine's own reckoning.\n\
         \x20   pub optional: &'static [usize],\n\
         \x20   /// Each position's declared type as the engine spells it; `mixed` when\n\
         \x20   /// the parameter has no declared type at all.\n\
         \x20   pub params: &'static [&'static str],\n\
         \x20   /// Each position's declared NAME. Only the name tells a size-shaped\n\
         \x20   /// `int` ($length, $times) from an offset — and an oversized probe on\n\
         \x20   /// the first is a multi-gigabyte allocation, a PHP fatal, and a dead\n\
         \x20   /// runner (ADR-0066's deliberately-absent probe).\n\
         \x20   pub param_names: &'static [&'static str],\n\
         \x20   /// `getNumberOfRequiredParameters()`.\n\
         \x20   pub params_required: usize,\n\
         }\n\n",
    );
    let _ = writeln!(s, "/// Sorted by name for binary search; keys are lowercase.");
    let _ = writeln!(s, "pub(crate) static PARAM_FACTS: &[(&str, ParamFacts)] = &[");
    for (name, r) in rows {
        let _ = writeln!(s, "    (");
        let _ = writeln!(s, "        {name:?},");
        let _ = writeln!(s, "        ParamFacts {{");
        let _ = writeln!(s, "            by_ref: &{:?},", r.by_ref);
        let _ = writeln!(s, "            callable: &{:?},", r.callable);
        let _ = writeln!(s, "            variadic: &{:?},", r.variadic);
        let _ = writeln!(s, "            optional: &{:?},", r.optional);
        let _ = writeln!(s, "            params: &{:?},", r.params);
        let _ = writeln!(s, "            param_names: &{:?},", r.param_names);
        let _ = writeln!(s, "            params_required: {},", r.params_required);
        let _ = writeln!(s, "        }},");
        let _ = writeln!(s, "    ),");
    }
    let _ = writeln!(s, "];\n");
    s.push_str(
        "/// Names mined and found to carry no by-ref, callable or variadic position.\n\
         /// Sorted; lowercase. Membership is the FACT — the completeness tests read it\n\
         /// to tell an empty row from a name nobody looked at.\n",
    );
    let _ = writeln!(s, "pub(crate) static PARAM_FACTS_PLAIN: &[&str] = &[");
    for n in plain {
        let _ = writeln!(s, "    {n:?},");
    }
    let _ = writeln!(s, "];");
    s
}

/// Parse a `"8.5"` minor spelling to `(major, minor)`.
fn parse_minor(s: &str) -> Option<(u16, u16)> {
    let (major, minor) = s.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The shape of `declared_returns.toml`. Exclusion sections document refusals
/// and are deliberately not read — nothing is generated from them.
#[derive(serde::Deserialize)]
struct EnvelopeDoc {
    meta: EnvelopeMeta,
    counts: EnvelopeCounts,
    #[serde(default)]
    declared: BTreeMap<String, String>,
    #[serde(default)]
    version_sensitive: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct EnvelopeMeta {
    phpstan_src_commit: String,
    crosscheck_php: String,
}

#[derive(serde::Deserialize)]
struct EnvelopeCounts {
    total_keys: usize,
    methods_skipped: usize,
    alternates_disagree: usize,
    not_lowerable: usize,
    not_lowerable_shaped_arrays: usize,
    not_lowerable_object_or_resource: usize,
    reflection_disagree: usize,
    reflection_missing: usize,
    admitted: usize,
    admitted_rich: usize,
}

/// Render the committed declared-return tables, provenance header and all.
fn render_declared_returns(
    meta: &EnvelopeMeta,
    counts: &EnvelopeCounts,
    rows: &BTreeMap<String, String>,
    sensitive: &BTreeMap<String, (u16, u16)>,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpstan-mining/declared_returns.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // Builtin DECLARED RETURN TYPES: the ADR-0069 Asserted floor\n\
         // (issues #73/#79, widened for the array vocabulary by ADR-0071).\n\
         // Without a live engine every rung of the return ladder is engine-gated and a\n\
         // builtin call with variable operands types as `unknown`; these rows raise that\n\
         // floor with the type the builtin DECLARES.\n\
         //\n\
         // LINEAGE — see the root NOTICE file for both MIT permission notices:\n\
         //   Steins <- phpstan-src `resources/functionMap.php`\n\
         //              (MIT, Copyright (c) Ondrej Mirtes and contributors)\n\
         //          <- Phan `src/Phan/Language/Internal/FunctionSignatureMap.php`\n\
         //              (MIT, Copyright (c) 2015 Rasmus Lerdorf,\n\
         //                   Copyright (c) 2015 Andrew Morrison)\n\
         //\n",
    );
    let _ = writeln!(s, "// phpstan-src pin: {}", meta.phpstan_src_commit);
    let _ = writeln!(s, "// cross-checked against PHP {} via the real sidecar.", meta.crosscheck_php);
    s.push_str("//\n// Mining counts at the pin:\n");
    let _ = writeln!(s, "//   {:>5}  functionMap entries (after the delta ladder)", counts.total_keys);
    let _ = writeln!(s, "//   {:>5}  `Class::method` rows skipped (methods stay out of this slice)", counts.methods_skipped);
    let _ = writeln!(s, "//   {:>5}  names whose alternate signatures disagree on the return type", counts.alternates_disagree);
    let _ = writeln!(s, "//   {:>5}  rows the declared-contract arm lane cannot carry, of which", counts.not_lowerable);
    let _ = writeln!(s, "//   {:>5}    shaped arrays / lists", counts.not_lowerable_shaped_arrays);
    let _ = writeln!(s, "//   {:>5}    objects / class names / callable / resource", counts.not_lowerable_object_or_resource);
    let _ = writeln!(s, "//   {:>5}  rows the arm-wise engine countersign refuses", counts.reflection_disagree);
    let _ = writeln!(s, "//   {:>5}  names the pinned engine does not know as functions", counts.reflection_missing);
    let _ = writeln!(s, "//   {:>5}  ADMITTED (the table below), of which", counts.admitted);
    let _ = writeln!(s, "//   {:>5}    RICHER than a single-base envelope (the #79 and ADR-0071 reach)", counts.admitted_rich);
    s.push_str(
        "//\n\
         // The skipped methods and the object bucket are what remains deferred\n\
         // (ADR-0069 §5 as amended 2026-08-01, ADR-0071 §2.3). Object, class-name,\n\
         // `callable` and `resource` arms have no extensional denotation — the\n\
         // acceptance relation falls to a reflexive is-a floor and steins-contract\n\
         // carries no hierarchy — so the countersign could only answer `Maybe`, and a\n\
         // row entering uncountersigned is what ADR-0069 §3 refuses. The shaped-array\n\
         // bucket is empty since ADR-0071 gave the relation a structural denotation\n\
         // for `array` / `list<T>` / `array<K, V>` / `array{…}`.\n\
         //\n\
         // GRADE: every row seeds at the `Asserted` stratum, never `Verified`\n\
         // (ADR-0069 §2). It reaches the dump surface and contracts-tier reasoning;\n\
         // the proof layer's all-Verified premise rule keeps it out of every finding.\n\
         //\n\
         // WHEN IT SPEAKS: per NAME, not per run. The consuming rung fires exactly\n\
         // where the folder's reflected envelope yielded None for the asked name —\n\
         // `--no-php` is only the total case. With a live engine the floor still\n\
         // speaks where that engine is SILENT: an extension the analyzing PHP does\n\
         // not load, or a builtin with no declared return type. Where the engine\n\
         // answers, the floor never overrides it. The absence family never reads\n\
         // these rows at all: existence is a boot-surface fact, and an absence\n\
         // finding standing beside a floor fact is complementary, not contradictory.\n\
         //\n\
         // Each row: (lowercased builtin name, canonical phpdoc spelling — a base, a\n\
         // `T|false` failure union, a refinement, or an array type). The consumer\n\
         // re-lowers the string\n\
         // through the SAME `lower_str` → `flatten_arms` seam a PROJECT function's\n\
         // declared return takes (issue #60) and seeds the arms Asserted. Sorted by key\n\
         // for binary search.\n\n",
    );
    s.push_str("pub(crate) static DECLARED_RETURNS: &[(&str, &str)] = &[\n");
    for (key, ty) in rows {
        let _ = writeln!(s, "    ({key:?}, {ty:?}),");
    }
    s.push_str("];\n\n");
    s.push_str(
        "// The A11-shaped change oracle: names whose declared RETURN type moves between\n\
         // two adjacent supported minors, keyed to the minor it moved AT. A project whose\n\
         // declared PhpTarget is not wholly at or above that minor declines the row; an\n\
         // unknown target admits (the row is Asserted anyway, ADR-0069 §3).\n\
         //\n\
         // Listed independently of the table above: a name can be version-sensitive\n\
         // without carrying an admitted row, and the gate must stay complete either way.\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static RETURN_VERSION_SENSITIVE: &[(&str, (u16, u16))] = &[\n");
    for (key, (major, minor)) in sensitive {
        let _ = writeln!(s, "    ({key:?}, ({major}, {minor})),");
    }
    s.push_str("];\n");
    s
}

/// Regenerate the builtin return-fact refinement table (ADR-0056) from
/// `return_facts.toml` into `return_facts_generated.rs`. Each row is a curated
/// phpdoc-type refinement keyed by lowercased builtin name; may be empty (R1
/// lands zero rows — the reflected envelope alone serves the bool family).
fn gen_return_facts() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/return_facts.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: ReturnDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut table: BTreeMap<String, String> = BTreeMap::new();
    for f in &doc.function {
        let key = f.name.to_ascii_lowercase();
        if table.insert(key.clone(), f.refinement.clone()).is_some() {
            return Err(format!("duplicate return-fact row for `{key}`"));
        }
    }

    let out = render_return_facts(&table);
    let dst = repo_root().join("crates/steins-catalog/src/return_facts_generated.rs");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("gen-catalog: {} return-fact rows emitted → {}", table.len(), dst.display());
    Ok(())
}

/// The `[[function]]` shape of `return_facts.toml`; other keys (evidence/probe
/// notes) are documentation and ignored here.
#[derive(serde::Deserialize)]
struct ReturnDoc {
    #[serde(default)]
    function: Vec<ReturnRow>,
}

#[derive(serde::Deserialize)]
struct ReturnRow {
    name: String,
    refinement: String,
}

/// Render the committed return-fact table. Deterministic (BTreeMap order).
fn render_return_facts(table: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpsrc-mining/return_facts.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // Builtin return-fact REFINEMENTS (ADR-0056): each row is a curated phpdoc\n\
         // type string that narrows strictly WITHIN a builtin's reflected return\n\
         // envelope. The reflected envelope itself is seeded without a row; a row is\n\
         // consumed only after the acceptance machinery confirms it is an extensional\n\
         // subset of the envelope AND the project PHP minor equals PINNED_PHP\n\
         // (ADR-0056 §2). The table may be empty — R1 lands zero rows (the bool\n\
         // family's envelope is already `bool`). Source of record is the TOML.\n\
         //\n\
         // Each row: (lowercased builtin name, curated refinement phpdoc string).\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static RETURN_FACTS: &[(&str, &str)] = &[\n");
    for (key, refinement) in table {
        s.push_str(&format!("    ({key:?}, {refinement:?}),\n"));
    }
    s.push_str("];\n");
    s
}

/// Render the committed display-name table. Deterministic (BTreeMap order).
fn render_display_names(table: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpsrc-mining/hierarchy.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // php-src pin: 6bc7c26cf67a9480b5ef9d6191aebe87fa931183 (Thu Jul 9 2026),\n\
         // cross-checked against PHP 8.5.8. Source of record is the TOML; run\n\
         // `cargo xtask gen-catalog` to regenerate after editing it.\n\
         //\n\
         // Each row: (lowercased class/interface/enum name, the casing php-src\n\
         // DECLARES it with). Display fidelity only — every judgment compares\n\
         // through the case-insensitive `class_eq`, so nothing may decide on this\n\
         // table. Unlike HIERARCHY it keeps the enum rows: the enum exclusion\n\
         // there guards the is-a oracle against an incomplete super-edge set, and\n\
         // a display name has no such soundness gate.\n\
         //\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static DISPLAY_NAMES: &[(&str, &str)] = &[\n");
    for (key, name) in table {
        s.push_str(&format!("    ({key:?}, {name:?}),\n"));
    }
    s.push_str("];\n");
    s
}

/// The `[[class]]` array-of-tables shape of `hierarchy.toml`.
#[derive(serde::Deserialize)]
struct Doc {
    class: Vec<Class>,
}

#[derive(serde::Deserialize)]
struct Class {
    name: String,
    kind: String,
    #[serde(default)]
    extends: Vec<String>,
    #[serde(default)]
    implements: Vec<String>,
}

/// Render the committed Rust table. Deterministic (BTreeMap iteration order).
fn render(table: &BTreeMap<String, Vec<String>>) -> String {
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpsrc-mining/hierarchy.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // php-src pin: 6bc7c26cf67a9480b5ef9d6191aebe87fa931183 (Thu Jul 9 2026),\n\
         // cross-checked against PHP 8.5.8. Source of record is the TOML; run\n\
         // `cargo xtask gen-catalog` to regenerate after editing it.\n\
         //\n\
         // Each row: (lowercased class/interface name, its DIRECT supertypes with\n\
         // declared casing preserved — `extends` then `implements`). The is-a oracle\n\
         // (ADR-0043) walks these transitively; a name absent here is an unknown\n\
         // external (→ oracle `Unknown`, never `No`). Builtin enums are deliberately\n\
         // omitted (incomplete implicit-interface/backing data — see gen_catalog.rs).\n\
         //\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static HIERARCHY: &[(&str, &[&str])] = &[\n");
    for (key, supers) in table {
        let supers_lit = if supers.is_empty() {
            "&[]".to_owned()
        } else {
            let items: Vec<String> = supers.iter().map(|x| format!("{x:?}")).collect();
            format!("&[{}]", items.join(", "))
        };
        s.push_str(&format!("    ({key:?}, {supers_lit}),\n"));
    }
    s.push_str("];\n");
    s
}
