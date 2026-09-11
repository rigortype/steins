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
//! 3. The engines countersign: every surviving row is checked arm-wise against the real
//!    sidecar's `reflect(name)` via [`steins_contract::normalize::subsumes`], total in both
//!    directions — every row arm subsumed by some engine arm (never invented), and every
//!    engine arm subsuming some row arm (may sharpen, never drop: `string` vs `?string` loses
//!    a null, `int` vs `int|false` loses the failure arm; both excluded and listed verbatim).
//!    A name unknown to the engine is excluded; a function with no declared return type is
//!    not a disagreement — that's where the map adds reach.
//!
//! # Several engines, and what each of them may do (issue #714)
//!
//! `--php PATH` is repeatable, the way `mine-constants`' is, and the engines answer in
//! ascending minor order. The division of labour is deliberately asymmetric, because the
//! two halves of "countersign" are different claims:
//!
//! * **The TOP engine decides a row's fate**, exactly as the single-engine run did. Which
//!   bucket a refusal is charged to is the cross-run comparison series ADR-0069 §5's table
//!   is built on, and letting a lower minor reclassify a row would make the columns
//!   incomparable between pins.
//! * **Every LOWER engine is a veto** ([`veto`]). A row the top engine admits and a
//!   supported minor CONTRADICTS is a row that is false on that minor, so it is refused and
//!   listed with the version that objected. An engine that lacks the name, or declares no
//!   return type, vetoes nothing — that is absence, not disagreement, and it is the same
//!   clause `mine-constants` applies for the same reason (ADR-0094 §2).
//!
//! `[meta] crosscheck_php` is the top engine and `crosscheck_diffed` is the whole set, so a
//! row's provenance names every PHP that vouched for it.
//!
//! ADR-0069 §3: rot answered by machinery, not diligence.
//!
//! # Two populations, one pipeline
//!
//! functionMap's keys split on `::`: plain functions, and `Class::method` rows.
//! The method half was skipped outright while the floor was function-keyed —
//! ADR-0069 §5's last deferral, 6,658 keys at the pin — and issue #673 lifts it
//! now that ADR-0093 §3.1 lets a declaration-sourced object arm into the contract
//! lane. Both halves run the same three stages; only stage 3's question differs
//! (`reflect_class(Class)` rather than `reflect(name)`), and [`mine_methods`] says
//! how. There is no property half: functionMap's key grammar has no spelling for
//! a property.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-function-map [/path/to/phpstan-src] [--functions] [--methods] [--php PATH]…
//! ```
//!
//! Default checkout: `~/repo/php/phpstan-src`, read-only. Its `HEAD` becomes
//! the mining pin recorded in the emitted TOMLs. With neither flag both halves
//! are written; see [`Halves`] for why either may be written alone. `--php PATH`
//! (repeatable) names the countersigning engines, low minor last; with none given
//! the run asks the `php` on `PATH` alone and nothing is vetoed.
//!
//! Output: `docs/research/phpstan-mining/declared_returns.toml` and
//! `declared_method_returns.toml` (sources of record). `cargo xtask gen-catalog`
//! turns those into the shipped Rust tables.

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
    /// `class::method` -> declared return type, the issue #673 population.
    method_rows: BTreeMap<String, String>,
    method_alternates_disagree: BTreeMap<String, Vec<String>>,
    method_version_sensitive: BTreeMap<String, Vec<String>>,
}

/// Which half (or halves) of the pipeline a run writes.
///
/// One pipeline, one pin, two committed TOMLs — and a run may write either one
/// alone. The reason is the cross-check engine rather than convenience: the two
/// tables record the PHP the sidecar happened to be when they were mined, so a
/// methods-only slice mined on a later patch release would otherwise rewrite the
/// function table's `crosscheck_php` and its counts for a reason that has nothing
/// to do with the slice. Regenerating alongside a `PINNED_PHP` bump writes both.
#[derive(Clone, Copy)]
pub struct Halves {
    pub functions: bool,
    pub methods: bool,
}

/// One countersigning engine: its own version string and a live sidecar on it.
struct Engine {
    version: String,
    minor: (u16, u16),
    sidecar: Sidecar,
}

/// Spawn one sidecar per `--php PATH`, ascending by minor so the TOP engine is
/// last. With no path given the run asks the `php` on `PATH` alone, which is the
/// single-engine run this command has always been.
fn engines(php_bins: &[String]) -> Result<Vec<Engine>, String> {
    let bins: Vec<String> =
        if php_bins.is_empty() { vec!["php".to_owned()] } else { php_bins.to_vec() };
    let mut out = Vec::new();
    for bin in bins {
        let mut sidecar =
            Sidecar::spawn_with(&bin).map_err(|e| format!("spawn php sidecar on {bin}: {e}"))?;
        let version = engine_version(&mut sidecar)?;
        let minor = php_minor(&version)?;
        out.push(Engine { version, minor, sidecar });
    }
    out.sort_by_key(|e| e.minor);
    Ok(out)
}

/// `"8.4.25"` -> `(8, 4)`. A version this cannot read is a failed run: the minor is
/// what orders the engines, and guessing it would pick the wrong top one.
fn php_minor(version: &str) -> Result<(u16, u16), String> {
    let mut parts = version.split('.');
    let maj = parts.next().and_then(|p| p.parse().ok());
    let min = parts.next().and_then(|p| p.parse().ok());
    match (maj, min) {
        (Some(maj), Some(min)) => Ok((maj, min)),
        _ => Err(format!("engine reported PHP version `{version}`, which has no major.minor")),
    }
}

/// **The lower engines' veto over an admitted FUNCTION row**: the version that
/// contradicted it and what it declared, or `None` when none of them does.
///
/// Only a CONTRADICTION vetoes. An engine that does not have the name is a build
/// or a minor without it, and an engine that declares no return type is the very
/// silence this table exists to fill — neither is a counter-example to the row.
fn veto(
    lower: &mut [Engine],
    name: &str,
    arms: &[ContractTy],
) -> Result<Option<(String, String)>, String> {
    for e in lower.iter_mut() {
        let Some(refl) = e.sidecar.reflect(name) else {
            return Err(format!(
                "sidecar `reflect({name})` failed on PHP {} — refusing to mine a partial table",
                e.version
            ));
        };
        if !refl.function_exists {
            continue;
        }
        let Some(engine_ty) = refl.return_type.as_deref() else { continue };
        if !countersigned(arms, engine_ty) {
            return Ok(Some((e.version.clone(), engine_ty.to_owned())));
        }
    }
    Ok(None)
}

/// Entry point for `cargo xtask mine-function-map`.
pub fn run(checkout: Option<&str>, halves: Halves, php_bins: &[String]) -> Result<(), String> {
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
        "mine-function-map: {} keys, {} `Class::method` keys, {} alternate-disagreement names, {} plain-function rows, {} method rows",
        mined.total_keys,
        mined.methods_skipped,
        mined.alternates_disagree.len(),
        mined.rows.len(),
        mined.method_rows.len(),
    );
    if !mined.malformed.is_empty() {
        return Err(format!("{} malformed signature rows: {:?}", mined.malformed.len(), mined.malformed));
    }
    let mut engines = engines(php_bins)?;
    let versions: Vec<String> = engines.iter().map(|e| e.version.clone()).collect();
    let engine_version =
        versions.last().ok_or("no PHP engine to countersign with")?.clone();
    if !halves.functions {
        return mine_methods(&mined, &pin, &versions, &mut engines);
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
         {} spelled from source because the row names a class or `spell_arms` declined); \
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

    // Stage 3 — the engines countersign: the TOP one decides, the lower ones veto.
    println!(
        "mine-function-map: cross-checking {} rows against PHP {}",
        candidates.len(),
        versions.join(", ")
    );

    let mut admitted: BTreeMap<String, String> = BTreeMap::new();
    let mut disagree: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut typeless = 0usize;
    let (lower, top) = engines.split_at_mut(versions.len() - 1);
    let top = &mut top[0];
    for (name, row) in &candidates {
        let Some(refl) = top.sidecar.reflect(name) else {
            return Err(format!("sidecar `reflect({name})` failed — refusing to mine a partial table"));
        };
        if !refl.function_exists {
            missing.push(name.clone());
            continue;
        }
        match refl.return_type.as_deref() {
            // The engine declares nothing: the map adds reach, not a contradiction.
            None => {
                typeless += 1;
                admitted.insert(name.clone(), row.canon.clone());
            }
            Some(engine_ty) if countersigned(&row.arms, engine_ty) => {
                admitted.insert(name.clone(), row.canon.clone());
            }
            Some(engine_ty) => {
                disagree.insert(name.clone(), vec![row.canon.clone(), engine_ty.to_owned()]);
            }
        }
    }
    // The veto pass, over the rows the top engine let through. Recorded in the same
    // `[exclusions.reflection_disagree]` table, with the version that objected, so a
    // reviewer sees WHICH minor the row was false on.
    let mut vetoed = 0usize;
    for name in admitted.keys().cloned().collect::<Vec<_>>() {
        let row = &candidates[&name];
        if let Some((version, engine_ty)) = veto(lower, &name, &row.arms)? {
            admitted.remove(&name);
            disagree.insert(
                name.clone(),
                vec![row.canon.clone(), format!("{engine_ty} (PHP {version})")],
            );
            vetoed += 1;
        }
    }
    let admitted_rich = admitted.keys().filter(|n| !candidates[*n].envelope).count();
    if vetoed > 0 {
        println!("mine-function-map: {vetoed} rows vetoed by a lower minor");
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
        &versions,
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
    if halves.methods {
        mine_methods(&mined, &pin, &versions, &mut engines)?;
    }
    println!("mine-function-map: now run `cargo xtask gen-catalog`");
    Ok(())
}

/// The pinned engine's own version string — recorded in each TOML's `[meta]` as
/// the countersigning authority, so a row's provenance names the PHP that vouched
/// for it and not merely the phpstan-src commit that proposed it.
fn engine_version(sidecar: &mut Sidecar) -> Result<String, String> {
    sidecar
        .env()
        .map(|e| e.php_version)
        .ok_or_else(|| "sidecar `env` failed — cannot record the cross-check engine".to_owned())
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
    /// one — because the speller declined the arms, or because the row names a class and only
    /// the source string remembers its casing ([`arm_names_a_class`]). Such a row still
    /// countersigns and lowers correctly (the source string lowers by construction); only the
    /// dump surface's rendering differs.
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
            // No phpdoc spelling lowers to an enum-case arm (issue #429 seeds it
            // from a native declaration alone), so a mined row never reaches
            // here; it counts with the objects for the day one does.
            | ContractTy::EnumCase { .. }
            | ContractTy::ObjectAny
            | ContractTy::CallableTy { .. }
            // `resource` stays in this bucket (ADR-0069 §5) so the comparison series doesn't
            // shift; it's excluded by the countersign, not the lowering — a genuine resource
            // producer has no declared return type.
            | ContractTy::Resource
            | ContractTy::Opaque => &mut self.objects,
            // `unset` counts with the value-less spellings; a mined stub return
            // type never carries it (ADR-0087), so this arm keeps the census
            // total exact without shifting any series.
            ContractTy::Mixed
            | ContractTy::MixedMinus(_)
            | ContractTy::Never
            | ContractTy::Unset => &mut self.voidish,
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

/// Does this arm carry a class NAME whose source casing only the raw functionMap
/// string preserves? Top-level and nested alike (`list<GdFont>`), since a nested
/// class arm respells through the same case-folded [`ContractTy::Class`].
fn arm_names_a_class(ty: &ContractTy) -> bool {
    match ty {
        ContractTy::Class(_) | ContractTy::EnumCase { .. } => true,
        ContractTy::ListOf { elem, .. } => arm_names_a_class(elem),
        ContractTy::MapOf { key, val, .. } | ContractTy::IterableOf { key, val } => {
            arm_names_a_class(key) || arm_names_a_class(val)
        }
        ContractTy::Shape { fields, unsealed, .. } => {
            fields.iter().any(|f| arm_names_a_class(&f.ty))
                || unsealed.as_ref().is_some_and(|(k, v)| {
                    k.as_ref().is_some_and(|k| arm_names_a_class(k)) || arm_names_a_class(v)
                })
        }
        ContractTy::Union(members) | ContractTy::Inter(members) => {
            members.iter().any(arm_names_a_class)
        }
        _ => false,
    }
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
    // ADR-0093 §3 taught `spell_arms` to spell a class arm, and a mined row is the
    // one caller that must NOT take that spelling: `ContractTy::Class` case-folds,
    // so a canonical respelling would write `gdfont` into the shipped table and
    // lose php-src's own `GdFont`. The source string lowers to the countersigned
    // arms by construction, so keeping it costs nothing and preserves the casing
    // (`Cx::class_display_fqn` recovers it on the dump surface either way, but
    // only for a name the builtin display table knows).
    let spelled = (!arms.iter().any(arm_names_a_class))
        .then(|| steins_contract::spell::spell_arms(&arms))
        .flatten()
        .filter(|s| round_trips(s, &arms));
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
    versions: &[String],
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
    // Every engine the run asked, low minor first. `crosscheck_php` above is the
    // TOP one, which decides each row's bucket; the rest are vetoes, and a row a
    // lower minor contradicted is in `[exclusions.reflection_disagree]` with the
    // version that objected. One entry = nothing was vetoed.
    let _ = writeln!(s, "crosscheck_diffed = [");
    for v in versions {
        let _ = writeln!(s, "  {v:?},");
    }
    let _ = writeln!(s, "]");
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
         # Where a row NAMES A CLASS, or `spell_arms` declines the arms outright, the\n\
         # row keeps functionMap's OWN string, which lowers back to the countersigned\n\
         # arms by construction and, unlike a canonical respelling, preserves the\n\
         # class's source casing (`ContractTy::Class` case-folds and could not restate\n\
         # it — ADR-0093 §3 gave the speller a class spelling, not a case memory).\n\
         # That is why a few rows read `__benevolent<...>`: it is\n\
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

// ---------------------------------------------------------------------------
// The `Class::method` half (issue #673).
// ---------------------------------------------------------------------------

/// One admitted method row: its spelling and whether the engine declares the
/// method `static`.
///
/// The static bit is the engine's, never functionMap's — the map's key grammar
/// spells `Class::method` for an instance method and a static one alike, so the
/// only witness to the difference is reflection. The consuming lookup reads it to
/// keep `PDO::connect` and `Closure::bind` apart from the instance rows beside them.
struct MethodRow {
    canon: String,
    is_static: bool,
    envelope: bool,
}

/// The counts the method table's provenance header carries.
struct MethodCounts {
    keys: usize,
    alternates_disagree: usize,
    dropped: Dropped,
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

/// Whether the engine's own rendering of a return type is the one native answer
/// that binds nothing: `mixed`.
///
/// The function half can afford to admit over `mixed`, because ADR-0056's
/// reflected envelope is a rung ABOVE the floor at analysis time and corrects it
/// per name. The method half has no such rung, so a row admitted over `mixed`
/// would be the last word — and `mixed` subsumes everything, so "refines the
/// engine" degenerates into "was not checked at all". `DirectoryIterator::key`
/// is the witness: functionMap says `string`, `Iterator::key(): mixed` says
/// nothing, and PHP returns `int(0)`.
fn engine_says_mixed(engine_ty: &str) -> bool {
    engine_ty.trim().trim_start_matches('\\').eq_ignore_ascii_case("mixed")
}

/// The transitive builtin ancestors of `class`, lowercased and NEAREST FIRST,
/// excluding `class` itself — the same breadth-first
/// [`steins_catalog::builtin_class_supers`] closure the consuming walk takes, run
/// here so the miner can see which row a refused key would otherwise inherit.
fn builtin_ancestors(class: &str) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut frontier = vec![class.to_ascii_lowercase()];
    let mut out = Vec::new();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for name in &frontier {
            for sup in steins_catalog::builtin_class_supers(name).unwrap_or_default() {
                let sup = sup.to_ascii_lowercase();
                if seen.insert(sup.clone()) {
                    out.push(sup.clone());
                    next.push(sup);
                }
            }
        }
        frontier = next;
    }
    out
}

/// Mine the `Class::method` rows into `declared_method_returns.toml`, the method
/// twin of the function table above (issue #673, ADR-0069 §3's machinery applied
/// unchanged one key-grammar over).
///
/// Five things differ from the function half, and only five:
///
/// 1. **The countersign asks a class, not a name.** `reflect_class(Class)` reports
///    every method the engine resolves on that class, INHERITED ONES INCLUDED, so a
///    functionMap row keyed on a subclass is checked against the declaration the
///    runtime would actually reach. One request per class, memoized.
/// 2. **The engine also decides `static`.** functionMap's `Class::method` key spells
///    an instance method and a static one identically; [`MethodRow::is_static`] is
///    reflection's word.
/// 3. **Two absence buckets, not one.** A class the engine does not have at all
///    (an unloaded extension) is charged per class *and* per row; a class it has
///    without the method is its own bucket, and the two say different things about
///    the map — the first is this build's extension set, the second is drift.
/// 4. **No countersign, no row — including the silent countersigns.** The function
///    half admits a row the engine declares nothing for, and admits one the engine
///    declares `mixed` for (everything "refines" `mixed`). Both are safe THERE
///    because ADR-0056's reflected envelope sits above the function floor at
///    analysis time and corrects it per name. Here there is no rung above, and a
///    row admitted without a native envelope is not merely unchecked at the key it
///    is keyed on: the consuming walk hands it to every descendant on a covariance
///    argument that only a native envelope can make (ADR-0049 A16 bounds an
///    override by what the PARENT natively declares, and an untyped or `mixed`
///    parent declares no bound at all). So both are refused, into
///    [`MethodCounts::engine_untyped`] and [`MethodCounts::engine_mixed`], and
///    listed by name. `PDOException::getCode` — `Exception::getCode` is `final`
///    and untyped, PHP returns `"HY000"` — is what the old rule got wrong.
/// 5. **A refused child SHADOWS an admitted ancestor.** The consuming walk reads
///    "no row on the child" as "inherit the ancestor's", but 5,573 of the 6,606
///    reduced keys never became rows. A key functionMap states and this miner
///    dropped or refused says the nearest declaration is NOT the ancestor's — so
///    every such key whose ancestor DOES carry a row for the same method is
///    emitted into the `[blocked]` table, and the walk stops there rather than
///    climbing past it.
///
/// Everything else is the function half verbatim: the same [`floor_row`] carriability
/// filter, the same arm-wise [`countersigned`] relation in both directions, the same
/// A11-shaped change oracle, the same Asserted grade.
///
/// PROPERTIES are absent by construction, not by filter: functionMap's key grammar
/// has no spelling for a property, so the vendor/builtin property reads issue #673
/// counts alongside the method rows have no source here at all. Nor could they be
/// countersigned if they did — `ReflectedProperty` carries a name, a static bit and a
/// visibility, and no type.
fn mine_methods(
    mined: &Mined,
    pin: &str,
    versions: &[String],
    engines: &mut [Engine],
) -> Result<(), String> {
    let engine_version = versions.last().ok_or("no PHP engine to countersign with")?.clone();
    let (lower, top) = engines.split_at_mut(versions.len() - 1);
    let sidecar = &mut top[0].sidecar;
    // Stage 2 — lowerability, the same filter and the same buckets. Every key that
    // does NOT survive to a row is also remembered by name, with why: difference 5
    // reads that list back once the admitted set is known.
    let mut candidates: BTreeMap<String, Row> = BTreeMap::new();
    let mut dropped = Dropped::default();
    let mut refused: BTreeMap<String, &'static str> = BTreeMap::new();
    for key in mined.method_alternates_disagree.keys() {
        refused.insert(key.clone(), "alternates_disagree");
    }
    for (key, ty) in &mined.method_rows {
        match floor_row(ty) {
            Some(row) => {
                candidates.insert(key.clone(), row);
            }
            None => {
                dropped.charge(ty);
                refused.insert(key.clone(), "not_lowerable");
            }
        }
    }
    println!(
        "mine-function-map: {} method rows carriable by the arm lane; {} dropped \
         ({} shaped arrays/lists, {} multi-base unions, {} scalar refinements, \
         {} object/resource, {} void/never/mixed, {} unparseable)",
        candidates.len(),
        dropped.total(),
        dropped.arrays,
        dropped.unions,
        dropped.refinements,
        dropped.objects,
        dropped.voidish,
        dropped.unparseable,
    );

    // Stage 3 — the engine countersigns, one `reflect_class` per class.
    let mut classes: BTreeMap<String, Option<steins_sidecar::ReflectedClass>> = BTreeMap::new();
    let mut admitted: BTreeMap<String, MethodRow> = BTreeMap::new();
    let mut disagree: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut class_missing: Vec<String> = Vec::new();
    let mut class_missing_rows = 0usize;
    let mut method_missing: Vec<String> = Vec::new();
    let mut engine_untyped: Vec<String> = Vec::new();
    let mut engine_mixed: Vec<String> = Vec::new();
    for (key, row) in &candidates {
        let Some((class, method)) = key.split_once("::") else {
            return Err(format!("method key `{key}` has no `::`"));
        };
        if !classes.contains_key(class) {
            let refl = sidecar.reflect_class(class).ok_or_else(|| {
                format!("sidecar `reflect_class({class})` failed — refusing to mine a partial table")
            })?;
            if refl.declaration.is_none() {
                class_missing.push(class.to_owned());
            }
            classes.insert(class.to_owned(), refl.declaration);
        }
        let Some(decl) = classes.get(class).and_then(Option::as_ref) else {
            class_missing_rows += 1;
            refused.insert(key.clone(), "class_missing");
            continue;
        };
        let Some(m) = decl.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) else {
            method_missing.push(key.clone());
            refused.insert(key.clone(), "method_missing");
            continue;
        };
        let mut admit = || {
            admitted.insert(
                key.clone(),
                MethodRow {
                    canon: row.canon.clone(),
                    is_static: m.is_static,
                    envelope: row.envelope,
                },
            );
        };
        match m.return_type.as_deref() {
            // No countersign, no row (ADR-0069 §3, and difference 4 above). The
            // function half admits both of these and is corrected from above by
            // ADR-0056; nothing sits above this table, and the hierarchy walk would
            // hand an unbound row to every descendant.
            None => {
                engine_untyped.push(key.clone());
                refused.insert(key.clone(), "engine_untyped");
            }
            Some(engine_ty) if engine_says_mixed(engine_ty) => {
                engine_mixed.push(key.clone());
                refused.insert(key.clone(), "engine_mixed");
            }
            Some(engine_ty) if countersigned(&row.arms, engine_ty) => admit(),
            Some(engine_ty) => {
                disagree.insert(key.clone(), vec![row.canon.clone(), engine_ty.to_owned()]);
                refused.insert(key.clone(), "reflection_disagree");
            }
        }
    }

    // The veto pass (issue #714), over the keys the top engine let through. A lower
    // minor that has the class, has the method, and DECLARES a contradicting return
    // type is a counter-example; a minor that lacks either, or declares nothing, is
    // an absence and vetoes nothing.
    let mut vetoed = 0usize;
    for key in admitted.keys().cloned().collect::<Vec<_>>() {
        let Some((class, method)) = key.split_once("::") else { continue };
        let arms = &candidates[&key].arms;
        for e in lower.iter_mut() {
            let refl = e.sidecar.reflect_class(class).ok_or_else(|| {
                format!(
                    "sidecar `reflect_class({class})` failed on PHP {} — refusing to mine a \
                     partial table",
                    e.version
                )
            })?;
            let Some(decl) = refl.declaration else { continue };
            let Some(m) = decl.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) else {
                continue;
            };
            let Some(engine_ty) = m.return_type.as_deref() else { continue };
            if engine_says_mixed(engine_ty) || countersigned(arms, engine_ty) {
                continue;
            }
            admitted.remove(&key);
            disagree.insert(
                key.clone(),
                vec![
                    candidates[&key].canon.clone(),
                    format!("{engine_ty} (PHP {})", e.version),
                ],
            );
            refused.insert(key.clone(), "reflection_disagree");
            vetoed += 1;
            break;
        }
    }
    if vetoed > 0 {
        println!("mine-function-map: {vetoed} method rows vetoed by a lower minor");
    }

    // Difference 5 — the shadow set. A key functionMap states and this miner did
    // not admit is a statement that the child's own declaration differs from
    // whatever an ancestor declares; the walk must not climb past it. Only the keys
    // whose ancestor actually carries a row for the same method matter, which is
    // what keeps the table small enough to read.
    let mut blocked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, why) in &refused {
        let Some((class, method)) = key.split_once("::") else { continue };
        let Some(ancestor) = builtin_ancestors(class)
            .into_iter()
            .find(|a| admitted.contains_key(&format!("{a}::{method}")))
        else {
            continue;
        };
        blocked.insert(key.clone(), vec![(*why).to_owned(), ancestor]);
    }

    let admitted_static = admitted.values().filter(|r| r.is_static).count();
    let admitted_rich = admitted.values().filter(|r| !r.envelope).count();
    println!(
        "mine-function-map: {} method rows admitted ({} static, {} richer than an envelope), \
         {} disagreements, {} the engine declares no return type for, {} it declares `mixed` for, \
         {} rows on {} classes the engine does not have, {} rows the class does not declare; \
         {} refused keys shadow an ancestor's row",
        admitted.len(),
        admitted_static,
        admitted_rich,
        disagree.len(),
        engine_untyped.len(),
        engine_mixed.len(),
        class_missing_rows,
        class_missing.len(),
        method_missing.len(),
        blocked.len(),
    );

    let counts = MethodCounts {
        keys: mined.methods_skipped,
        alternates_disagree: mined.method_alternates_disagree.len(),
        dropped,
        class_missing_rows,
        class_missing_classes: class_missing.len(),
        method_missing: method_missing.len(),
        reflection_disagree: disagree.len(),
        engine_untyped: engine_untyped.len(),
        engine_mixed: engine_mixed.len(),
        blocked: blocked.len(),
        admitted: admitted.len(),
        admitted_static,
        admitted_rich,
    };
    let toml = render_methods(
        pin,
        &engine_version,
        versions,
        &counts,
        &admitted,
        &mined.method_version_sensitive,
        &mined.method_alternates_disagree,
        &disagree,
        &class_missing,
        &method_missing,
        &engine_untyped,
        &engine_mixed,
        &blocked,
    );
    let dst = repo_root().join("docs/research/phpstan-mining/declared_method_returns.toml");
    std::fs::write(&dst, &toml).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("mine-function-map: wrote {}", dst.display());
    Ok(())
}

/// Render the committed method-mining TOML.
#[allow(clippy::too_many_arguments)]
fn render_methods(
    pin: &str,
    engine_version: &str,
    versions: &[String],
    counts: &MethodCounts,
    admitted: &BTreeMap<String, MethodRow>,
    version_sensitive: &BTreeMap<String, Vec<String>>,
    alternates_disagree: &BTreeMap<String, Vec<String>>,
    reflection_disagree: &BTreeMap<String, Vec<String>>,
    class_missing: &[String],
    method_missing: &[String],
    engine_untyped: &[String],
    engine_mixed: &[String],
    blocked: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut s = String::new();
    s.push_str(
        "# Builtin DECLARED METHOD RETURN TYPES — the ADR-0069 Asserted floor's data,\n\
         # keyed `class::method` (issue #673).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-function-map --methods`,\n\
         # which runs `mine_function_map.php` against a pinned phpstan-src checkout and\n\
         # then makes the real PHP sidecar countersign every surviving row through\n\
         # `reflect_class`. Regenerate alongside a `PINNED_PHP` bump, never by hand.\n\
         #\n\
         # LINEAGE (see the root NOTICE file):\n\
         #   Steins <- phpstan-src `resources/functionMap.php`\n\
         #              (MIT, Copyright (c) Ondrej Mirtes and contributors)\n\
         #          <- Phan `src/Phan/Language/Internal/FunctionSignatureMap.php`\n\
         #              (MIT, Copyright (c) 2015 Rasmus Lerdorf,\n\
         #                   Copyright (c) 2015 Andrew Morrison)\n\
         #\n\
         # GRADE: every row here is Asserted, never Verified (ADR-0069 §2), and the\n\
         # object-returning rows ride ADR-0093 §3.1's sourcing rule — a mined declared\n\
         # return IS a declaration, which is what lets a class arm into the contract\n\
         # lane at all. functionMap is not a native stub, so no row is ever Verified:\n\
         # a builtin has no `@return` docblock to promote from, and the map's own word\n\
         # is exactly the unconfirmed claim the Asserted lane exists for.\n\
         #\n\
         # WHERE IT SPEAKS: at a method or static call whose receiver is a BUILTIN\n\
         # class by declaration, after the project chain has answered nothing. A\n\
         # project class that extends a builtin keeps its own declaration on every\n\
         # name it declares; this table answers only the inherited names.\n\
         #\n\
         # NO COUNTERSIGN, NO ROW — and unlike the function half, that includes the\n\
         # two SILENT countersigns. A row the engine declares no return type for, and\n\
         # a row it declares `mixed` for, are both refused here and listed under\n\
         # `[exclusions]`. The function table admits them because ADR-0056's reflected\n\
         # envelope is a rung ABOVE it at analysis time and corrects it per name;\n\
         # nothing sits above this table, and the consuming walk hands a row to every\n\
         # DESCENDANT on a covariance argument (ADR-0049 A16) that only a native,\n\
         # non-`mixed` envelope can make.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "phpstan_src_commit = {pin:?}");
    let _ = writeln!(s, "crosscheck_php = {engine_version:?}");
    // Every engine the run asked, low minor first. `crosscheck_php` above is the
    // TOP one, which decides each row's bucket; the rest are vetoes, and a row a
    // lower minor contradicted is in `[exclusions.reflection_disagree]` with the
    // version that objected. One entry = nothing was vetoed.
    let _ = writeln!(s, "crosscheck_diffed = [");
    for v in versions {
        let _ = writeln!(s, "  {v:?},");
    }
    let _ = writeln!(s, "]");
    let _ = writeln!(
        s,
        "miner = \"docs/research/phpstan-mining/mine_function_map.php\"\n\
         generator = \"cargo xtask mine-function-map --methods\"\n"
    );

    s.push_str(
        "# keys                 `Class::method` entries at the pin, after the delta ladder\n\
         # alternates_disagree  keys whose alternate signatures state different returns\n\
         # not_lowerable        rows the declared-contract arm lane cannot carry, by\n\
         #                      reason (below), classified on the LOWERED TOP-LEVEL shape\n\
         # class_missing_*      rows whose CLASS the pinned engine does not have (an\n\
         #                      extension this build does not load), and how many distinct\n\
         #                      classes those rows name\n\
         # method_missing       rows whose class the engine has WITHOUT the method — the\n\
         #                      drift bucket, distinct from the extension-set one above\n\
         # reflection_disagree  rows the arm-wise countersign refuses\n\
         # engine_untyped       rows REFUSED because the engine declares no return type\n\
         # engine_mixed         rows REFUSED because the engine declares `mixed`\n\
         # blocked              refused keys whose ancestor carries a row for the same\n\
         #                      method — the walk stops at them instead of inheriting\n\
         # admitted             rows emitted into the shipped table\n\
         # admitted_static      of those, the ones the engine declares `static`\n\
         # admitted_rich        of those, the rows RICHER than a single-base envelope\n\
         #\n\
         # PROPERTIES ARE NOT HERE, and their absence is the source's, not a filter's:\n\
         # functionMap's key grammar has no spelling for a property, so the vendor and\n\
         # builtin property reads issue #673 counts alongside the method rows have no\n\
         # mining source at all. A property table would need a different source and a\n\
         # different countersign — `ReflectedProperty` carries a name, a static bit and\n\
         # a visibility, and no type.\n\
         #\n\
         # WHAT IS EXCLUDED, and why it is the same list the function half excludes:\n\
         # `callable`, the intersections, `resource` and `void` have no extensional\n\
         # denotation `subsumes` could use, so the countersign could only answer\n\
         # `Maybe` — which ADR-0069 §3 refuses. Class and bare-`object` arms are NOT\n\
         # excluded: `subsumes_class` is reflexive, so a row naming the class the\n\
         # engine names countersigns on that alone, and a row naming a DIFFERENT one\n\
         # stays `Maybe` and is refused. That reflexive floor is what keeps the stale\n\
         # rows out while admitting `DOMDocument::getElementById` = `?DOMElement`.\n",
    );
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "keys = {}", counts.keys);
    let _ = writeln!(s, "alternates_disagree = {}", counts.alternates_disagree);
    let _ = writeln!(s, "not_lowerable = {}", counts.dropped.total());
    let _ = writeln!(s, "not_lowerable_shaped_arrays = {}", counts.dropped.arrays);
    let _ = writeln!(s, "not_lowerable_multi_base_unions = {}", counts.dropped.unions);
    let _ = writeln!(s, "not_lowerable_scalar_refinements = {}", counts.dropped.refinements);
    let _ = writeln!(s, "not_lowerable_object_or_resource = {}", counts.dropped.objects);
    let _ = writeln!(s, "not_lowerable_void_never_mixed = {}", counts.dropped.voidish);
    let _ = writeln!(s, "not_lowerable_unparseable = {}", counts.dropped.unparseable);
    let _ = writeln!(s, "class_missing_rows = {}", counts.class_missing_rows);
    let _ = writeln!(s, "class_missing_classes = {}", counts.class_missing_classes);
    let _ = writeln!(s, "method_missing = {}", counts.method_missing);
    let _ = writeln!(s, "reflection_disagree = {}", counts.reflection_disagree);
    let _ = writeln!(s, "engine_untyped = {}", counts.engine_untyped);
    let _ = writeln!(s, "engine_mixed = {}", counts.engine_mixed);
    let _ = writeln!(s, "blocked = {}", counts.blocked);
    let _ = writeln!(s, "admitted = {}", counts.admitted);
    let _ = writeln!(s, "admitted_static = {}", counts.admitted_static);
    let _ = writeln!(s, "admitted_rich = {}\n", counts.admitted_rich);

    s.push_str(
        "# The admitted rows: lowercased `class::method` -> [canonical phpdoc spelling,\n\
         # whether the ENGINE declares the method static]. The consumer re-lowers the\n\
         # spelling through the same `lower_str` -> `flatten_arms` seam a project\n\
         # method's declared return takes, and seeds the resulting arms Asserted. The\n\
         # key is where functionMap puts the row, which may be a SUBCLASS of the class\n\
         # that declares the method; the consuming lookup walks the builtin hierarchy\n\
         # (ADR-0043) so a row on a parent answers for a child receiver too — the\n\
         # parent's NATIVE, NON-`mixed` engine envelope is an upper bound on every\n\
         # override under covariance (ADR-0049 A16), which is why a row the engine\n\
         # could not countersign never reaches this section, and why `[blocked]` below\n\
         # stops the walk at a child functionMap states differently.\n\
         # Where a row NAMES A CLASS, or `spell_arms` declines the arms outright, it\n\
         # keeps functionMap's OWN string, which lowers back to the countersigned arms\n\
         # by construction and preserves the class's source casing.\n",
    );
    let _ = writeln!(s, "[declared]");
    for (key, row) in admitted {
        let _ = writeln!(s, "{key:?} = [{:?}, {}]", row.canon, row.is_static);
    }
    s.push('\n');

    s.push_str(
        "# The A11-shaped change oracle, keyed the same way: `class::method` entries\n\
         # whose RETURN type moves between two adjacent supported minors, keyed to the\n\
         # minor it moved AT. A project whose declared PhpTarget is not wholly at or\n\
         # above that minor declines the row; an unknown target admits.\n",
    );
    let _ = writeln!(s, "[version_sensitive]");
    for (key, minors) in version_sensitive {
        let last = minors.iter().max().cloned().unwrap_or_default();
        let all = minors.join(", ");
        let _ = writeln!(s, "{key:?} = {last:?}  # changed at: {all}");
    }
    s.push('\n');

    s.push_str(
        "# SHADOWS. The consuming walk climbs `builtin_class_supers` and reads \"no row\n\
         # on the child\" as \"inherit the ancestor's\" — but only 1 key in 7 became a\n\
         # row, so that reading is wrong wherever functionMap STATES the child and this\n\
         # miner dropped or refused it. Such a key is positive evidence that the\n\
         # nearest declaration is not the ancestor's, so it BLOCKS the walk: a receiver\n\
         # of that class, or of any class between it and the row-bearing ancestor,\n\
         # answers nothing at all. Listed only where an ancestor actually carries a row\n\
         # for the same method, which is what keeps the table short.\n\
         # key = [why the key was refused, the nearest ancestor whose row it shadows].\n\
         # `pdoexception::getcode` is the live witness: functionMap states it as `['']`,\n\
         # which is unparseable, and without this table the walk would reach\n\
         # `runtimeexception::getcode` and answer `int` where PHP returns `\"HY000\"`.\n",
    );
    let _ = writeln!(s, "[blocked]");
    for (key, why) in blocked {
        let items: Vec<String> = why.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{key:?} = [{}]", items.join(", "));
    }
    s.push('\n');

    s.push_str("# Exclusions, recorded so the refusals are auditable rather than invisible.\n");
    let _ = writeln!(s, "[exclusions]");
    let _ = writeln!(
        s,
        "# Classes the pinned engine does not have at all — this build's extension set,\n\
         # not a claim about the map. Existence is a boot-surface fact and this table\n\
         # refuses to guess at it.\n\
         class_missing = ["
    );
    for name in class_missing {
        let _ = writeln!(s, "  {name:?},");
    }
    s.push_str("]\n\n");

    let _ = writeln!(
        s,
        "# Rows whose class the engine HAS, without the method the row names. Drift, in\n\
         # the direction ADR-0014 warns about, caught by machinery.\n\
         method_missing = ["
    );
    for name in method_missing {
        let _ = writeln!(s, "  {name:?},");
    }
    s.push_str("]\n\n");

    let _ = writeln!(
        s,
        "# Rows the engine declares NO return type for. The map may well be right about\n\
         # them, but right is not countersigned, and with no rung above this table and a\n\
         # hierarchy walk below it an uncountersigned row becomes every descendant's\n\
         # answer. `exception::getcode` is `final` and untyped and returns `\"HY000\"` out\n\
         # of a PDOException; `mysqli::init` is untyped and returns `NULL`.\n\
         engine_untyped = ["
    );
    for name in engine_untyped {
        let _ = writeln!(s, "  {name:?},");
    }
    s.push_str("]\n\n");

    let _ = writeln!(
        s,
        "# Rows the engine declares `mixed` for. `mixed` subsumes everything, so the\n\
         # arm-wise countersign would pass ANY row against it — the check degenerates\n\
         # into no check. `directoryiterator::key` is the witness: the map says\n\
         # `string`, `Iterator::key(): mixed` says nothing, PHP returns `int(0)`.\n\
         engine_mixed = ["
    );
    for name in engine_mixed {
        let _ = writeln!(s, "  {name:?},");
    }
    s.push_str("]\n\n");

    s.push_str(
        "# Alternate signatures that state DIFFERENT return types for one key: a floor\n\
         # row must state one type, so the key is excluded outright.\n",
    );
    let _ = writeln!(s, "[exclusions.alternates_disagree]");
    for (key, types) in alternates_disagree {
        let items: Vec<String> = types.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{key:?} = [{}]", items.join(", "));
    }
    s.push('\n');

    s.push_str(
        "# Rows the arm-wise countersign refuses, verbatim:\n\
         # key = [functionMap row, engine `getReturnType()` rendering].\n\
         # The test is arm-wise subsumption in BOTH directions: a row may REFINE every\n\
         # arm the engine declares, never INVENT one it excludes, never DROP one it\n\
         # declares.\n",
    );
    let _ = writeln!(s, "[exclusions.reflection_disagree]");
    for (key, pair) in reflection_disagree {
        let items: Vec<String> = pair.iter().map(|t| format!("{t:?}")).collect();
        let _ = writeln!(s, "{key:?} = [{}]", items.join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{builtin_ancestors, countersigned, engine_says_mixed, floor_row};

    /// The method half's own refusal (issue #673 review): `mixed` is the engine
    /// answer that makes the countersign vacuous, so it is recognized by name
    /// rather than run through `subsumes`.
    #[test]
    fn the_mixed_engine_envelope_is_recognized_however_it_is_spelled() {
        assert!(engine_says_mixed("mixed"));
        assert!(engine_says_mixed(" mixed "));
        assert!(engine_says_mixed("\\mixed"));
        assert!(engine_says_mixed("MIXED"));
        assert!(!engine_says_mixed("string"));
        assert!(!engine_says_mixed("mixed|false"), "a union is a real envelope again");
        assert!(!engine_says_mixed(""));
        // And the reason it must be its own gate: `subsumes` says yes to
        // everything under `mixed`, so the countersign alone would admit a row
        // that contradicts the runtime.
        let arms = floor_row("string").expect("carriable").arms;
        assert!(countersigned(&arms, "mixed"), "directoryiterator::key's shape");
    }

    /// The shadow set's walk must be the consumer's walk: breadth-first over
    /// `builtin_class_supers`, nearest ancestor first, and the class itself out.
    #[test]
    fn the_ancestor_walk_is_breadth_first_and_excludes_the_class() {
        let supers = builtin_ancestors("PDOException");
        assert!(!supers.contains(&"pdoexception".to_owned()), "the class itself is not its ancestor");
        let at = |n: &str| supers.iter().position(|s| s == n);
        let (rt, ex) = (at("runtimeexception"), at("exception"));
        assert!(rt.is_some() && ex.is_some(), "the SPL chain is in the hierarchy table: {supers:?}");
        assert!(rt < ex, "nearest first: {supers:?}");
        assert!(builtin_ancestors("NoSuchBuiltinClassAnywhere").is_empty());
    }

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
