//! **What a bare global constant is worth** (ADR-0094, issue #598).
//!
//! `ArgValue::GlobalConst` is carried by the lowering and unproven by
//! construction (issue #168's ruling), so `PHP_INT_MAX`, `PHP_EOL` and
//! `JSON_THROW_ON_ERROR` all dumped `unknown` and every transfer whose deciding
//! argument is a constant matched on the NAME instead. This module is the
//! reader that gives such a name a value, in the two lanes ADR-0094 splits it
//! into:
//!
//! * **§3's classes**, answered here by rule ([`platform_fact`]). A host-dependent
//!   constant's default is the UNION of the values it can take, because a library
//!   cannot assume its deployment host; the engine version derives from the
//!   project's declared `PhpTarget`; and the integer width is 64-bit always.
//! * **§2's mined table**, answered by `steins_catalog::engine_constant` — the
//!   spec-fixed roster, whose value is the same on every host that has the
//!   constant at all. Nothing evaluates a constant at analysis time; the table is
//!   generated and committed, so the fold seam's recognizer surface is unchanged
//!   (ADR-0060/ADR-0066).
//!
//! # Strata (ADR-0094 §3.2)
//!
//! A spec-fixed literal, the 64-bit width and a default union are **Verified**:
//! each is true of every host the project can run on, so each may premise a
//! proof-layer finding. A value fixed by a `[runtime] os` pin is **Asserted** —
//! it is the user's claim about the deployment host, on the same footing as an
//! `@param` claim.
//!
//! # What is deliberately not here
//!
//! Existence. The table never says a constant is defined, and neither does this
//! module: a name with no answer is silence, not a claim that it is missing.
//! Whether reading an undefined constant is a finding is the absence family's
//! call (ADR-0094 §2, §5).

use steins_catalog::{ConstRow, ConstValue};
use steins_db::PhpTarget;
use steins_domain::{Base, Fact, IntRange, PhpStr, Refinement, StrPreds, Val};
use steins_syntax::{ArgValue, NameRef, RefKind, normalize_const_fqn};

use crate::cx::Cx;
use crate::env::{Stratum, singleton_fact};

/// The `[runtime] os` pin (ADR-0094 §3): the deployment host's `PHP_OS_FAMILY`,
/// as the project declares it.
///
/// One knob pins four constants together — `PHP_OS_FAMILY`, `PHP_EOL`,
/// `DIRECTORY_SEPARATOR` and `PATH_SEPARATOR` — because pinning one and leaving
/// the others as unions lets `if (PHP_OS_FAMILY === 'Windows')` stay alive while
/// `PHP_EOL` inside it is already `"\n"`, the self-inflicted disagreement
/// phpstan#14948 §4 describes. `None` is the default and means the union: a
/// library cannot assume its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Windows,
    Bsd,
    Darwin,
    Solaris,
    Linux,
    /// php-src's own sixth value: a host it could not classify.
    Unknown,
}

impl OsFamily {
    /// The `steins.toml` spelling (`os = "linux"`), lowercased as ADR-0094 §3
    /// writes it.
    #[must_use]
    pub fn from_config(value: &str) -> Option<Self> {
        match value {
            "windows" => Some(OsFamily::Windows),
            "bsd" => Some(OsFamily::Bsd),
            "darwin" => Some(OsFamily::Darwin),
            "solaris" => Some(OsFamily::Solaris),
            "linux" => Some(OsFamily::Linux),
            "unknown" => Some(OsFamily::Unknown),
            _ => None,
        }
    }

    /// The `PHP_OS_FAMILY` value php-src reports for this host.
    #[must_use]
    pub const fn php_os_family(self) -> &'static str {
        match self {
            OsFamily::Windows => "Windows",
            OsFamily::Bsd => "BSD",
            OsFamily::Darwin => "Darwin",
            OsFamily::Solaris => "Solaris",
            OsFamily::Linux => "Linux",
            OsFamily::Unknown => "Unknown",
        }
    }

    /// Whether this host is the one php-src spells its separators for. Every
    /// non-Windows family shares `"\n"`, `'/'` and `':'`; Windows is the only
    /// divergence, which is why the closed sets have two members and not six.
    const fn is_windows(self) -> bool {
        matches!(self, OsFamily::Windows)
    }

    /// The `doctor` line's spelling of the posture.
    #[must_use]
    pub const fn as_config_str(self) -> &'static str {
        match self {
            OsFamily::Windows => "windows",
            OsFamily::Bsd => "bsd",
            OsFamily::Darwin => "darwin",
            OsFamily::Solaris => "solaris",
            OsFamily::Linux => "linux",
            OsFamily::Unknown => "unknown",
        }
    }
}

/// The four constants a `[runtime] os` pin fixes TOGETHER (ADR-0094 §3), and so
/// the only four whose value can ever be `Asserted` — every other answer this
/// module gives is true of every host the project can run on.
///
/// Read by [`crate::walk::value_stratum`], which derives the stratum of a
/// composed expression without a `Cx` to consult. That is sound in both
/// directions: with a pin these four ARE the user's claim, and without one they
/// resolve to a union rather than a literal, so nothing downstream decides on
/// them and the stratum is never consulted for a verdict.
const OS_PINNED_CONSTANTS: &[&str] =
    &["DIRECTORY_SEPARATOR", "PATH_SEPARATOR", "PHP_EOL", "PHP_OS_FAMILY"];

/// **Every name ADR-0094 §3 answers by class** — the roster [`platform_fact`]
/// matches on, as a list rather than as a `match`'s reachability.
///
/// The difference is load bearing in one place: §3's version family DECLINES when
/// the project declares `PHP_VERSION_ID` (issue #29's discipline), and the engine
/// still holds the name. Reading "§3 gave no answer" as "the engine does not have
/// this name" is what let a polyfill's own `define('PHP_VERSION_ID', 70400)`
/// answer through §4 — walking straight around the discipline that decline exists
/// to enforce.
///
/// The same roster the miner refuses by (`xtask/src/mine_constants.rs`'s
/// `PLATFORM_RULED`), and the two are disjoint from the table by construction.
const PLATFORM_RULED: &[&str] = &[
    // Closed host sets and the open one.
    "PHP_EOL",
    "DIRECTORY_SEPARATOR",
    "PATH_SEPARATOR",
    "PHP_OS_FAMILY",
    "PHP_OS",
    // The engine version, derived from `PhpTarget`.
    "PHP_VERSION",
    "PHP_MAJOR_VERSION",
    "PHP_MINOR_VERSION",
    "PHP_RELEASE_VERSION",
    "PHP_VERSION_ID",
    "PHP_EXTRA_VERSION",
    // Integer width and float limits (§3.1).
    "PHP_INT_MAX",
    "PHP_INT_MIN",
    "PHP_INT_SIZE",
    "PHP_FLOAT_DIG",
    "PHP_FLOAT_EPSILON",
    "PHP_FLOAT_MAX",
    "PHP_FLOAT_MIN",
];

/// **Whether a constant reference denotes one of the four `[runtime] os` pins**,
/// by the name PHP resolves it to and never by the name the file spells.
///
/// `use const PHP_EOL as EOL;` writes `EOL`, and matching the raw spelling let the
/// alias launder the pin's `Asserted` stratum into `Verified` — `EOL === "\r\n"`
/// dumped `true` with no `(asserted)` marker on it. It is the aliasing hole issue
/// #279 closed for `use function trim as t;`, in the constant lane.
///
/// Read by [`crate::walk::value_stratum`], and true whether or not a pin is set.
/// That is sound in both directions: with a pin these four ARE the user's claim,
/// and without one they resolve to a union rather than a literal, so nothing
/// downstream decides on them and the stratum is never consulted for a verdict.
pub(crate) fn denotes_os_pinned_constant(cx: &Cx, r: &NameRef) -> bool {
    const_ref_candidates(cx, r).iter().any(|c| OS_PINNED_CONSTANTS.contains(&c.as_str()))
}

/// The six values php-src closes `PHP_OS_FAMILY` over, in the order
/// `Fact::OneOf` sorts them into.
const OS_FAMILIES: &[&str] = &["BSD", "Darwin", "Linux", "Solaris", "Unknown", "Windows"];

/// **The fact a global constant denotes**, or `None` when Steins states nothing
/// about it.
///
/// The order is ADR-0094's: §3's classes first, because they are the constants a
/// mined row would answer WRONGLY (with the analysis machine's host), then §2's
/// mined table. The two rosters are disjoint by construction — the miner refuses
/// every §3 name — so the order is documentation rather than a tie-break.
pub(crate) fn global_const_fact(cx: &Cx, r: &NameRef) -> Option<(Fact, Stratum)> {
    // PHP's own resolution order for a constant reference, which is what decides
    // WHICH constant the name means before anything decides what it is worth.
    for candidate in const_ref_candidates(cx, r) {
        let platform = platform_fact(cx, &candidate);
        let row = steins_catalog::engine_constant(&candidate);
        // By the NAME and not by whether §3 produced an answer: the version family
        // declines outright when the project declares `PHP_VERSION_ID` (issue
        // #29's discipline), and reading that decline as "the engine does not have
        // this name" is what let the polyfill's own literal answer.
        if !PLATFORM_RULED.contains(&candidate.as_str()) && row.is_none() {
            // No engine constant of this spelling, so §4's project declarations
            // are the only thing that can answer.
            //
            // A same-file declaration is the constant this file's reader actually
            // reaches. A project constant declared in ANOTHER file stops the walk
            // (ADR-0094 §4, which defers cross-file constants to their own slice),
            // and declining there is not caution: `namespace App;
            // PREG_UNMATCHED_AS_NULL` with `App\PREG_UNMATCHED_AS_NULL` declared
            // next door resolves to the PROJECT's constant, and falling through to
            // the global fallback would report the engine's 512 for a name PHP
            // reads as the project's own value.
            if let Some(answer) = same_file_fact(cx, &candidate) {
                return answer;
            }
            if cx.index.declares_constant(&candidate) {
                return None;
            }
            continue;
        }
        // The engine defines this name, and PHP does not let a project take it
        // (see [`engine_over_project`]). Answering the project's literal here is
        // what this check used to do, and it is a wrong answer of exactly the
        // shape ADR-0094 §2's version gate exists to avoid.
        if let Some(answer) = engine_over_project(cx, &candidate, row.as_ref()) {
            return answer;
        }
        // §3's classes before §2's table, because they are the constants a mined
        // row would answer WRONGLY (with the analysis machine's host). The two
        // rosters are disjoint by construction — the miner refuses every §3 name —
        // so the order is documentation rather than a tie-break.
        if let Some(answer) = platform {
            return Some(answer);
        }
        if let Some(row) = row {
            return target_admits(&row, cx.php_target)
                .then(|| (mined_fact(&row.value), Stratum::Verified));
        }
    }
    None
}

/// **What a project declaration of a name the ENGINE already has is worth**
/// (ADR-0094 §4), or `None` when the project declares no such name and the engine
/// answers alone.
///
/// PHP does not let a project take a name the engine holds. `define('SORT_REGULAR',
/// 99)` raises `Constant SORT_REGULAR already defined` and leaves the constant at
/// `0`; `define('PHP_EOL', 'x')` leaves it `"\n"`. So §4's same-file binding is not
/// a shadow of the table — above the row's `since` the engine wins outright and the
/// declaration the file carries is dead code. Reading it was how a project could
/// walk `PHP_VERSION_ID` past issue #29's discipline by declaring it.
///
/// Below the row's `since` the declaration is not dead: at a minor where the engine
/// has no constant of that name, the project's `define` is what the reader reaches.
/// That is only decidable when the target's WHOLE minor range lies outside the
/// row's — a range that straddles the arrival would need a union of the engine's
/// value and the project's, and a project that defines a name PHP is about to take
/// away does not earn one. It declines.
///
/// The outer `Option` says "this is the answer"; the inner one is
/// [`global_const_fact`]'s own "no answer", so a decline here stops the candidate
/// walk rather than falling through to the engine.
fn engine_over_project(
    cx: &Cx,
    candidate: &str,
    row: Option<&ConstRow>,
) -> Option<Option<(Fact, Stratum)>> {
    let same_file = same_file_fact(cx, candidate);
    if same_file.is_none() && !cx.index.declares_constant(candidate) {
        return None;
    }
    match engine_holds_the_name(row, cx.php_target) {
        // Every minor the target spans has the engine's constant: the declaration
        // is the dead code PHP makes it, and the engine answers over it.
        Presence::Always => None,
        // No minor the target spans has it, so the project's own declaration is
        // what runs. Only a SAME-FILE one answers; a cross-file declaration is the
        // slice §4 defers, and declines here as it does everywhere else.
        Presence::Never => Some(same_file.unwrap_or(None)),
        Presence::Straddles => Some(None),
    }
}

/// Whether the engine holds a name over the target's WHOLE minor range, none of
/// it, or part of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Presence {
    Always,
    Never,
    Straddles,
}

/// [`Presence`] for one engine name against the declared target.
fn engine_holds_the_name(row: Option<&ConstRow>, target: Option<&PhpTarget>) -> Presence {
    // A §3 class has no row and no arrival: the host sets, the integer width and
    // the version family exist at every minor Steins analyzes for.
    let (Some(row), Some(t)) = (row, target) else {
        // An undeclared target admits, the way [`target_admits`] does: a project
        // that says nothing about its PHP has not said it runs on the minor the
        // row excludes.
        return Presence::Always;
    };
    let below = row.since.is_some_and(|s| t.ceiling.is_some_and(|c| c < s));
    let above = row.until.is_some_and(|u| t.floor > u);
    if below || above {
        return Presence::Never;
    }
    if target_admits(row, Some(t)) { Presence::Always } else { Presence::Straddles }
}

/// **The names a constant reference can resolve to**, in PHP's own order.
///
/// The rule is not the class one and not the function one. A fully-qualified or
/// `namespace\`-relative name resolves to exactly one place. An unqualified name
/// consults `use const` first — **exact-case**, since constant names are
/// case-sensitive — and otherwise tries the current namespace and then falls back
/// to global, which is the fallback that lets `namespace App;` code write
/// `PHP_EOL` and mean the engine's.
///
/// Every candidate is normalized the way the index and the mined table key
/// themselves ([`normalize_const_fqn`]): namespace segments folded, final segment
/// left alone.
fn const_ref_candidates(cx: &Cx, r: &NameRef) -> Vec<String> {
    let ctx = cx.tree().ctx_at(r.offset);
    let ns = ctx.namespace.as_str();
    let qualify = |n: &str| {
        if ns.is_empty() { n.to_owned() } else { format!("{ns}\\{n}") }
    };
    let names = match r.kind {
        RefKind::FullyQualified => vec![r.raw.clone()],
        // `namespace\FOO` resolves against the enclosing namespace ONLY — no
        // `use`, no global fallback (ADR-0049 A8).
        RefKind::Relative => vec![qualify(&r.raw)],
        // A qualified name's FIRST segment is subject to a namespace import.
        RefKind::Qualified => {
            let first = r.raw.split('\\').next().unwrap_or(&r.raw);
            match ctx.class_imports.get(&first.to_ascii_lowercase()) {
                Some(target) => vec![format!("{target}{}", &r.raw[first.len()..])],
                None => vec![qualify(&r.raw)],
            }
        }
        RefKind::Unqualified => match ctx.const_imports.get(&r.raw) {
            Some(target) => vec![target.clone()],
            None if ns.is_empty() => vec![r.raw.clone()],
            // The namespace first, the global fallback second — PHP's own order.
            None => vec![qualify(&r.raw), r.raw.clone()],
        },
    };
    names.iter().map(|n| normalize_const_fqn(n)).collect()
}

/// **A same-file `const NAME = <literal>;` or `define('NAME', <literal>)`**
/// (ADR-0094 §4), bound from the declaration the walk already parsed.
///
/// `Some(None)` and `Some(Some(..))` are different answers and both are load
/// bearing: the outer `Some` says this file DECLARES the name, which stops the
/// engine table from answering for a project constant that happens to share a
/// spelling, and the inner `None` says the declaration states no value the reader
/// can take. It declines in three cases, each for its own reason:
///
/// * a **non-literal** initializer — the value would need an evaluation this
///   lowering does not do, and may depend on another file (the cross-file slice
///   ADR-0094 §4 defers);
/// * a **conditional** definition — `if (!defined('X')) define('X', 1);` says
///   what the value is when that branch runs, and taking it would report one arm
///   of a fork as the answer;
/// * **more than one** declaration of the name in the file, which is a fork of
///   the same shape without the `if`.
///
/// Verified: the declaration is the source, read directly.
fn same_file_fact(cx: &Cx, fqn: &str) -> Option<Option<(Fact, Stratum)>> {
    let mut decls = cx.tree().global_const_decls().iter().filter(|d| d.fqn == fqn);
    let decl = decls.next()?;
    if decls.next().is_some() || decl.conditional {
        return Some(None);
    }
    let fact = decl.value.as_ref().and_then(|v| singleton_fact(v, cx.php_minor));
    Some(fact.map(|f| (f, Stratum::Verified)))
}

/// The value a constant resolves to as a **literal**, for the seams that carry
/// values rather than facts (`resolve_literal_under`, and every fold and
/// comparison downstream of it).
///
/// Only a single-valued answer passes: a union (`PHP_EOL` without a pin) and a
/// range (`PHP_VERSION_ID`) are facts and not values, and a literal seam that
/// invented one member of them would be stating something false.
pub(crate) fn global_const_literal(cx: &Cx, r: &NameRef) -> Option<(ArgValue, Stratum)> {
    let (fact, stratum) = global_const_fact(cx, r)?;
    match fact {
        Fact::Singleton(Val::Int(i)) => Some((ArgValue::Int(i), stratum)),
        Fact::Singleton(Val::Float(f)) => Some((ArgValue::Float(f), stratum)),
        Fact::Singleton(Val::Str(s)) => Some((ArgValue::Str(s), stratum)),
        Fact::Singleton(Val::Bool(b)) => Some((ArgValue::Bool(b), stratum)),
        Fact::Singleton(Val::Null) => Some((ArgValue::Null, stratum)),
        _ => None,
    }
}

/// The mined row's value as a domain [`Fact`]. Total: every `ConstValue` arm is a
/// scalar the value domain has an inhabitant for (the miner refuses the ones it
/// does not — resources, `INF`, `NAN`).
fn mined_fact(value: &ConstValue) -> Fact {
    Fact::Singleton(match value {
        ConstValue::Int(i) => Val::Int(*i),
        ConstValue::Float(f) => Val::Float(*f),
        ConstValue::Bool(b) => Val::Bool(*b),
        ConstValue::Null => Val::Null,
        ConstValue::Str(s) => Val::Str(PhpStr::from_bytes(s.as_bytes())),
    })
}

/// **The mined row's version gate** (ADR-0094 §2), shaped like the declared-return
/// floor's [`floor_target_admits`]: the row speaks only for a target that lies
/// wholly inside the range the row claims.
///
/// A `since` above the target's floor declines — the project supports a minor
/// where the constant does not exist, and a value for a name that is not there is
/// not a fact about that minor. An **undeclared target admits**, as the floor's
/// gate does: a project that says nothing about its PHP has not said it runs on
/// the minor the row excludes.
///
/// [`floor_target_admits`]: crate::builtin_returns::floor_target_admits
fn target_admits(row: &ConstRow, target: Option<&PhpTarget>) -> bool {
    let Some(t) = target else {
        return true;
    };
    if let Some(since) = row.since
        && t.floor < since
    {
        return false;
    }
    // `until` is the last minor the name is known at. A ceiling above it — an
    // open-ended target included — reaches a minor the row does not cover.
    if let Some(until) = row.until
        && t.ceiling.is_none_or(|c| c > until)
    {
        return false;
    }
    true
}

/// **ADR-0094 §3's classes**, answered by rule.
///
/// Every arm here is a claim about the *deployment target*, which is why none of
/// these names is mined: the analysis machine's `PHP_EOL` is a fact about the
/// analysis machine, and a library's deployment host is not knowable from source.
/// The default is therefore the union of what the constant can be — sound on
/// every host — and a project that knows better says so with `[runtime] os`.
fn platform_fact(cx: &Cx, name: &str) -> Option<(Fact, Stratum)> {
    // §3.1 — integer width is 64-bit, always. No `4|8` union and no knob: a union
    // no runtime check can narrow away is the contradiction phpstan#14948 §1 and
    // §3 document, and Steins does not model 32-bit PHP at all (`Base::Int` is
    // documented 64-bit, `IntRange` clamps at i64, overflow to float is i64
    // overflow). Verified, since it is true of every target Steins supports.
    let verified = |f: Fact| Some((f, Stratum::Verified));
    let int = |i: i64| verified(Fact::Singleton(Val::Int(i)));
    let float = |f: f64| verified(Fact::Singleton(Val::Float(f)));
    match name {
        "PHP_INT_MAX" => return int(i64::MAX),
        "PHP_INT_MIN" => return int(i64::MIN),
        "PHP_INT_SIZE" => return int(8),
        "PHP_FLOAT_DIG" => return int(15),
        "PHP_FLOAT_EPSILON" => return float(f64::EPSILON),
        "PHP_FLOAT_MAX" => return float(f64::MAX),
        // php-src's `PHP_FLOAT_MIN` is the smallest NORMAL positive double
        // (`DBL_MIN`), not Rust's `f64::MIN` (the most negative finite one).
        "PHP_FLOAT_MIN" => return float(f64::MIN_POSITIVE),
        _ => {}
    }

    // §3 closed host sets. Windows is the only divergence php-src has for these
    // three, so each set has two members — and a pin picks one of them, at
    // `Asserted`, because it is the user's claim and not the language's.
    let host = |windows: &'static str, other: &'static str| -> Option<(Fact, Stratum)> {
        match cx.os_pin {
            Some(os) => Some((
                Fact::Singleton(Val::Str(PhpStr::from_bytes(if os.is_windows() { windows } else { other }.as_bytes()))),
                Stratum::Asserted,
            )),
            None => Some((
                Fact::from_vals(vec![
                    Val::Str(PhpStr::from_bytes(other.as_bytes())),
                    Val::Str(PhpStr::from_bytes(windows.as_bytes())),
                ])?,
                Stratum::Verified,
            )),
        }
    };
    match name {
        "PHP_EOL" => return host("\r\n", "\n"),
        "DIRECTORY_SEPARATOR" => return host("\\", "/"),
        "PATH_SEPARATOR" => return host(";", ":"),
        "PHP_OS_FAMILY" => {
            return match cx.os_pin {
                Some(os) => Some((
                    Fact::Singleton(Val::Str(PhpStr::from_bytes(os.php_os_family().as_bytes()))),
                    Stratum::Asserted,
                )),
                None => Some((
                    Fact::from_vals(
                        OS_FAMILIES
                            .iter()
                            .map(|s| Val::Str(PhpStr::from_bytes(s.as_bytes())))
                            .collect(),
                    )?,
                    Stratum::Verified,
                )),
            };
        }
        // §3 open host set. php-src does not close `PHP_OS`'s value set — not even
        // per family, which is why a pin does not narrow it — so the strongest
        // sound claim is that it is a non-empty string.
        "PHP_OS" => {
            return verified(Fact::refined(
                Base::String,
                Refinement::Str(StrPreds::NON_EMPTY.close()),
                false,
            ));
        }
        _ => {}
    }

    // §3 engine version, derived from the declared `PhpTarget`. With no target
    // there is nothing to derive from and the base is the honest floor.
    //
    // A project that declares `PHP_VERSION_ID` itself (a polyfill) takes the
    // whole family out of this rung, which is issue #29's own discipline applied
    // where it already lives: `Cx::version_id` goes `None` on that flag, and one
    // binary answering the target-derived range here while the version-guard fold
    // declines there would be two readings of one constant.
    if cx.units.iter().any(|u| u.tree.php_version_id_declared()) {
        return None;
    }
    version_fact(name, cx.php_target)
}

/// `PHP_VERSION*` from the project's declared target (ADR-0094 §3, "engine
/// version").
///
/// The derivation is the target's own arithmetic and nothing else — the analysis
/// engine's version never appears, because the project is not analyzed *for* the
/// machine running the analyzer. Each name takes the sharpest form its target
/// supports:
///
/// * `PHP_MAJOR_VERSION` / `PHP_MINOR_VERSION` are a literal when floor and
///   ceiling agree on that component, and the `int` floor otherwise.
/// * `PHP_VERSION_ID` is the `int<floor, ceiling>` range the target spans; an
///   open ceiling leaves it `int`.
/// * `PHP_VERSION` and `PHP_EXTRA_VERSION` stay strings: no target pins a patch
///   level (`PhpTarget` drops it by design), so no literal is derivable.
///   `PHP_EXTRA_VERSION` is the only one that may be EMPTY (a release build has
///   no suffix), so it is plain `string` where the others are `non-empty-string`.
/// * `PHP_RELEASE_VERSION` is the patch level, which the target drops entirely.
fn version_fact(name: &str, target: Option<&PhpTarget>) -> Option<(Fact, Stratum)> {
    let verified = |f: Fact| Some((f, Stratum::Verified));
    let int_general = || Fact::General { base: Base::Int, nullable: false };
    let non_empty_string =
        || Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY.close()), false);
    match name {
        "PHP_MAJOR_VERSION" => verified(match target {
            Some(t) if t.ceiling.is_some_and(|c| c.0 == t.floor.0) => {
                Fact::Singleton(Val::Int(i64::from(t.floor.0)))
            }
            _ => int_general(),
        }),
        "PHP_MINOR_VERSION" => verified(match target {
            Some(t) if t.ceiling == Some(t.floor) => Fact::Singleton(Val::Int(i64::from(t.floor.1))),
            _ => int_general(),
        }),
        // The patch level, which `PhpTarget` drops by design (every version-keyed
        // decision keys on the minor), so nothing narrows this.
        "PHP_RELEASE_VERSION" => verified(int_general()),
        "PHP_VERSION_ID" => verified(match target {
            Some(t) => {
                let lo = version_id(t.floor, 0);
                // The ceiling's own patch level is unknown, so the interval's top
                // is the last id that minor can carry — `8.3.x` reaches 80399.
                // `^8.1` spells its ceiling as `(8, u16::MAX)` — "any minor of 8",
                // which is an open bound wearing a number, and multiplying it out
                // would mint an interval no engine can reach.
                match t.ceiling.filter(|c| c.1 != u16::MAX).map(|c| version_id(c, 99)) {
                    Some(hi) => IntRange::new(lo, hi)
                        .map_or_else(int_general, |r| {
                            Fact::refined(Base::Int, Refinement::Int(r), false)
                        }),
                    None => int_general(),
                }
            }
            None => int_general(),
        }),
        "PHP_VERSION" => verified(non_empty_string()),
        // A release build has no extra suffix, so the empty string is in the set.
        "PHP_EXTRA_VERSION" => verified(Fact::General { base: Base::String, nullable: false }),
        _ => None,
    }
}

/// php-src's own `PHP_VERSION_ID` arithmetic: `major * 10000 + minor * 100 + patch`.
fn version_id((major, minor): (u16, u16), patch: i64) -> i64 {
    i64::from(major) * 10_000 + i64::from(minor) * 100 + patch
}
