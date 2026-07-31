//! `mine-function-map`: build the committed declared-envelope mining TOML from a
//! pinned phpstan-src checkout (ADR-0069 / issue #73 slice F1).
//!
//! # The pipeline, and why it has three stages
//!
//! 1. **PHP reads PHP.** `docs/research/phpstan-mining/mine_function_map.php` is
//!    `require`d by the real engine and emits JSON — the functionMap key grammar
//!    (alternate-signature ticks, `::` method rows) and the forward-applied delta
//!    ladder are PHPStan's own, and transcribing either into Rust would be a
//!    second implementation to keep in sync.
//! 2. **Rust lowers.** Every candidate return-type string is lowered through
//!    [`steins_contract::lower_str`] and kept only when it is **envelope-
//!    representable**: a bare scalar base or its `?T` nullable pair, exactly what
//!    `steins-infer`'s `envelope_fact` can turn into one value-domain fact. A row
//!    that cannot become an envelope is dropped here, at generation time, so the
//!    shipped table has no rows the seam would silently discard.
//! 3. **The engine countersigns.** Every surviving row is put to the *real*
//!    sidecar's `reflect(name)` at the pin (PHP 8.5.8), and admitted only when the
//!    row's envelope **subsumes** the engine's own declaration
//!    ([`steins_contract::normalize::subsumes`] — the same acceptance relation the
//!    checker enforces). An envelope is an upper bound, so a coarser row over a
//!    narrower engine type (`bool` over the engine's `true`) is agreement; a row
//!    the engine's declaration escapes (`string` over the engine's `?string`,
//!    `int` over the engine's `bool`) is disagreement and the row is excluded,
//!    verbatim. A name the engine does not know as a function is excluded too. A
//!    function the engine knows but for which it declares **no** return type is
//!    not a disagreement — those rows are precisely where the map adds value over
//!    reflection.
//!
//! This is ADR-0069 §3's "rot answered by machinery, not diligence": the per-row
//! evidence bar of ADR-0056 is automated rather than waived.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-function-map [/path/to/phpstan-src]
//! ```
//!
//! The default checkout path is `~/repo/php/phpstan-src`. The checkout is read
//! **only** — never modified, never checked out to another ref. Its `HEAD` is
//! recorded in the emitted TOML as the mining pin.
//!
//! Output: `docs/research/phpstan-mining/declared_envelopes.toml` (the source of
//! record). `cargo xtask gen-catalog` turns that into the shipped Rust table.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use steins_contract::ContractTy;
use steins_domain::Base;
use steins_sidecar::Sidecar;

use crate::corpus::repo_root;

/// The mining script's JSON shape.
#[derive(serde::Deserialize)]
struct Mined {
    total_keys: usize,
    methods_skipped: usize,
    malformed: Vec<String>,
    rows: BTreeMap<String, String>,
    alternates_disagree: BTreeMap<String, Vec<String>>,
    version_sensitive: BTreeMap<String, Vec<String>>,
}

/// Entry point for `cargo xtask mine-function-map`.
pub fn run(checkout: Option<&str>) -> Result<(), String> {
    let root = match checkout {
        Some(p) => PathBuf::from(p),
        None => default_checkout()?,
    };
    if !root.join("resources/functionMap.php").is_file() {
        return Err(format!("{} is not a phpstan-src checkout", root.display()));
    }
    let pin = git_head(&root)?;

    let mined = run_miner(&root)?;
    println!(
        "mine-function-map: {} keys, {} method rows skipped, {} alternate-disagreement names, {} plain-function rows",
        mined.total_keys,
        mined.methods_skipped,
        mined.alternates_disagree.len(),
        mined.rows.len(),
    );
    if !mined.malformed.is_empty() {
        return Err(format!("{} malformed signature rows: {:?}", mined.malformed.len(), mined.malformed));
    }

    // Stage 2 — lowerability. `canonical_envelope` is the whole filter: a row that
    // does not become one `General{base, nullable}` fact is not an envelope.
    //
    // The drop is counted BY REASON, because the reasons are not noise: the rows
    // where functionMap genuinely exceeds reflection — shaped arrays, `T|false`
    // unions — are the ones ADR-0069 §5 hands to a future contracts-grade slice that
    // seeds through the full `lower_str` lowering instead of `envelope_fact`. These
    // numbers are that slice's size estimate.
    let mut candidates: BTreeMap<String, String> = BTreeMap::new();
    let mut dropped = Dropped::default();
    for (name, ty) in &mined.rows {
        match canonical_envelope(ty) {
            Some(canon) => {
                candidates.insert(name.clone(), canon);
            }
            None => dropped.charge(ty),
        }
    }
    println!(
        "mine-function-map: {} envelope-representable; {} dropped as richer than an envelope \
         ({} shaped arrays/lists, {} multi-base unions, {} scalar refinements, {} object/resource, \
         {} void/never/mixed, {} unparseable)",
        candidates.len(),
        dropped.total(),
        dropped.arrays,
        dropped.unions,
        dropped.refinements,
        dropped.objects,
        dropped.voidish,
        dropped.unparseable,
    );

    // Stage 3 — the engine countersigns.
    let mut sidecar = Sidecar::spawn().map_err(|e| format!("spawn php sidecar: {e}"))?;
    let engine_version = sidecar
        .env()
        .map(|e| e.php_version)
        .ok_or_else(|| "sidecar `env` failed — cannot record the cross-check engine".to_owned())?;
    println!("mine-function-map: cross-checking {} rows against PHP {engine_version}", candidates.len());

    let mut admitted: BTreeMap<String, String> = BTreeMap::new();
    let mut disagree: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut typeless = 0usize;
    for (name, canon) in &candidates {
        let Some(refl) = sidecar.reflect(name) else {
            return Err(format!("sidecar `reflect({name})` failed — refusing to mine a partial table"));
        };
        if !refl.function_exists {
            missing.push(name.clone());
            continue;
        }
        match refl.return_type.as_deref() {
            // The engine declares nothing: the map is adding value, not contradicting.
            None => {
                typeless += 1;
                admitted.insert(name.clone(), canon.clone());
            }
            // An envelope is an UPPER BOUND, so the test is subsumption, not
            // equality: the row is admitted exactly when everything the engine
            // declares fits inside it.
            Some(engine_ty) if envelope_contains(canon, engine_ty) => {
                admitted.insert(name.clone(), canon.clone());
            }
            Some(engine_ty) => {
                disagree.insert(name.clone(), vec![canon.clone(), engine_ty.to_owned()]);
            }
        }
    }

    println!(
        "mine-function-map: {} admitted ({} of them where the engine declares no return type), {} reflection disagreements, {} names the engine does not know",
        admitted.len(),
        typeless,
        disagree.len(),
        missing.len()
    );

    let counts = Counts {
        total_keys: mined.total_keys,
        methods_skipped: mined.methods_skipped,
        alternates_disagree: mined.alternates_disagree.len(),
        dropped,
        reflection_disagree: disagree.len(),
        reflection_missing: missing.len(),
        admitted: admitted.len(),
        engine_typeless: typeless,
    };
    let toml = render(
        &pin,
        &engine_version,
        &counts,
        &admitted,
        &mined.version_sensitive,
        &mined.alternates_disagree,
        &disagree,
        &missing,
    );
    let dst = repo_root().join("docs/research/phpstan-mining/declared_envelopes.toml");
    std::fs::write(&dst, &toml).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("mine-function-map: wrote {}", dst.display());
    println!("mine-function-map: now run `cargo xtask gen-catalog`");
    Ok(())
}

/// The counts the provenance header carries.
struct Counts {
    total_keys: usize,
    methods_skipped: usize,
    alternates_disagree: usize,
    dropped: Dropped,
    reflection_disagree: usize,
    reflection_missing: usize,
    admitted: usize,
    engine_typeless: usize,
}

/// Rows dropped for being **richer than the envelope lowering**, split by reason
/// (ADR-0069 §5). The first two buckets are the ones that matter: they are where
/// functionMap says more than reflection ever could, and they size the future
/// contracts-grade slice rather than merely explaining a subtraction.
#[derive(Default)]
struct Dropped {
    /// `array{…}`, `list<T>`, `array<K, V>`, `iterable<T>` — the shaped-array rows.
    arrays: usize,
    /// Multi-base unions that are not the `?T` nullable pair — `string|false`,
    /// `int|string`, the whole `T|false` failure-arm family.
    unions: usize,
    /// Scalar types richer than a base: `non-empty-string`, `int<0, 255>`,
    /// `positive-int`, literal types, the opaque string family.
    refinements: usize,
    /// Objects, class names, `resource`, `callable`.
    objects: usize,
    /// `void`, `never`, `mixed`, and the `mixed`-minus-a-cut spellings.
    voidish: usize,
    /// A type string the phpdoc grammar does not accept at all (an empty return
    /// type, a PHPStan-internal spelling such as `__benevolent<…>`).
    unparseable: usize,
}

impl Dropped {
    fn total(&self) -> usize {
        self.arrays + self.unions + self.refinements + self.objects + self.voidish + self.unparseable
    }

    /// Charge one dropped row to its reason bucket, judged on the LOWERED type so
    /// the classification is the grammar's, not a substring guess.
    fn charge(&mut self, ty: &str) {
        let Some(lowered) = steins_contract::lower_str(ty) else {
            self.unparseable += 1;
            return;
        };
        let bucket = match &lowered {
            ContractTy::ArrayAny { .. }
            | ContractTy::ListOf { .. }
            | ContractTy::MapOf { .. }
            | ContractTy::IterableOf { .. }
            | ContractTy::Shape { .. } => &mut self.arrays,
            ContractTy::Union(_) | ContractTy::Inter(_) => &mut self.unions,
            ContractTy::IntIn(_)
            | ContractTy::StrWith(_)
            | ContractTy::StrOpaque
            | ContractTy::LitInt(_)
            | ContractTy::LitFloat(_)
            | ContractTy::LitStr(_)
            | ContractTy::LitBool(_)
            | ContractTy::Null => &mut self.refinements,
            ContractTy::Class(_)
            | ContractTy::ObjectAny
            | ContractTy::CallableTy { .. }
            | ContractTy::Opaque => &mut self.objects,
            ContractTy::Mixed | ContractTy::MixedMinus(_) | ContractTy::Never => &mut self.voidish,
            // A bare scalar base would have been admitted by `canonical_envelope`.
            ContractTy::Base(_) => &mut self.refinements,
        };
        *bucket += 1;
    }
}

/// `~/repo/php/phpstan-src`, the owner's read-only working checkout.
fn default_checkout() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_owned())?;
    Ok(PathBuf::from(home).join("repo/php/phpstan-src"))
}

/// The checkout's `HEAD` — the mining pin recorded in the TOML and the generated
/// file. Read-only: `git rev-parse`, nothing else.
fn git_head(root: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["-C", &root.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("git rev-parse in {}: {e}", root.display()))?;
    if !out.status.success() {
        return Err(format!("git rev-parse in {} failed", root.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Run the committed PHP miner over `root` and parse its JSON.
fn run_miner(root: &Path) -> Result<Mined, String> {
    let script = repo_root().join("docs/research/phpstan-mining/mine_function_map.php");
    let out = Command::new("php")
        .arg(&script)
        .arg(root)
        .output()
        .map_err(|e| format!("run php {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!(
            "miner failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse miner JSON: {e}"))
}

/// The canonical envelope spelling of a declared type string, or `None` when it is
/// not envelope-representable.
///
/// "Envelope-representable" is exactly what `steins-infer`'s `envelope_fact` can
/// turn into a single value-domain fact: a bare scalar base (`bool`/`int`/`float`/
/// `string`) or the two-member nullable pair (`?string`, `string|null`). Anything
/// else — a multi-base union (`int|false`), a refinement (`non-empty-string`,
/// `int<0, max>`), an array, an object, `void`, `mixed` — yields `None` and its row
/// is dropped at generation time.
///
/// The output is the *canonical* spelling (`"string"`, `"?string"`), so the shipped
/// table is normalized and two spellings of one envelope compare equal.
fn canonical_envelope(ty: &str) -> Option<String> {
    let lowered = steins_contract::lower_str(ty)?;
    let (base, nullable) = match &lowered {
        ContractTy::Base(b) => (*b, false),
        ContractTy::Union(members)
            if members.len() == 2 && members.iter().any(|m| matches!(m, ContractTy::Null)) =>
        {
            let base = members.iter().find_map(|m| match m {
                ContractTy::Base(b) => Some(*b),
                _ => None,
            })?;
            (base, true)
        }
        _ => return None,
    };
    let name = match base {
        Base::Bool => "bool",
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
    };
    Some(if nullable { format!("?{name}") } else { name.to_owned() })
}

/// Whether the candidate envelope `canon` contains everything the engine's own
/// declaration `engine_ty` admits — the generation-time cross-check of ADR-0069 §3.
///
/// Subsumption rather than equality, because the row is an *envelope*: `bool` over
/// the engine's `true` is a coarser but sound upper bound and the row stands, while
/// `string` over the engine's `?string` lets a null escape and the row is refused.
/// An engine type that does not lower at all is an answer this cannot judge, so it
/// counts as disagreement — the refusing side.
fn envelope_contains(canon: &str, engine_ty: &str) -> bool {
    let (Some(a), Some(b)) = (steins_contract::lower_str(canon), steins_contract::lower_str(engine_ty))
    else {
        return false;
    };
    steins_contract::normalize::subsumes(&a, &b).is_yes()
}

/// Render the committed mining TOML.
#[allow(clippy::too_many_arguments)]
fn render(
    pin: &str,
    engine_version: &str,
    counts: &Counts,
    admitted: &BTreeMap<String, String>,
    version_sensitive: &BTreeMap<String, Vec<String>>,
    alternates_disagree: &BTreeMap<String, Vec<String>>,
    reflection_disagree: &BTreeMap<String, Vec<String>>,
    reflection_missing: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(
        "# Builtin declared-return ENVELOPES — the ADR-0069 Asserted floor's data.\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-function-map`, which runs\n\
         # `mine_function_map.php` against a pinned phpstan-src checkout and then makes\n\
         # the real PHP sidecar countersign every surviving row. Regenerate alongside a\n\
         # `PINNED_PHP` bump, the way `hierarchy.toml` is regenerated — never by hand.\n\
         #\n\
         # LINEAGE (see the root NOTICE file):\n\
         #   Steins <- phpstan-src `resources/functionMap.php`\n\
         #              (MIT, Copyright (c) Ondrej Mirtes and contributors)\n\
         #          <- Phan `src/Phan/Language/Internal/FunctionSignatureMap.php`\n\
         #              (MIT, Copyright (c) 2015 Rasmus Lerdorf,\n\
         #                   Copyright (c) 2015 Andrew Morrison)\n\
         #\n\
         # GRADE: every row here is Asserted, never Verified (ADR-0069 §2). It seeds the\n\
         # dump surface and contracts-tier reasoning; it is never a proof-layer premise.\n\
         # It fires per NAME, wherever the engine's reflected envelope is silent —\n\
         # `--no-php` is only the total case; an unloaded extension or a builtin with no\n\
         # declared return type is the per-name one. Where the engine answers, it wins.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "phpstan_src_commit = {pin:?}");
    let _ = writeln!(s, "crosscheck_php = {engine_version:?}");
    let _ = writeln!(
        s,
        "miner = \"docs/research/phpstan-mining/mine_function_map.php\"\n\
         generator = \"cargo xtask mine-function-map\"\n"
    );

    s.push_str(
        "# total_keys           functionMap entries at the pin, after the delta ladder\n\
         # methods_skipped      `Class::method` rows — the floor is function-keyed, and\n\
         #                      methods stay OUT of this slice entirely\n\
         # alternates_disagree  names whose alternate signatures state different returns\n\
         # not_lowerable        rows richer than the envelope lowering, by reason (below)\n\
         # reflection_disagree  rows the pinned engine's own declaration escapes\n\
         # reflection_missing   names the pinned engine does not know as functions\n\
         # engine_typeless      admitted rows where the engine declares NO return type\n\
         #                      (the rows where the map genuinely adds reach)\n\
         # admitted             rows emitted into the shipped table\n\
         #\n\
         # THE TWO NUMBERS THAT SIZE THE NEXT SLICE (ADR-0069 §5): `methods_skipped`\n\
         # and the `not_lowerable_*` array/union buckets. Those are the rows where\n\
         # functionMap genuinely exceeds what reflection can say — shaped arrays and\n\
         # `T|false` failure unions, plus every method row — and they await a\n\
         # contracts-grade Asserted slice that seeds through the FULL `lower_str`\n\
         # lowering rather than `envelope_fact`. Nothing here is lost data; it is\n\
         # deferred data, counted so the deferral stays visible.\n",
    );
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "total_keys = {}", counts.total_keys);
    let _ = writeln!(s, "methods_skipped = {}", counts.methods_skipped);
    let _ = writeln!(s, "alternates_disagree = {}", counts.alternates_disagree);
    let _ = writeln!(s, "not_lowerable = {}", counts.dropped.total());
    let _ = writeln!(s, "not_lowerable_shaped_arrays = {}", counts.dropped.arrays);
    let _ = writeln!(s, "not_lowerable_multi_base_unions = {}", counts.dropped.unions);
    let _ = writeln!(s, "not_lowerable_scalar_refinements = {}", counts.dropped.refinements);
    let _ = writeln!(s, "not_lowerable_object_or_resource = {}", counts.dropped.objects);
    let _ = writeln!(s, "not_lowerable_void_never_mixed = {}", counts.dropped.voidish);
    let _ = writeln!(s, "not_lowerable_unparseable = {}", counts.dropped.unparseable);
    let _ = writeln!(s, "reflection_disagree = {}", counts.reflection_disagree);
    let _ = writeln!(s, "reflection_missing = {}", counts.reflection_missing);
    let _ = writeln!(s, "engine_typeless = {}", counts.engine_typeless);
    let _ = writeln!(s, "admitted = {}\n", counts.admitted);

    s.push_str(
        "# The admitted rows: lowercased builtin name -> canonical envelope spelling.\n\
         # The consumer re-lowers this string through the SAME `lower_str` ->\n\
         # `envelope_fact` seam the reflected envelope uses (ADR-0069 §2: one\n\
         # lowering, two provenances).\n",
    );
    let _ = writeln!(s, "[envelope]");
    for (name, ty) in admitted {
        let _ = writeln!(s, "{name:?} = {ty:?}");
    }
    s.push('\n');

    s.push_str(
        "# The A11-shaped change oracle: names whose RETURN type moves between two\n\
         # adjacent supported minors, keyed to the minor it moved AT. A project whose\n\
         # declared PhpTarget is not wholly at or above that minor declines the row\n\
         # (an unknown target admits — the row is Asserted anyway, ADR-0069 §3).\n\
         # A name that merely APPEARS at a minor is an existence fact, which this\n\
         # table never speaks to.\n",
    );
    let _ = writeln!(s, "[version_sensitive]");
    for (name, minors) in version_sensitive {
        // The highest boundary is the one that governs: the map states the pin's
        // signature, which is correct only at or above the last change.
        let last = minors.iter().max().cloned().unwrap_or_default();
        let all = minors.join(", ");
        let _ = writeln!(s, "{name:?} = {last:?}  # changed at: {all}");
    }
    s.push('\n');

    s.push_str("# Exclusions, recorded so the refusals are auditable rather than invisible.\n");
    let _ = writeln!(s, "[exclusions]");
    let _ = writeln!(
        s,
        "# Names the pinned engine does not know as functions (an extension this build\n\
         # does not load, or a name gone from the engine). Existence is a boot-surface\n\
         # fact and this table refuses to guess at it.\n\
         reflection_missing = ["
    );
    for name in reflection_missing {
        let _ = writeln!(s, "  {name:?},");
    }
    s.push_str("]\n\n");

    s.push_str(
        "# Alternate signatures that state DIFFERENT return types for one name: a floor\n\
         # row must state one envelope, so the name is excluded outright.\n",
    );
    let _ = writeln!(s, "[exclusions.alternates_disagree]");
    for (name, types) in alternates_disagree {
        let items: Vec<String> = types.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{name:?} = [{}]", items.join(", "));
    }
    s.push('\n');

    s.push_str(
        "# Rows the pinned engine's own declaration escapes, verbatim:\n\
         # name = [functionMap envelope, engine `getReturnType()` rendering].\n\
         # The test is subsumption, not equality — an envelope is an upper bound, so\n\
         # a coarser row over a narrower engine type is admitted, and only a row the\n\
         # engine's declaration can step outside of lands here. These are exactly the\n\
         # silent-rot cases ADR-0014 warns about, caught by machinery at generation\n\
         # time (ADR-0069 §3).\n",
    );
    let _ = writeln!(s, "[exclusions.reflection_disagree]");
    for (name, pair) in reflection_disagree {
        let items: Vec<String> = pair.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{name:?} = [{}]", items.join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::canonical_envelope;

    #[test]
    fn canonical_envelope_keeps_only_single_base_shapes() {
        assert_eq!(canonical_envelope("string").as_deref(), Some("string"));
        assert_eq!(canonical_envelope("bool").as_deref(), Some("bool"));
        assert_eq!(canonical_envelope("?string").as_deref(), Some("?string"));
        assert_eq!(canonical_envelope("string|null").as_deref(), Some("?string"));
        // Not envelopes: multi-base unions, refinements, non-scalars.
        assert_eq!(canonical_envelope("string|false"), None);
        assert_eq!(canonical_envelope("non-empty-string"), None);
        assert_eq!(canonical_envelope("int<0, max>"), None);
        assert_eq!(canonical_envelope("array"), None);
        assert_eq!(canonical_envelope("void"), None);
        assert_eq!(canonical_envelope("mixed"), None);
        assert_eq!(canonical_envelope(""), None);
    }
}
