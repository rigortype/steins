//! `mine-function-map`: build the committed declared-return mining TOML from a
//! pinned phpstan-src checkout (ADR-0069 / issues #73, #79).
//!
//! # The pipeline, and why it has three stages
//!
//! 1. **PHP reads PHP.** `docs/research/phpstan-mining/mine_function_map.php` is
//!    `require`d by the real engine and emits JSON — the functionMap key grammar
//!    (alternate-signature ticks, `::` method rows) and the forward-applied delta
//!    ladder are PHPStan's own, and transcribing either into Rust would be a
//!    second implementation to keep in sync.
//! 2. **Rust lowers.** Every candidate return-type string is lowered through
//!    [`steins_contract::lower_str`] and kept only when the lowering flattens to an
//!    **arm list the declared-contract machinery carries**: the scalar bases, their
//!    literals, the two scalar refinements (`int<lo, hi>`, the string predicates)
//!    and `null` — exactly the vocabulary `steins-infer`'s declared-return arm
//!    lane already seeds for a project function (ADR-0052 §9), and exactly what
//!    `spell_arms` can render back. A row the arm lane could not carry is dropped
//!    here, at generation time, so the shipped table has no rows the seam would
//!    silently discard.
//!
//!    Issue #79 widened this filter. The #73 slice kept only rows that lowered to
//!    a single-base **envelope** (`string`, `?int`); the `T|false` failure unions
//!    and the scalar refinements — the rows where functionMap genuinely exceeds
//!    reflection — were counted and dropped, awaiting the contracts-grade slice
//!    this is. Arrays, objects, `mixed`/`void`/`never` and the opaque-string form
//!    stay dropped, still counted: the arm lane has no faithful seeding for them.
//! 3. **The engine countersigns.** Every surviving row is put to the *real*
//!    sidecar's `reflect(name)` at the pin (PHP 8.5.8) and judged **arm-wise**
//!    through [`steins_contract::normalize::subsumes`] — the same acceptance
//!    relation `admit_return_fact` uses to admit a curated refinement against a
//!    reflected envelope. The correspondence must be total in both directions:
//!
//!    * every **row** arm is subsumed by some engine arm — the row refines the
//!      engine's declaration and never invents an arm outside it
//!      (`non-empty-string` under the engine's `string` is agreement; `int` under
//!      the engine's `string` is not);
//!    * every **engine** arm subsumes some row arm — the row may sharpen an arm
//!      but may not *drop* one. This is the #73 catch, kept: `string` against the
//!      engine's `?string` silently loses a null, `int` against the engine's
//!      `int|false` silently loses the failure arm, and both are excluded and
//!      listed verbatim.
//!
//!    A name the engine does not know as a function is excluded too. A function
//!    the engine knows but for which it declares **no** return type is not a
//!    disagreement — those rows are precisely where the map adds reach over
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
//! Output: `docs/research/phpstan-mining/declared_returns.toml` (the source of
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

    // Stage 2 — lowerability. `floor_row` is the whole filter: a row whose lowering
    // does not flatten to an arm list the declared-contract lane carries is dropped.
    //
    // The drop is counted BY REASON, and the reasons are classified on the LOWERED
    // TOP-LEVEL shape exactly as the #73 slice classified them, so the two runs'
    // buckets are directly comparable. Post-#79 the union and refinement buckets
    // hold only their *residue* — a union with an array/object/mixed arm, a string
    // whose only spelling is the opaque form — while the array, object, void and
    // unparseable buckets are untouched by the relaxation and must read identically.
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
    println!(
        "mine-function-map: {} carriable by the arm lane ({} of them richer than an envelope); \
         {} dropped ({} shaped arrays/lists, {} multi-base unions, {} scalar refinements, \
         {} object/resource, {} void/never/mixed, {} unparseable)",
        candidates.len(),
        rich,
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
            // The engine declares nothing: the map is adding reach, not contradicting.
            None => {
                typeless += 1;
                admit(row);
            }
            // The arm-wise countersign (module docs, stage 3): the row may refine
            // every arm the engine declares, and may drop none of them.
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

/// An admitted candidate row: its canonical spelling (what the TOML stores and the
/// consumer re-lowers) and the arms that spelling denotes.
struct Row {
    /// The canonical phpdoc spelling, produced by `spell_arms` and verified to
    /// re-lower to [`Self::arms`].
    canon: String,
    /// The flattened arm list the declared-contract lane would carry.
    arms: Vec<ContractTy>,
    /// Whether this row is a #73-shaped **envelope** — a bare scalar base or its
    /// `?T` nullable pair. The complement is the #79 population, counted separately
    /// so the slice's own reach stays legible in the header.
    envelope: bool,
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
/// carry, split by reason (ADR-0069 §5), classified on the LOWERED TOP-LEVEL shape.
///
/// The classification is deliberately unchanged from the #73 slice so the two runs
/// compare directly: the array, object, void and unparseable buckets are untouched
/// by the #79 relaxation and must read identically, while the union and refinement
/// buckets shrink to their residue — the unions carrying an arm from one of the
/// other buckets, and the strings whose only spelling is the opaque form.
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

/// Flatten a lowered contract into a top-level arm list, dissolving nested unions —
/// the generator's copy of `steins-infer`'s `flatten_arms`, which is the shape the
/// consuming floor rung will hand to the declared-contract lane.
fn flatten_arms(cty: ContractTy) -> Vec<ContractTy> {
    match cty {
        ContractTy::Union(members) => members.into_iter().flat_map(flatten_arms).collect(),
        other => vec![other],
    }
}

/// Whether one arm is carriable by the declared-contract lane the floor seeds into
/// (ADR-0052 §9): the scalar bases, their literals, the two scalar refinements and
/// `null`. This is exactly the vocabulary `spell_arms` renders and `subsumes`
/// decides extensionally, so an admitted row can always be spelled back and always
/// countersigned against the engine.
///
/// Everything else stays out, still counted (ADR-0069 §5 as amended by #79):
/// the array vocabulary (the arm lane carries it, but a *builtin* row would need
/// the shape lane's `to_shape_fact` seeding and a shape-aware countersign — a
/// slice of its own), classes/`object`/`callable`/`resource` and intersections
/// (no scalar denotation, so `subsumes` can only answer `Maybe` and the countersign
/// would be vacuous), `mixed`/`never`/the `mixed`-minus cuts (nothing to say), and
/// `StrOpaque` (no faithful spelling — `spell_arms` refuses it).
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
    )
}

/// Whether an arm list is the #73-shaped **envelope** — a bare scalar base, or that
/// base paired with `null`. Used only for counting: the envelope rows are the #73
/// population, and the complement is what issue #79 added.
fn is_envelope(arms: &[ContractTy]) -> bool {
    let bases = arms.iter().filter(|a| matches!(a, ContractTy::Base(_))).count();
    let nulls = arms.iter().filter(|a| matches!(a, ContractTy::Null)).count();
    bases == 1 && bases + nulls == arms.len()
}

/// The floor row a declared type string contributes, or `None` when the arm lane
/// cannot carry it.
///
/// The stored spelling is *canonical* — `spell_arms` over the lowered arms — so the
/// shipped table is normalized and two spellings of one type compare equal. It is
/// verified to round-trip (re-lowering the canonical spelling must yield an
/// arm-equal list); a spelling that does not round-trip would be a row the consumer
/// re-lowers differently from what was countersigned, so the raw source string —
/// which lowers correctly by construction — is stored instead.
fn floor_row(ty: &str) -> Option<Row> {
    let arms = flatten_arms(steins_contract::lower_str(ty)?);
    if arms.is_empty() || !arms.iter().all(arm_is_carriable) {
        return None;
    }
    let canon = steins_contract::spell::spell_arms(&arms)
        .filter(|spelled| round_trips(spelled, &arms))
        .unwrap_or_else(|| ty.to_owned());
    let envelope = is_envelope(&arms);
    Some(Row { canon, arms, envelope })
}

/// Whether re-lowering `spelled` yields the same arm **multiset** as `arms`.
///
/// Order-insensitive on purpose: `?string` and `string|null` lower to the same two
/// arms in different orders, and the speller states one of them. What must not
/// differ is the denotation, and that is what an arm-for-arm pairing checks.
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

/// The generation-time engine countersign (ADR-0069 §3, widened by issue #79):
/// whether the candidate row is consistent with the pinned engine's own declaration
/// in one of the two shapes an Asserted floor row may take.
///
/// The relation throughout is [`steins_contract::normalize::subsumes`], the single
/// acceptance relation the checker enforces. A row is admitted when either holds:
///
/// 1. **The row BOUNDS the engine** (`engine ⊆ row`) — the #73 rule, kept verbatim.
///    The row is a true upper bound on everything the engine's declaration admits,
///    possibly a coarse one: `bool` over the engine's `true` says less than the
///    engine does, but nothing it says is false.
/// 2. **The row REFINES the engine, arm-wise** — the #79 addition, and the same
///    subset discipline `admit_return_fact` applies to a curated refinement against
///    a reflected envelope. The arm correspondence must be total in **both**
///    directions: every row arm lands under some engine arm (the row never invents
///    an arm outside the declaration), and every engine arm covers some row arm
///    (the row may sharpen an arm but may not **drop** one). `non-empty-string`
///    under `string` passes; `string` under `?string` does not.
///
/// Everything else is a disagreement, listed verbatim. The second clause of (2) is
/// what keeps the #73 catch list intact through the relaxation: without it, "the row
/// refines" would readmit exactly the rows the pinned engine disowns — `string`
/// hiding the null in `?string`, `int` hiding the failure arm in `int|false`.
///
/// An engine type that does not lower at all is an answer this cannot judge, so it
/// counts as disagreement — the refusing side.
fn countersigned(row: &[ContractTy], engine_ty: &str) -> bool {
    let Some(engine_ty) = steins_contract::lower_str(engine_ty) else {
        return false;
    };
    let engine = flatten_arms(engine_ty.clone());
    if engine.is_empty() {
        return false;
    }
    // (1) The row bounds the engine: rebuild the row as one type and ask directly,
    // so a union row is judged as a union rather than arm by arm.
    let row_ty = match row {
        [only] => only.clone(),
        many => ContractTy::Union(many.to_vec()),
    };
    if steins_contract::normalize::subsumes(&row_ty, &engine_ty).is_yes() {
        return true;
    }
    // (2) The row refines the engine, arm-wise and totally in both directions.
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
         #                      that issue #79 admitted (the #73 slice counted and\n\
         #                      dropped them)\n\
         #\n\
         # WHAT IS STILL DEFERRED (ADR-0069 §5 as amended 2026-08-01): `methods_skipped`\n\
         # and the array / object / void / unparseable buckets. The array vocabulary\n\
         # lowers fine — `lower_str` spells `array{…}` and `list<T>` — but seeding it\n\
         # needs the SHAPE lane (`to_shape_fact`) and a shape-aware countersign, which\n\
         # is a slice of its own. Object, `callable` and `resource` arms have no scalar\n\
         # denotation, so `subsumes` can only answer `Maybe` and the countersign would\n\
         # be vacuous. Nothing here is lost data; it is deferred data, counted so the\n\
         # deferral stays visible.\n\
         #\n\
         # The union and refinement buckets now hold only their RESIDUE: a union with an\n\
         # array/object/mixed arm, a string whose only spelling is the opaque form. The\n\
         # other four buckets are untouched by the #79 relaxation and read exactly as\n\
         # they did at #73.\n",
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
         # verified at generation time to re-lower to the arms that were countersigned.\n",
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
        // The #73 population, unchanged and still canonicalized.
        assert_eq!(canon("string").as_deref(), Some("string"));
        assert_eq!(canon("bool").as_deref(), Some("bool"));
        assert_eq!(canon("?string").as_deref(), Some("string|null"));
        assert_eq!(canon("string|null").as_deref(), Some("string|null"));
        // What issue #79 added: the `T|false` failure family and the scalar
        // refinements — the two buckets #73 counted and dropped.
        assert_eq!(canon("string|false").as_deref(), Some("string|false"));
        assert_eq!(canon("int|false").as_deref(), Some("int|false"));
        assert_eq!(canon("non-empty-string").as_deref(), Some("non-empty-string"));
        assert_eq!(canon("int<0, max>").as_deref(), Some("non-negative-int"));
        // Still out, still counted: no faithful arm-lane seeding exists for them.
        assert_eq!(canon("array"), None);
        assert_eq!(canon("array{a: int}"), None);
        assert_eq!(canon("array|false"), None);
        assert_eq!(canon("resource"), None);
        assert_eq!(canon("void"), None);
        assert_eq!(canon("mixed"), None);
        assert_eq!(canon(""), None);
    }

    #[test]
    fn every_admitted_spelling_round_trips() {
        // The stored spelling must re-lower to the arms that were countersigned, or
        // the consumer reads a different type than the engine signed off on.
        for ty in ["string", "?int", "string|false", "non-empty-string", "int<0, 255>", "int|string|null"] {
            let row = floor_row(ty).expect("carriable");
            let back = floor_row(&row.canon).expect("the canonical spelling must re-lower");
            assert_eq!(back.canon, row.canon, "{ty} does not round-trip through {}", row.canon);
        }
    }

    #[test]
    fn the_countersign_admits_refinements_and_refuses_dropped_arms() {
        let arms = |ty: &str| floor_row(ty).expect("carriable").arms;
        // The reach case: the row refines what the engine declares.
        assert!(countersigned(&arms("non-empty-string"), "string"));
        assert!(countersigned(&arms("string|false"), "string|false"));
        assert!(countersigned(&arms("false"), "bool"));
        assert!(countersigned(&arms("int<0, 255>"), "int"));
        // The #73 clause, kept: a row that BOUNDS the engine stands even when it is
        // coarser than the engine's own declaration.
        assert!(countersigned(&arms("bool"), "true"));
        assert!(countersigned(&arms("string|null"), "string"));
        // A DROPPED arm is the #73 catch, preserved: the engine can return a null or
        // a `false` the row does not state.
        assert!(!countersigned(&arms("string"), "?string"), "xml_error_string's shape");
        assert!(!countersigned(&arms("int"), "int|false"), "intlcal_get's shape");
        assert!(!countersigned(&arms("bool"), "int|bool"), "ldap_compare's shape");
        assert!(!countersigned(&arms("string"), "array|string|bool"), "pg_last_notice's shape");
        // An arm the engine excludes is not by itself a refusal: `int|false` over
        // the engine's `int` is a coarse upper bound, and clause (1) admits it for
        // the same reason `bool` over `true` stands. What it must not do is ALSO
        // fail to bound — see the `substr_compare` shape below.
        assert!(countersigned(&arms("int|false"), "int"));
        assert!(!countersigned(&arms("int<-1, 1>|false"), "int"), "substr_compare's shape");
        assert!(!countersigned(&arms("int"), "string"), "pg_port's shape");
        assert!(!countersigned(&arms("int"), "bool"), "imageinterlace's shape");
        // An engine type this cannot lower is an answer it cannot judge — refuse.
        assert!(!countersigned(&arms("string"), "void"), "sodium_add's shape");
    }
}
