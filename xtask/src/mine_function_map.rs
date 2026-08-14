//! `mine-function-map`: build the committed declared-return mining TOML from a
//! pinned phpstan-src checkout (ADR-0069 / issues #73, #79).
//!
//! # The pipeline
//!
//! 1. PHP reads PHP: `docs/research/phpstan-mining/mine_function_map.php` is `require`d by
//!    the real engine and emits JSON, avoiding a second Rust implementation of PHPStan's own
//!    functionMap grammar and delta ladder.
//! 2. Rust lowers: each candidate return-type string goes through
//!    [`steins_contract::lower_str`] and is kept only if it flattens to an arm list the
//!    declared-contract arm lane carries (ADR-0052 §9) — scalar bases, their literals, the
//!    two scalar refinements (`int<lo, hi>`, string predicates), `null`, and the array
//!    vocabulary. Everything else is dropped at generation time, so the shipped table never
//!    holds a row the consumer would silently discard. Widened twice: #79 dropped the #73
//!    single-base-envelope requirement (`string`, `?int`), admitting `T|false` failure unions
//!    and scalar refinements; ADR-0071 admitted the array vocabulary (`array`, `list<T>`,
//!    `array<K, V>`, `array{…}`, `iterable<K, V>`) once `subsumes` could judge array pairs
//!    instead of answering `Maybe`. Objects, `mixed`/`void`/`never` and opaque strings stay
//!    dropped and counted.
//! 3. The engine countersigns: every surviving row is checked arm-wise against the real
//!    sidecar's `reflect(name)` at the pin (PHP 8.5.8) via
//!    [`steins_contract::normalize::subsumes`], total in both directions — every row arm
//!    subsumed by some engine arm (never invented), and every engine arm subsuming some row
//!    arm (may sharpen, never drop: `string` vs `?string` loses a null, `int` vs `int|false`
//!    loses the failure arm; both excluded and listed verbatim). A name unknown to the engine
//!    is excluded; a function with no declared return type is not a disagreement — that's
//!    where the map adds reach.
//!
//! ADR-0069 §3: rot answered by machinery, not diligence.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-function-map [/path/to/phpstan-src]
//! ```
//!
//! Default checkout: `~/repo/php/phpstan-src`, read-only. Its `HEAD` becomes
//! the mining pin recorded in the emitted TOML.
//!
//! Output: `docs/research/phpstan-mining/declared_returns.toml` (source of
//! record). `cargo xtask gen-catalog` turns that into the shipped Rust table.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use steins_contract::ContractTy;
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

    // Stage 2 — lowerability: `floor_row` is the whole filter (see its doc and
    // [`Dropped`] for the drop reasons).
    let mut candidates: BTreeMap<String, Row> = BTreeMap::new();
    let mut dropped = Dropped::default();
    for (name, ty) in &mined.rows {
        match floor_row(ty) {
            Some(row) => {
                candidates.insert(name.clone(), row);
            }
            None => dropped.charge(ty),
        }
    }
    let rich = candidates.values().filter(|r| !r.envelope).count();
    let source_spelled = candidates.values().filter(|r| r.source_spelled).count();
    println!(
        "mine-function-map: {} carriable by the arm lane ({} of them richer than an envelope, \
         {} spelled from source because `spell_arms` declined); \
         {} dropped ({} shaped arrays/lists, {} multi-base unions, {} scalar refinements, \
         {} object/resource, {} void/never/mixed, {} unparseable)",
        candidates.len(),
        rich,
        source_spelled,
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
    let mut admitted_rich = 0usize;
    let mut disagree: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut typeless = 0usize;
    for (name, row) in &candidates {
        let Some(refl) = sidecar.reflect(name) else {
            return Err(format!("sidecar `reflect({name})` failed — refusing to mine a partial table"));
        };
        if !refl.function_exists {
            missing.push(name.clone());
            continue;
        }
        let mut admit = |row: &Row| {
            if !row.envelope {
                admitted_rich += 1;
            }
            admitted.insert(name.clone(), row.canon.clone());
        };
        match refl.return_type.as_deref() {
            // The engine declares nothing: the map adds reach, not a contradiction.
            None => {
                typeless += 1;
                admit(row);
            }
            Some(engine_ty) if countersigned(&row.arms, engine_ty) => admit(row),
            Some(engine_ty) => {
                disagree.insert(name.clone(), vec![row.canon.clone(), engine_ty.to_owned()]);
            }
        }
    }

    println!(
        "mine-function-map: {} admitted ({} richer than an envelope, {} where the engine declares no return type), {} reflection disagreements, {} names the engine does not know",
        admitted.len(),
        admitted_rich,
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
        admitted_rich,
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
    let dst = repo_root().join("docs/research/phpstan-mining/declared_returns.toml");
    std::fs::write(&dst, &toml).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("mine-function-map: wrote {}", dst.display());
    println!("mine-function-map: now run `cargo xtask gen-catalog`");
    Ok(())
}

/// An admitted candidate row: its canonical spelling (what the TOML stores and the consumer
/// re-lowers) and the arms that spelling denotes.
struct Row {
    /// The canonical phpdoc spelling, produced by `spell_arms` and verified to
    /// re-lower to [`Self::arms`].
    canon: String,
    /// The flattened arm list the declared-contract lane would carry.
    arms: Vec<ContractTy>,
    /// Whether this row is a #73-shaped **envelope** — a bare scalar base or its `?T` nullable
    /// pair. The complement is the #79 population, counted separately.
    envelope: bool,
    /// Whether [`Self::canon`] is the raw source spelling rather than `spell_arms`' canonical
    /// one, because the speller declined the arms — always true for a class arm. Such a row
    /// still countersigns and lowers correctly (the source string lowers by construction);
    /// only the dump surface's rendering differs.
    source_spelled: bool,
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
    admitted_rich: usize,
    engine_typeless: usize,
}

/// Rows dropped for lowering to something the declared-contract arm lane cannot
/// carry, split by reason (ADR-0069 §5), classified on the LOWERED TOP-LEVEL
/// shape — unchanged from the #73 slice so every run compares directly. The
/// refinement, void and unparseable buckets must read identically across
/// every run; that invariance is the check that classification hasn't drifted.
#[derive(Default)]
struct Dropped {
    /// `array{…}`, `list<T>`, `array<K, V>`, `iterable<T>` — the shaped-array rows. Emptied by
    /// ADR-0071 (which gave the countersign a denotation for them); kept so a later pin's
    /// regression is legible.
    arrays: usize,
    /// Multi-base unions that are not the `?T` nullable pair — `string|false`, `int|string`,
    /// the whole `T|false` failure-arm family.
    unions: usize,
    /// Scalar types richer than a base: `non-empty-string`, `int<0, 255>`, `positive-int`,
    /// literal types, the opaque string family.
    refinements: usize,
    /// Everything lowering to `Opaque` or `CallableTy` — and, before the object slice,
    /// `Class`/`ObjectAny` too. Also holds `void` and the `resource` family (both lower to
    /// `Opaque`, not to [`Self::voidish`]). At the ADR-0071 pin the 620 rows were 146
    /// class/`object`, 322 `void`, 149 `resource`, 2 `Closure`, 1 `int-mask<…>`; the object
    /// slice carried off the 146, leaving 474. Left uncorrected: these counts are the
    /// cross-run comparison series ADR-0069's table is built on, and moving a row between
    /// buckets would make the columns incomparable.
    objects: usize,
    /// `void`, `never`, `mixed`, and the `mixed`-minus-a-cut spellings.
    voidish: usize,
    /// A type string the phpdoc grammar does not accept at all (an empty return type, a
    /// PHPStan-internal spelling such as `__benevolent<…>`).
    unparseable: usize,
}

impl Dropped {
    fn total(&self) -> usize {
        self.arrays + self.unions + self.refinements + self.objects + self.voidish + self.unparseable
    }

    /// Charge one dropped row to its reason bucket, judged on the LOWERED type so the
    /// classification is the grammar's, not a substring guess.
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
            // `resource` stays in this bucket (ADR-0069 §5) so the comparison series doesn't
            // shift; it's excluded by the countersign, not the lowering — a genuine resource
            // producer has no declared return type.
            | ContractTy::Resource
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

/// The checkout's `HEAD` — the mining pin recorded in the TOML and the generated file.
/// Read-only: `git rev-parse`, nothing else.
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

/// Flatten a lowered contract into a top-level arm list, dissolving nested unions — the
/// generator's copy of `steins-infer`'s `flatten_arms`, which is the shape the consuming floor
/// rung will hand to the declared-contract lane.
fn flatten_arms(cty: ContractTy) -> Vec<ContractTy> {
    match cty {
        ContractTy::Union(members) => members.into_iter().flat_map(flatten_arms).collect(),
        other => vec![other],
    }
}

/// Whether one arm is carriable by the declared-contract lane the floor seeds into (ADR-0052
/// §9): scalar bases, their literals, the two scalar refinements, `null`, the array
/// vocabulary (`array`, `list<T>`, `array<K, V>`, `iterable<K, V>`, `array{…}`, ADR-0071),
/// and the class vocabulary (a named `ContractTy::Class` or bare `object`, added by the
/// object slice). Checked per arm, so `?ClassName` is carriable by composition.
///
/// Array admission tracks `subsumes` gaining a structural denotation for the vocabulary at
/// ADR-0071 (`array ⊇ array{dirname: string}` is `Yes`; `?array ⊉ array` is a proven `No`).
/// Class admission needed no new rule: `subsumes_class` is reflexive, so a row naming the
/// engine's own class name countersigns (clause 2 of [`countersigned`]); a differing name
/// stays `Maybe` and is refused — this is what catches the stale pre-8.0 rows (functionMap
/// says `resource` where PHP 8 returns `GdImage`/`CurlHandle`).
///
/// Still out, still counted (ADR-0069 §5 as amended by #79): `callable`, intersections,
/// `resource` (`KNOWN_UNENFORCED` keywords lowering to `Opaque`, not a class arm),
/// `mixed`/`never`/the `mixed`-minus cuts, `StrOpaque` (no faithful spelling), and
/// `self`/`static`/`parent` (lower to `Opaque` as keywords).
fn arm_is_carriable(ty: &ContractTy) -> bool {
    matches!(
        ty,
        ContractTy::Base(_)
            | ContractTy::Null
            | ContractTy::LitBool(_)
            | ContractTy::LitInt(_)
            | ContractTy::LitFloat(_)
            | ContractTy::LitStr(_)
            | ContractTy::IntIn(_)
            | ContractTy::StrWith(_)
            | ContractTy::ArrayAny { .. }
            | ContractTy::ListOf { .. }
            | ContractTy::MapOf { .. }
            | ContractTy::IterableOf { .. }
            | ContractTy::Shape { .. }
            | ContractTy::Class(_)
            | ContractTy::ObjectAny
    )
}

/// Whether an arm list is the #73-shaped **envelope** — a bare scalar base, or that base
/// paired with `null`. Used only for counting: the envelope rows are the #73 population, and
/// the complement is what issue #79 added.
fn is_envelope(arms: &[ContractTy]) -> bool {
    let bases = arms.iter().filter(|a| matches!(a, ContractTy::Base(_))).count();
    let nulls = arms.iter().filter(|a| matches!(a, ContractTy::Null)).count();
    bases == 1 && bases + nulls == arms.len()
}

/// The floor row a declared type string contributes, or `None` when the arm lane cannot carry
/// it.
///
/// The stored spelling is `spell_arms` over the lowered arms — canonical, so two spellings of
/// one type compare equal — and verified to round-trip (re-lowering it must yield an arm-equal
/// list). When it doesn't, the raw source string is stored instead, since it lowers correctly
/// by construction.
fn floor_row(ty: &str) -> Option<Row> {
    let arms = flatten_arms(steins_contract::lower_str(ty)?);
    if arms.is_empty() || !arms.iter().all(arm_is_carriable) {
        return None;
    }
    let spelled = steins_contract::spell::spell_arms(&arms).filter(|s| round_trips(s, &arms));
    let source_spelled = spelled.is_none();
    let canon = spelled.unwrap_or_else(|| ty.to_owned());
    let envelope = is_envelope(&arms);
    Some(Row { canon, arms, envelope, source_spelled })
}

/// Whether re-lowering `spelled` yields the same arm **multiset** as `arms`.
///
/// Order-insensitive on purpose: `?string` and `string|null` lower to the same two arms in
/// different orders, and the speller states one of them. What must not differ is the
/// denotation, and that is what an arm-for-arm pairing checks.
fn round_trips(spelled: &str, arms: &[ContractTy]) -> bool {
    let Some(mut back) = steins_contract::lower_str(spelled).map(flatten_arms) else {
        return false;
    };
    if back.len() != arms.len() {
        return false;
    }
    for arm in arms {
        match back.iter().position(|b| steins_contract::normalize::arm_eq(b, arm)) {
            Some(i) => {
                back.remove(i);
            }
            None => return false,
        }
    }
    true
}

/// Implements the module doc's stage 3 (ADR-0069 §3, widened by #79). A row is admitted when
/// either clause holds:
///
/// 1. **Bounds the engine** (`engine ⊆ row`, the #73 rule): a true upper bound, possibly
///    coarse (`bool` over `true` says less but nothing false).
/// 2. **Refines the engine, arm-wise** (the #79 addition): every row arm lands under some
///    engine arm, and every engine arm covers some row arm — sharpening an arm is fine,
///    dropping one isn't. `non-empty-string` under `string` passes; `string` under `?string`
///    doesn't (else "refines" would readmit the #73 catch: a hidden null or failure arm).
///
/// Everything else, including an engine type that fails to lower, is a disagreement, listed
/// verbatim.
fn countersigned(row: &[ContractTy], engine_ty: &str) -> bool {
    let Some(engine_ty) = steins_contract::lower_str(engine_ty) else {
        return false;
    };
    let engine = flatten_arms(engine_ty.clone());
    if engine.is_empty() {
        return false;
    }
    // (1) rebuild the row as one type so a union is judged as a union.
    let row_ty = match row {
        [only] => only.clone(),
        many => ContractTy::Union(many.to_vec()),
    };
    if steins_contract::normalize::subsumes(&row_ty, &engine_ty).is_yes() {
        return true;
    }
    // (2) arm-wise, totally in both directions.
    let covers = |e: &ContractTy, r: &ContractTy| steins_contract::normalize::subsumes(e, r).is_yes();
    row.iter().all(|r| engine.iter().any(|e| covers(e, r)))
        && engine.iter().all(|e| row.iter().any(|r| covers(e, r)))
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
        "# Builtin DECLARED RETURN TYPES — the ADR-0069 Asserted floor's data.\n\
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
         # not_lowerable        rows the declared-contract arm lane cannot carry, by\n\
         #                      reason (below), classified on the LOWERED TOP-LEVEL shape\n\
         # reflection_disagree  rows the arm-wise countersign refuses\n\
         # reflection_missing   names the pinned engine does not know as functions\n\
         # engine_typeless      admitted rows where the engine declares NO return type\n\
         #                      (the rows where the map genuinely adds reach)\n\
         # admitted             rows emitted into the shipped table\n\
         # admitted_rich        of those, the rows RICHER than a single-base envelope —\n\
         #                      the `T|false` failure unions and the scalar refinements\n\
         #                      issue #79 admitted, the array-vocabulary rows\n\
         #                      ADR-0071 admitted, and the class rows the object slice\n\
         #                      admitted (the #73 slice counted and dropped every one\n\
         #                      of them)\n\
         #\n\
         # WHAT IS STILL DEFERRED (ADR-0069 §5 as amended 2026-08-01): `methods_skipped`\n\
         # and the void / unparseable buckets, plus what is LEFT in the object one —\n\
         # `callable`, the intersections, and `resource`. Those have no extensional\n\
         # denotation the countersign could use: a reflexive floor says nothing about a\n\
         # signature, and `resource` is a KNOWN_UNENFORCED keyword lowering to an opaque\n\
         # arm rather than to a class. A row entering uncountersigned is the one thing\n\
         # ADR-0069 §3 refuses, so they stay out. Nothing here is lost data; it is\n\
         # deferred data, counted so the deferral stays visible.\n\
         #\n\
         # The ARRAY bucket is emptied by ADR-0071: `subsumes` gained a structural\n\
         # denotation for `array` / `list<T>` / `array<K, V>` / `array{…}`, so the\n\
         # countersign is a real question for a shaped row rather than a vacuous\n\
         # `Maybe`. The OBJECT bucket then loses its object half with no new rule at\n\
         # all: `subsumes_class` is reflexive, and a row naming the class the engine\n\
         # names countersigns on that alone. A row naming a DIFFERENT class stays\n\
         # `Maybe` and is refused — which is exactly how the stale pre-8.0 rows are\n\
         # kept out, since the floor only ever admits a name the engine itself spelled.\n\
         # The union and refinement buckets hold only their RESIDUE — a union with a\n\
         # `resource`, `callable` or `mixed` arm, a string whose only spelling is the\n\
         # opaque form. Note that `not_lowerable_object_or_resource` also holds every\n\
         # `void` row (`void` lowers to an opaque arm, not to a value type), so it is a\n\
         # coarser bucket than its name suggests; the refinement, void and unparseable\n\
         # buckets read exactly as they did at #73.\n",
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
    let _ = writeln!(s, "admitted = {}", counts.admitted);
    let _ = writeln!(s, "admitted_rich = {}\n", counts.admitted_rich);

    s.push_str(
        "# The admitted rows: lowercased builtin name -> canonical phpdoc spelling.\n\
         # The consumer re-lowers this string through the SAME `lower_str` ->\n\
         # `flatten_arms` seam a PROJECT function's declared return takes (issue #60),\n\
         # and seeds the resulting arms Asserted — one lowering, two provenances\n\
         # (ADR-0069 §2). The spelling is `spell_arms` over the lowered arms and is\n\
         # verified at generation time to re-lower to the arms that were countersigned.\n\
         # Where `spell_arms` declines the arms outright — it has no faithful spelling\n\
         # for a class arm — the row keeps functionMap's OWN string, which lowers back\n\
         # to the countersigned arms by construction and, unlike a canonical respelling,\n\
         # preserves the class's source casing (`ContractTy::Class` case-folds and could\n\
         # not restate it). That is why a few rows read `__benevolent<...>`: it is\n\
         # PHPStan's spelling of a plain union, and the parser expands it to one.\n",
    );
    let _ = writeln!(s, "[declared]");
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
        // The highest boundary governs: the map states the pin's signature.
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
         # row must state one type, so the name is excluded outright.\n",
    );
    let _ = writeln!(s, "[exclusions.alternates_disagree]");
    for (name, types) in alternates_disagree {
        let items: Vec<String> = types.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{name:?} = [{}]", items.join(", "));
    }
    s.push('\n');

    s.push_str(
        "# Rows the arm-wise countersign refuses, verbatim:\n\
         # name = [functionMap row, engine `getReturnType()` rendering].\n\
         # The test is arm-wise subsumption in BOTH directions. A row may REFINE every\n\
         # arm the engine declares (`non-empty-string` under `string` stands, and that\n\
         # is the reach this table exists for); it may not INVENT an arm the engine\n\
         # excludes, and it may not DROP one the engine declares — a `string` over the\n\
         # engine's `?string` hides a null, an `int` over `int|false` hides the failure\n\
         # arm. These are exactly the silent-rot cases ADR-0014 warns about, caught by\n\
         # machinery at generation time (ADR-0069 §3).\n",
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
    use super::{countersigned, floor_row};

    fn canon(ty: &str) -> Option<String> {
        floor_row(ty).map(|r| r.canon)
    }

    #[test]
    fn the_arm_lane_carries_scalars_unions_and_refinements() {
        assert_eq!(canon("string").as_deref(), Some("string"));
        assert_eq!(canon("bool").as_deref(), Some("bool"));
        assert_eq!(canon("?string").as_deref(), Some("string|null"));
        assert_eq!(canon("string|null").as_deref(), Some("string|null"));
        assert_eq!(canon("string|false").as_deref(), Some("string|false"));
        assert_eq!(canon("int|false").as_deref(), Some("int|false"));
        assert_eq!(canon("non-empty-string").as_deref(), Some("non-empty-string"));
        // The speller states PHPStan's own interval spelling, not the phpdoc
        // keyword sugar (issue #90).
        assert_eq!(canon("int<0, max>").as_deref(), Some("int<0, max>"));
        assert_eq!(canon("non-negative-int").as_deref(), Some("int<0, max>"));
        assert_eq!(canon("array").as_deref(), Some("array"));
        assert_eq!(canon("array{a: int}").as_deref(), Some("array{a: int}"));
        assert_eq!(canon("list<string>").as_deref(), Some("list<string>"));
        assert_eq!(canon("array<string, int>").as_deref(), Some("array<string, int>"));
        // Canonical order follows the speller: array members after scalar ones
        // (ADR-0062 §6, D4), so this is `false|array`, not the source order.
        assert_eq!(canon("array|false").as_deref(), Some("false|array"));
        assert_eq!(canon("GdFont").as_deref(), Some("GdFont"));
        assert_eq!(canon("?GdFont").as_deref(), Some("?GdFont"));
        assert_eq!(canon("object").as_deref(), Some("object"));
        assert_eq!(canon("GdImage|false").as_deref(), Some("GdImage|false"));
        assert_eq!(canon("resource"), None);
        assert_eq!(canon("open-resource"), None);
        assert_eq!(canon("closed-resource"), None);
        assert_eq!(canon("resource|false"), None);
        assert_eq!(canon("array|resource"), None);
        assert_eq!(canon("callable"), None);
        // `lower_identifier` case-folds before consulting the table, so the
        // `Closure` keyword wins over any class spelling of the same name.
        assert_eq!(canon("Closure"), None);
        assert_eq!(canon("Countable&Traversable"), None);
        assert_eq!(canon("static"), None);
        assert_eq!(canon("self"), None);
        assert_eq!(canon("void"), None);
        assert_eq!(canon("mixed"), None);
        assert_eq!(canon(""), None);
    }

    #[test]
    fn the_countersign_decides_class_rows_by_reflexivity_alone() {
        let arms = |ty: &str| floor_row(ty).expect("carriable").arms;
        assert!(countersigned(&arms("GdFont"), "GdFont"), "imageloadfont's shape");
        assert!(countersigned(&arms("GdFont"), "\\GdFont"), "the leading `\\` is normalized away");
        assert!(countersigned(&arms("gdfont"), "GdFont"), "class names are case-folded");
        assert!(countersigned(&arms("?GdFont"), "?GdFont"));
        assert!(countersigned(&arms("object"), "object"));
        assert!(countersigned(&arms("object"), "GdFont"));
        assert!(countersigned(&arms("GdImage"), "object"));
        assert!(!countersigned(&arms("GdFont"), "GdImage"));
        // Genuinely hierarchy-dependent questions are refused both ways
        // (ADR-0071 §2.3's deferral) — a real is-a oracle would decide these.
        assert!(!countersigned(&arms("ArrayObject"), "Traversable"), "a subclass row");
        assert!(!countersigned(&arms("Traversable"), "ArrayObject"), "a superclass row");
        assert!(floor_row("resource").is_none(), "the resource rows stay uncarriable");
        assert!(
            !countersigned(&[steins_contract::ContractTy::Opaque], "GdImage"),
            "curl_init's era: functionMap says `resource`, PHP 8 returns a CurlHandle"
        );
        assert!(!countersigned(&arms("GdFont"), "?GdFont"), "a class row may not hide a null");
        assert!(countersigned(&arms("?GdFont"), "GdFont"), "but it may bound one, clause (1)");
        assert!(!countersigned(&arms("GdFont|GdImage"), "?GdFont"));
    }

    #[test]
    fn every_admitted_spelling_round_trips() {
        for ty in ["string", "?int", "string|false", "non-empty-string", "int<0, 255>", "int|string|null"] {
            let row = floor_row(ty).expect("carriable");
            let back = floor_row(&row.canon).expect("the canonical spelling must re-lower");
            assert_eq!(back.canon, row.canon, "{ty} does not round-trip through {}", row.canon);
        }
    }

    #[test]
    fn the_countersign_admits_refinements_and_refuses_dropped_arms() {
        let arms = |ty: &str| floor_row(ty).expect("carriable").arms;
        assert!(countersigned(&arms("non-empty-string"), "string"));
        assert!(countersigned(&arms("string|false"), "string|false"));
        assert!(countersigned(&arms("false"), "bool"));
        assert!(countersigned(&arms("int<0, 255>"), "int"));
        assert!(countersigned(&arms("bool"), "true"));
        assert!(countersigned(&arms("string|null"), "string"));
        assert!(!countersigned(&arms("string"), "?string"), "xml_error_string's shape");
        assert!(!countersigned(&arms("int"), "int|false"), "intlcal_get's shape");
        assert!(!countersigned(&arms("bool"), "int|bool"), "ldap_compare's shape");
        assert!(!countersigned(&arms("string"), "array|string|bool"), "pg_last_notice's shape");
        // An excluded arm is not by itself a refusal if the row still bounds
        // (clause 1) — it must ALSO fail to bound, as below.
        assert!(countersigned(&arms("int|false"), "int"));
        assert!(!countersigned(&arms("int<-1, 1>|false"), "int"), "substr_compare's shape");
        assert!(!countersigned(&arms("int"), "string"), "pg_port's shape");
        assert!(!countersigned(&arms("int"), "bool"), "imageinterlace's shape");
        assert!(!countersigned(&arms("string"), "void"), "sodium_add's shape");
    }

    #[test]
    fn the_countersign_decides_array_rows_rather_than_shrugging_at_them() {
        let arms = |ty: &str| floor_row(ty).expect("carriable").arms;
        // The mining workhorse (ADR-0071 §2.1): the entire 388-row bucket in
        // one assertion.
        assert!(countersigned(&arms("array{dirname: string, basename: string}"), "array"));
        assert!(countersigned(&arms("list<string>"), "array"), "str_split's shape");
        assert!(countersigned(&arms("array<string, int>"), "array"));
        assert!(countersigned(&arms("non-empty-array"), "array"));
        assert!(countersigned(&arms("array"), "array"));
        assert!(countersigned(&arms("array{a: int}|null"), "?array"));
        assert!(!countersigned(&arms("array"), "?array"), "ftp_raw's shape");
        assert!(!countersigned(&arms("array{a: int}"), "?array"));
        assert!(!countersigned(&arms("null|array"), "array|false|null"), "mysqli_fetch_row's shape");
        assert!(!countersigned(&arms("array{a: int}"), "string"));
        assert!(!countersigned(&arms("list<string>"), "int|false"));
        assert!(!countersigned(&arms("string"), "array|string|bool"), "pg_last_notice's shape");
    }
}
