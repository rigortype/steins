//! `gen-catalog`: regenerate the builtin class-hierarchy table from the pinned
//! php-src mining data (ADR-0043 §3).
//!
//! # Source of record
//!
//! `docs/research/phpsrc-mining/hierarchy.toml` is the *source of record*: 368
//! production class/interface/enum declarations mined from php-src
//! `6bc7c26cf67a9480b5ef9d6191aebe87fa931183` and cross-checked against PHP
//! 8.5.8. It records **direct** edges (`extends` + `implements`); the is-a
//! oracle computes the transitive closure by walking `builtin_class_supers`
//! ([the crosscheck](docs/research/phpsrc-mining/crosscheck.txt) verified that
//! closure-of-direct-edges == runtime `class_implements` for a sample).
//!
//! This command reads that TOML with the `toml` crate (an xtask-only dependency;
//! the shipped `steins-catalog` crate stays dependency-free) and emits a
//! committed Rust source file — `crates/steins-catalog/src/hierarchy_generated.rs`
//! — containing a single sorted `&[(&str, &[&str])]` table for binary-search
//! lookup. No runtime TOML parsing, no new shipped dependency.
//!
//! # What is emitted, and what is deliberately not
//!
//! * **`kind = 'class'` and `kind = 'interface'` rows are emitted** — direct
//!   supers, lowercased key preserving declared-casing supers. Namespaced names
//!   are kept (backslash preserved in the key); the oracle resolves them the same
//!   way it resolves a global name.
//! * **`kind = 'enum'` rows are SKIPPED** — the mining extractor did not capture
//!   an enum's implicit `UnitEnum`/`BackedEnum` interfaces nor its backing, so
//!   the recorded super-set (empty) is *incomplete*. Emitting it would let the
//!   oracle read a builtin enum as a fully-enumerated root and return a spurious
//!   `No` against `UnitEnum`/`BackedEnum` — unsound. Absence → `None` → `Unknown`
//!   is the FP-safe verdict ADR-0043 §3 requires when enumeration is incomplete.
//!   (Re-mining enum backing would let these move to a sound `Some`.)
//!
//! Two further tables ride the same pipeline: the curated return-fact refinements
//! ([`gen_return_facts`], from `phpsrc-mining/return_facts.toml`) and the ADR-0069
//! declared-return floor ([`gen_declared_returns`], from
//! `phpstan-mining/declared_returns.toml`, whose own source of record is produced
//! by `cargo xtask mine-function-map`).
//!
//! A fourth table is a *byproduct* of the hierarchy source: the builtin-class
//! **display-name** table (`display_names_generated.rs`), lowercased key → the
//! casing php-src declares. It exists because `ContractTy::Class` case-folds on
//! the way in (that is what makes `class_eq` comparison work) and the project
//! index knows nothing about a builtin, so without it the dump surface renders
//! `gmp` where PHPStan renders `GMP` (the ADR-0069 third-amendment residual).
//! Unlike the hierarchy table it **keeps the enum rows**: the enum exclusion
//! guards the is-a oracle against an incomplete super-edge set, and a display
//! name has no such soundness gate — the casing is the casing.
//!
//! Run `cargo xtask gen-catalog` after editing any of those TOMLs; the committed
//! generated files must stay in sync (a test asserts the table is sorted and
//! self-consistent).

use std::collections::BTreeMap;

use crate::corpus::repo_root;

/// Entry point for `cargo xtask gen-catalog`.
pub fn run() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/hierarchy.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: Doc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    // Lowercase-keyed, sorted (BTreeMap) → deterministic binary-search table.
    // Enums are skipped (see module docs); classes/interfaces are kept.
    let mut table: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // The display-name table keeps EVERY row, enums included (see module docs).
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
    gen_declared_returns()?;
    Ok(())
}

/// Regenerate the builtin **declared-return floor** (ADR-0069, issues #73/#79)
/// from `docs/research/phpstan-mining/declared_returns.toml` into
/// `crates/steins-catalog/src/declared_returns_generated.rs`.
///
/// Two tables come out of one source of record: the declared rows themselves, and
/// the A11-shaped change oracle (`[version_sensitive]`) the consumer's target gate
/// reads. The TOML is produced by `cargo xtask mine-function-map`, which owns the
/// mining, the lowerability filter and the engine cross-check; this function only
/// transcribes it, so the two commands can be re-run independently (mining needs a
/// phpstan-src checkout and a live `php`; generation needs neither).
fn gen_declared_returns() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpstan-mining/declared_returns.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: EnvelopeDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    // Lowercase-keyed, sorted (BTreeMap) → deterministic binary-search tables.
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

/// Parse a `"8.5"` minor spelling to `(major, minor)`.
fn parse_minor(s: &str) -> Option<(u16, u16)> {
    let (major, minor) = s.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The shape of `declared_returns.toml`. Exclusion sections are documentation of
/// the refusals and are deliberately not read here — nothing is generated from them.
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
/// `docs/research/phpsrc-mining/return_facts.toml` into
/// `crates/steins-catalog/src/return_facts_generated.rs`. Each row is a curated
/// refinement (a phpdoc type string) keyed by the lowercased builtin name; the
/// table may be empty (R1 lands zero rows — the reflected envelope alone serves
/// the bool family). See the TOML header for the sourcing discipline.
fn gen_return_facts() -> Result<(), String> {
    let src = repo_root().join("docs/research/phpsrc-mining/return_facts.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let doc: ReturnDoc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;

    // Lowercase-keyed, sorted (BTreeMap) → deterministic binary-search table.
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

/// The `[[function]]` array-of-tables shape of `return_facts.toml`. A row carries
/// the curated `refinement` phpdoc string; other keys (evidence/probe notes) are
/// documentation and ignored here.
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
