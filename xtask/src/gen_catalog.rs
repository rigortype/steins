//! `gen-catalog`: regenerate the builtin class-hierarchy table from the pinned
//! php-src mining data (ADR-0043 §3).
//!
//! # Source of record
//!
//! `docs/research/phpsrc-mining/hierarchy.toml` is the *source of record*: 368
//! production class/interface/enum declarations mined from php-src
//! `6bc7c26cf67a9480b5ef9d6191aebe87fa931183` and cross-checked against PHP
//! 8.5.8. It records **direct** edges (`extends` + `implements`); the is-a
//! oracle computes the transitive closure by walking [`builtin_class_supers`]
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
//! Run `cargo xtask gen-catalog` after editing `hierarchy.toml`; the committed
//! generated file must stay in sync (a test asserts the table is sorted and
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
    let mut skipped_enums = 0usize;
    for c in &doc.class {
        if c.kind == "enum" {
            skipped_enums += 1;
            continue;
        }
        let key = c.name.to_ascii_lowercase();
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

    gen_return_facts()?;
    Ok(())
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
