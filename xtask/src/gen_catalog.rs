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
//! Three further tables ride the pipeline: curated return-fact refinements
//! ([`gen_return_facts`], `return_facts.toml`) and the ADR-0069 declared-return
//! floor in both its halves — function-keyed ([`gen_declared_returns`],
//! `phpstan-mining/declared_returns.toml`) and `Class::method`-keyed
//! ([`gen_declared_method_returns`], `phpstan-mining/declared_method_returns.toml`,
//! issue #673), both sourced by `cargo xtask mine-function-map`.
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
use std::path::Path;

use crate::corpus::repo_root;

/// What a run did, for its own output — a `--check` run that says "emitted" is
/// telling the reader it wrote something.
fn verb(check: bool) -> &'static str {
    if check { "verified" } else { "emitted" }
}

/// Write a rendered table — or, in check mode, assert the committed file already
/// **is** it, byte for byte.
///
/// The check half is the point. A generator whose output is committed has two
/// sources of truth for one artefact, and they drift silently: editing a
/// committed header (as the 2026-08-15 comment-compression pass did) leaves the
/// render template behind, so the next regeneration of any table reverts files
/// nobody touched.
fn emit(dst: &Path, text: &str, check: bool) -> Result<(), String> {
    if !check {
        return std::fs::write(dst, text).map_err(|e| format!("write {}: {e}", dst.display()));
    }
    let have = std::fs::read_to_string(dst).map_err(|e| format!("read {}: {e}", dst.display()))?;
    if have == text {
        return Ok(());
    }
    let (line, committed, rendered) = have
        .lines()
        .zip(text.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map_or((0, "", ""), |(i, (a, b))| (i + 1, a, b));
    Err(format!(
        "{} is not what the generator renders — `cargo xtask gen-catalog` would rewrite it.\n  \
         first difference at line {line}:\n    committed: {committed}\n    rendered:  {rendered}\n  \
         Edit the render template in xtask/src/gen_catalog.rs, not the generated file.",
        dst.display()
    ))
}

/// Entry point for `cargo xtask gen-catalog`.
///
/// `check` renders every table and compares it with what is committed instead of
/// writing — `cargo xtask gen-catalog --check`. That mode exists because the
/// generator once drifted from its own output: the committed headers had been
/// edited (the 2026-08-15 comment-compression pass) while the render templates
/// still emitted the older, longer text, so the next person to regenerate ANY
/// table silently reverted three files they never meant to touch, and their
/// reviewer saw churn with nothing to do with the change. A generator whose
/// output is committed has to be idempotent, and now a test says so.
pub fn run(check: bool) -> Result<(), String> {
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
    emit(&dst, &out, check)?;

    println!(
        "gen-catalog: {} classes/interfaces {}, {} enums skipped → {}",
        table.len(),
        verb(check),
        skipped_enums,
        dst.display()
    );

    let out = render_display_names(&display);
    let dst = repo_root().join("crates/steins-catalog/src/display_names_generated.rs");
    emit(&dst, &out, check)?;
    println!("gen-catalog: {} display-name rows {} → {}", display.len(), verb(check), dst.display());

    gen_return_facts(check)?;
    gen_resource_returns(check)?;
    gen_declared_returns(check)?;
    gen_declared_method_returns(check)?;
    gen_param_facts(check)?;
    Ok(())
}

/// Regenerate the **resource-return** table (ADR-0056 §8) from
/// `resource_returns.toml` into `resource_returns_generated.rs`. Only two
/// fields survive transcription — name and whether the stub's `@return`
/// carries a `false` arm; the rest (stub path, probe transcript, confidence
/// grade) is evidence that belongs in the source of record, not here.
fn gen_resource_returns(check: bool) -> Result<(), String> {
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
    emit(&dst, &out, check)?;
    println!("gen-catalog: {} resource-return rows {} → {}", table.len(), verb(check), dst.display());
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
fn gen_declared_returns(check: bool) -> Result<(), String> {
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
    emit(&dst, &out, check)?;
    println!(
        "gen-catalog: {} declared-return rows + {} version-sensitive names {} → {}",
        rows.len(),
        sensitive.len(),
        verb(check),
        dst.display()
    );
    Ok(())
}


/// Regenerate the **class-method** declared-return table (issue #673) from
/// `phpstan-mining/declared_method_returns.toml` into
/// `declared_method_returns_generated.rs` — the method twin of
/// [`gen_declared_returns`], sourced by `cargo xtask mine-function-map --methods`.
///
/// Keys are `class::method`, lowercased whole and sorted, so one binary search
/// answers both halves of the name; the static bit rides beside the spelling
/// because functionMap's key grammar cannot spell it and only reflection knows.
fn gen_declared_method_returns(check: bool) -> Result<(), String> {
    let src = repo_root().join("docs/research/phpstan-mining/declared_method_returns.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: MethodEnvelopeDoc =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut rows: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for (key, row) in &doc.declared {
        let key = key.to_ascii_lowercase();
        if !key.contains("::") {
            return Err(format!("method row `{key}` is not a `class::method` key"));
        }
        rows.insert(key, row.clone());
    }
    let mut sensitive: BTreeMap<String, (u16, u16)> = BTreeMap::new();
    for (key, minor) in &doc.version_sensitive {
        let parsed = parse_minor(minor)
            .ok_or_else(|| format!("unparseable version_sensitive minor `{minor}` for `{key}`"))?;
        sensitive.insert(key.to_ascii_lowercase(), parsed);
    }

    // The shadow keys ship as a table of their own: the consumer needs them at the
    // call site, where "no row on this class" and "functionMap states this class
    // differently" must answer differently.
    let mut blocked: Vec<String> = Vec::new();
    for key in doc.blocked.keys() {
        let key = key.to_ascii_lowercase();
        if !key.contains("::") {
            return Err(format!("blocked key `{key}` is not a `class::method` key"));
        }
        if rows.contains_key(&key) {
            return Err(format!("`{key}` is both admitted and blocked — the miner disagrees with itself"));
        }
        blocked.push(key);
    }
    blocked.sort();

    let out = render_declared_method_returns(&doc.meta, &doc.counts, &rows, &sensitive, &blocked);
    let dst = repo_root().join("crates/steins-catalog/src/declared_method_returns_generated.rs");
    emit(&dst, &out, check)?;
    println!(
        "gen-catalog: {} declared method-return rows + {} version-sensitive keys + {} shadow keys {} → {}",
        rows.len(),
        sensitive.len(),
        blocked.len(),
        verb(check),
        dst.display()
    );
    Ok(())
}

/// The shape of `declared_method_returns.toml`. The `[exclusions]` sections
/// document refusals and are deliberately not read — nothing is generated from
/// them. `[blocked]` is the one refusal record that IS read, because a refused
/// key is not merely an absence: it shadows the ancestor's row.
#[derive(serde::Deserialize)]
struct MethodEnvelopeDoc {
    meta: EnvelopeMeta,
    counts: MethodEnvelopeCounts,
    /// `class::method` -> `[canonical spelling, is_static]`.
    #[serde(default)]
    declared: BTreeMap<String, (String, bool)>,
    #[serde(default)]
    version_sensitive: BTreeMap<String, String>,
    /// `class::method` -> `[why it was refused, the ancestor it shadows]`. Only
    /// the key is generated; the pair is the audit trail.
    #[serde(default)]
    blocked: BTreeMap<String, (String, String)>,
}

#[derive(serde::Deserialize)]
struct MethodEnvelopeCounts {
    keys: usize,
    alternates_disagree: usize,
    not_lowerable: usize,
    not_lowerable_object_or_resource: usize,
    class_missing_rows: usize,
    class_missing_classes: usize,
    method_missing: usize,
    reflection_disagree: usize,
    engine_untyped: usize,
    engine_mixed: usize,
    blocked: usize,
    admitted: usize,
    admitted_static: usize,
    admitted_rich: usize,
}

/// Render the committed class-method declared-return tables, provenance header
/// and all.
fn render_declared_method_returns(
    meta: &EnvelopeMeta,
    counts: &MethodEnvelopeCounts,
    rows: &BTreeMap<String, (String, bool)>,
    sensitive: &BTreeMap<String, (u16, u16)>,
    blocked: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(
        "// @generated by `cargo xtask gen-catalog` from\n\
         // docs/research/phpstan-mining/declared_method_returns.toml — DO NOT EDIT BY HAND.\n\
         //\n\
         // Builtin DECLARED METHOD RETURN TYPES: the ADR-0069 Asserted floor, keyed\n\
         // `class::method` (issue #673). The function table beside this one raised the\n\
         // floor for `str_repeat($s, $n)`; this one raises it for `$f->fgets()`, where\n\
         // the receiver is a builtin class BY DECLARATION and the project chain answers\n\
         // nothing.\n\
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
    let _ = writeln!(s, "//   {:>5}  `Class::method` entries (after the delta ladder)", counts.keys);
    let _ = writeln!(s, "//   {:>5}  keys whose alternate signatures disagree on the return type", counts.alternates_disagree);
    let _ = writeln!(s, "//   {:>5}  rows the declared-contract arm lane cannot carry, of which", counts.not_lowerable);
    let _ = writeln!(s, "//   {:>5}    objects / `callable` / `resource` / `void` / the `self`-family keywords", counts.not_lowerable_object_or_resource);
    let _ = writeln!(s, "//   {:>5}  rows on {} classes the pinned engine does not have", counts.class_missing_rows, counts.class_missing_classes);
    let _ = writeln!(s, "//   {:>5}  rows whose class the engine has WITHOUT the method", counts.method_missing);
    let _ = writeln!(s, "//   {:>5}  rows the arm-wise engine countersign refuses", counts.reflection_disagree);
    let _ = writeln!(s, "//   {:>5}  rows REFUSED because the engine declares no return type", counts.engine_untyped);
    let _ = writeln!(s, "//   {:>5}  rows REFUSED because the engine declares `mixed`", counts.engine_mixed);
    let _ = writeln!(s, "//   {:>5}  refused keys that SHADOW an ancestor's row (the second table below)", counts.blocked);
    let _ = writeln!(s, "//   {:>5}  ADMITTED (the table below), of which", counts.admitted);
    let _ = writeln!(s, "//   {:>5}    STATIC, by the engine's own reckoning", counts.admitted_static);
    let _ = writeln!(s, "//   {:>5}    RICHER than a single-base envelope", counts.admitted_rich);
    s.push_str(
        "//\n\
         // GRADE: every row seeds `Asserted`, never `Verified` (ADR-0069 §2), and an\n\
         // object-returning row is in the contract lane at all only because ADR-0093\n\
         // §3.1 sources it from a declaration. There is no Verified twin waiting to be\n\
         // built: a native stub's `@return` would be one, and functionMap is not a stub\n\
         // — it is a third party's claim about the engine, which is what the Asserted\n\
         // lane is for. The proof layer's all-Verified premise rule therefore excludes\n\
         // every row here from every finding by construction.\n\
         //\n\
         // WHERE IT SPEAKS: at a method or static call whose receiver's DECLARED class\n\
         // is a builtin, consulted after the project chain has answered nothing —\n\
         // ADR-0049 A17's declared-receiver lane supplies the class, A16 licenses\n\
         // reading a declaration off an unproven receiver, and a builtin class has no\n\
         // `ClassDecl` so it never reaches `resolve_in_chain` as a project class. A\n\
         // project class extending a builtin keeps its own declaration on every name it\n\
         // declares; this table answers the inherited names alone, and the two cannot\n\
         // disagree because the walk stops at the first project declaration it finds.\n\
         //\n\
         // INHERITANCE: the key is where functionMap puts the row, which may be a\n\
         // subclass of the class that declares the method. The consuming lookup walks\n\
         // the builtin hierarchy (ADR-0043) from the receiver's declared class upward,\n\
         // so `SplFileInfo::getPath` answers for an `SplFileObject` receiver — sound\n\
         // because PHP enforces return covariance at class-declaration time, making the\n\
         // declaring class's NATIVE, NON-`mixed` engine envelope an upper bound on\n\
         // every override (ADR-0049 A16). That is exactly why no row here was admitted\n\
         // over an untyped or `mixed` engine answer: such a row bounds nothing, and the\n\
         // walk would hand it to every descendant.\n\
         //\n\
         // ...and why the walk needs `BLOCKED_METHOD_KEYS` below. \"No row on the\n\
         // child\" is not the same fact as \"functionMap never mentioned the child\":\n\
         // only 957 of the 6,606 reduced keys became rows, so a key the map STATES and\n\
         // the miner dropped or refused is positive evidence that the child's own\n\
         // declaration differs from the ancestor's. `PDOException::getCode` is the\n\
         // witness the review found — stated as `['']`, unparseable, and the walk read\n\
         // it as `RuntimeException`'s `int` where PHP returns `\"HY000\"`.\n\
         //\n\
         // Each row: (lowercased `class::method`, canonical phpdoc spelling, whether\n\
         // the engine declares the method `static`). Re-lowered through the same\n\
         // `lower_str` → `flatten_arms` seam a project method's declared return takes.\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static DECLARED_METHOD_RETURNS: &[(&str, &str, bool)] = &[\n");
    for (key, (ty, is_static)) in rows {
        let _ = writeln!(s, "    ({key:?}, {ty:?}, {is_static}),");
    }
    s.push_str("];\n\n");
    s.push_str(
        "// The A11-shaped change oracle, keyed the same way: `class::method` entries\n\
         // whose declared RETURN type moves between two adjacent supported minors, keyed\n\
         // to the minor it moved AT. A project whose declared PhpTarget is below that\n\
         // minor declines the row; unknown target admits (the row is Asserted anyway,\n\
         // ADR-0069 §3). Listed independently of the table above since a key can be\n\
         // version-sensitive without an admitted row.\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static METHOD_RETURN_VERSION_SENSITIVE: &[(&str, (u16, u16))] = &[\n");
    for (key, (major, minor)) in sensitive {
        let _ = writeln!(s, "    ({key:?}, ({major}, {minor})),");
    }
    s.push_str("];\n\n");
    s.push_str(
        "// The SHADOW keys: `class::method` entries functionMap states and the miner\n\
         // dropped or refused, whose ancestor carries an admitted row for the same\n\
         // method. The consuming walk answers NOTHING at these keys rather than\n\
         // climbing to the ancestor's row, since the map itself says the child's\n\
         // declaration is not the ancestor's. Disjoint from the table above by\n\
         // construction — the generator refuses a key that is in both.\n\
         // Sorted by key for binary search.\n\n",
    );
    s.push_str("pub(crate) static BLOCKED_METHOD_KEYS: &[&str] = &[\n");
    for key in blocked {
        let _ = writeln!(s, "    {key:?},");
    }
    s.push_str("];\n");
    s
}

/// The per-parameter facts table (issue #382): `param_facts.toml`, mined off the
/// engine's own arginfo by `cargo xtask mine-param-facts`, into
/// `param_facts_generated.rs`.
///
/// One row per internal function the mining build had — including the ones that
/// carry nothing, because "mined, and empty" has to be distinguishable from
/// "never looked at", and because `cargo xtask fold-probe --names <name>` reads
/// these facts to generate a candidate's tuples.
fn gen_param_facts(check: bool) -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/param_facts.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: ParamDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    let mut rows: BTreeMap<String, ParamRow> = BTreeMap::new();
    for (name, row) in &doc.r#fn {
        rows.insert(name.to_ascii_lowercase(), row.clone());
    }

    let out = render_param_facts(&doc.meta, &doc.counts, &rows);
    let dst = repo_root().join("crates/steins-catalog/src/param_facts_generated.rs");
    emit(&dst, &out, check)?;
    println!(
        "gen-catalog: {} parameter-fact rows {} → {}",
        rows.len(),
        verb(check),
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
    let _ = writeln!(s, "//   {:>5}  rows (one per internal function — an empty row is a FACT)", counts.rows);
    let _ = writeln!(s, "//   {:>5}    of those, carrying by-ref / declared-callable / variadic", counts.hazardous);
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
        let _ = writeln!(
            s,
            "    ({name:?}, ParamFacts {{ by_ref: &{:?}, callable: &{:?}, variadic: &{:?}, \
             optional: &{:?}, params: &{:?}, param_names: &{:?}, params_required: {} }}),",
            r.by_ref, r.callable, r.variadic, r.optional, r.params, r.param_names, r.params_required
        );
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
         // Builtin DECLARED RETURN TYPES: the ADR-0069 Asserted floor (issues #73/#79,\n\
         // widened for arrays by ADR-0071). Without a live engine a builtin call with\n\
         // variable operands types as `unknown`; these rows raise that floor to the\n\
         // builtin's declared type.\n\
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
         // The object half of that bucket is no longer deferred. ADR-0071 emptied the\n\
         // shaped-array bucket by giving `array`/`list<T>`/`array<K, V>`/`array{…}` a\n\
         // structural denotation, and the same move reached the class rows through\n\
         // `subsumes_class`'s reflexivity — a row naming the class the engine names\n\
         // countersigns on that alone, a row naming a different one stays `Maybe` and is\n\
         // refused. What ADR-0093 §3 then added is the missing half: those rows now\n\
         // SPELL, so `date_create` dumps `DateTime|false` instead of `unknown`, under\n\
         // §3.1's sourcing rule (an object arm enters the contract lane only from a\n\
         // declaration — this table is one). ADR-0069 §5 / ADR-0071 §2.3 lift for the\n\
         // object rows on that ruling (ADR-0093 is PENDING ratification).\n\
         //\n\
         // The `Class::method` rows this count calls skipped are no longer deferred\n\
         // either: they are `declared_method_returns_generated.rs`, mined by the same\n\
         // pipeline at the same pin (issue #673). What is LEFT in the bucket the count\n\
         // names is `callable`, the intersections, `resource` and `void` — those have\n\
         // no extensional denotation the countersign could use, so it could only\n\
         // answer `Maybe`, which ADR-0069 §3 refuses.\n\
         //\n\
         // GRADE: every row seeds `Asserted`, never `Verified` (ADR-0069 §2) — it\n\
         // reaches the dump surface and contracts-tier reasoning, but the proof\n\
         // layer's all-Verified premise rule excludes it from every finding.\n\
         //\n\
         // WHEN IT SPEAKS: per NAME, not per run, where the folder's reflected\n\
         // envelope yielded None (`--no-php` is the total case). With a live engine it\n\
         // speaks only where the engine is SILENT (unloaded extension, or no declared\n\
         // return type); the engine is never overridden. The absence family never\n\
         // reads these rows — existence is a boot-surface fact, complementary to an\n\
         // absence finding.\n\
         //\n\
         // Each row: (lowercased builtin name, canonical phpdoc spelling). Re-lowered\n\
         // through the same `lower_str` → `flatten_arms` seam a PROJECT function's\n\
         // declared return takes (issue #60), seeded Asserted. Sorted by key for\n\
         // binary search.\n\n",
    );
    s.push_str("pub(crate) static DECLARED_RETURNS: &[(&str, &str)] = &[\n");
    for (key, ty) in rows {
        let _ = writeln!(s, "    ({key:?}, {ty:?}),");
    }
    s.push_str("];\n\n");
    s.push_str(
        "// The A11-shaped change oracle: names whose declared RETURN type moves between\n\
         // two adjacent supported minors, keyed to the minor it moved AT. A project whose\n\
         // declared PhpTarget is below that minor declines the row; unknown target admits\n\
         // (the row is Asserted anyway, ADR-0069 §3). Listed independently of the table\n\
         // above since a name can be version-sensitive without an admitted row.\n\
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
fn gen_return_facts(check: bool) -> Result<(), String> {
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
    emit(&dst, &out, check)?;
    println!("gen-catalog: {} return-fact rows {} → {}", table.len(), verb(check), dst.display());
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
         // Each row: (lowercased name, php-src's DECLARED casing). Display fidelity\n\
         // only — judgments compare via the case-insensitive `class_eq`, so nothing\n\
         // decides on this table. Unlike HIERARCHY, enum rows are kept: HIERARCHY\n\
         // excludes them to guard the is-a oracle against an incomplete super-edge\n\
         // set, a soundness gate that doesn't apply here.\n\
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
         // Each row: (lowercased class/interface name, DIRECT supertypes with declared\n\
         // casing — `extends` then `implements`). The is-a oracle (ADR-0043) walks\n\
         // these transitively; an absent name is an unknown external (→ `Unknown`,\n\
         // never `No`). Builtin enums are omitted (incomplete implicit-interface/\n\
         // backing data — see gen_catalog.rs).\n\
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

#[cfg(test)]
mod tests {
    /// **Regenerating is a no-op**, which is the property a committed generated
    /// file needs and did not have.
    ///
    /// The templates and their output are two sources of truth for one artefact,
    /// and they drifted: the 2026-08-15 comment-compression pass edited the
    /// committed headers of `hierarchy_generated.rs`,
    /// `display_names_generated.rs` and `declared_returns_generated.rs`, the
    /// render templates kept emitting the older text, and so the next
    /// `cargo xtask gen-catalog` — run for an unrelated table — reverted three
    /// files nobody had touched. That is invisible to a reviewer reading a diff
    /// for something else, which is what makes it worth a test rather than a
    /// note.
    ///
    /// A failure here means the render template and the committed file disagree.
    /// Fix the **template**; the generated file is the output, not the source.
    #[test]
    fn regenerating_the_catalog_tables_changes_nothing() {
        super::run(true).expect("`cargo xtask gen-catalog` must be a no-op on a clean tree");
    }
}
