//! Lowering between the checker's values and the fold wire: the analysis PHP view
//! (issue #28), the array / union fold budgets, and the `ArgValue` ↔ `FoldValue`
//! conversions with their shape tracking.

use steins_domain::PhpStr;
use steins_sidecar::{FoldArg, FoldKey, FoldValue};
use steins_syntax::{ArgValue, ArrayKey, php_canonical_int_string};

/// The ADR-0049 A12 boundary: the minor where PHP changed the next-auto-index
/// rule for array literals with negative keys. The one version boundary any
/// value rule keys on today.
const NEXT_INT_BOUNDARY: (u16, u16) = (8, 3);

/// The analysis PHP view (issue #28): fold the sidecar's **runtime** minor and
/// the project's **declared target** into the two per-run answers the checker
/// consumes.
///
/// - The **effective minor** feeds `normalize_array` (ADR-0049 A12): with a
///   declared target, the range must agree on the next-int boundary — one side
///   entirely answers with its floor, a straddling range answers `None` (A12's
///   existing unknown leg: a boundary-sensitive literal declines, every other
///   literal still resolves). With no target, the runtime minor answers as
///   before #28.
/// - The **catalog skew** flag feeds ADR-0052 A11's arm-deletion demotion: the
///   catalog is verified only at [`steins_catalog::PINNED_PHP`], so a target
///   range is skewed unless it is exactly the pin; no target falls back to the
///   runtime-vs-pin comparison (A11 unchanged).
/// - The **version-id interval** feeds the issue-#29 `PHP_VERSION_ID` guard
///   fold — see [`PhpView::version_id`].
///
/// (This doc describes [`effective_php_view`]; the two small id helpers below
/// are its arithmetic.)
///
/// The lowest `PHP_VERSION_ID` a `(major, minor)` admits (patch `00`).
fn version_id_lo(m: (u16, u16)) -> u32 {
    u32::from(m.0) * 10_000 + u32::from(m.1.min(99)) * 100
}

/// The highest `PHP_VERSION_ID` a `(major, minor)` ceiling admits (patch `99`;
/// a `(maj, u16::MAX)` "any minor of this major" ceiling caps at minor 99 —
/// PHP_VERSION_ID reserves two digits per component).
fn version_id_hi(m: (u16, u16)) -> u32 {
    u32::from(m.0) * 10_000 + u32::from(m.1.min(99)) * 100 + 99
}

/// The per-run PHP view (issues #28/#29): the three version answers the checker
/// consumes, all derived from the one target-or-runtime seam.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PhpView {
    /// The effective minor for version-keyed value rules (ADR-0049 A12) — see
    /// [`effective_php_view`].
    pub(crate) effective_minor: Option<(u16, u16)>,
    /// The ADR-0052 A11 catalog-skew flag.
    pub(crate) catalog_skew: bool,
    /// The `PHP_VERSION_ID` interval `[lo, hi]` the analysis is about (issue
    /// #29): the declared target range's, else the runtime minor's (its exact
    /// patch is unknown, so the interval spans the minor). `hi = None` is an
    /// open upper bound; the whole thing `None` when no version is known at all.
    pub(crate) version_id: Option<(u32, Option<u32>)>,
}

pub(crate) fn effective_php_view(
    runtime: Option<(u16, u16)>,
    target: Option<&steins_db::PhpTarget>,
) -> PhpView {
    match target {
        Some(t) => PhpView {
            effective_minor: if t.straddles(NEXT_INT_BOUNDARY) { None } else { Some(t.floor) },
            catalog_skew: !t.is_exactly(steins_catalog::PINNED_PHP),
            version_id: Some((version_id_lo(t.floor), t.ceiling.map(version_id_hi))),
        },
        None => PhpView {
            effective_minor: runtime,
            catalog_skew: runtime.is_some_and(|m| m != steins_catalog::PINNED_PHP),
            version_id: runtime.map(|m| (version_id_lo(m), Some(version_id_hi(m)))),
        },
    }
}

/// Parse a PHP version string (`"8.5.8"`, `"8.5.8-dev"`) to `(major, minor)`.
/// `None` when the first two dotted components are not both integers.
pub(crate) fn parse_php_minor(v: &str) -> Option<(u16, u16)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor_part = it.next()?;
    // A minor like `5` or `5-dev`: take the leading digit run.
    let minor: u16 = minor_part.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    Some((major, minor))
}

/// The fold seam's array budget (issue #39): the greatest number of entries an
/// array argument may contribute, counted **recursively** over nesting, and the
/// deepest nesting a fold argument may have.
///
/// Both exist because a fold argument is serialized, sent over IPC, executed, and
/// used as a memo key — all linear in the literal's size, all paid per call site.
/// The values cover every array literal a human writes as a `count` /
/// `in_array` / `implode` argument while refusing a generated 10,000-entry
/// lookup table, where a fold costs real time and buys nothing (its `count` is
/// a number nobody would mistype into a `string` parameter). Exceeding either
/// bound **widens** — a miss, never a false positive (ADR-0002). The depth
/// bound also keeps the recursive encoders here and in the runner off an
/// unbounded stack.
const FOLD_ARRAY_MAX_ENTRIES: usize = 256;
pub(crate) const FOLD_ARRAY_MAX_DEPTH: u8 = 8;

/// The member-wise union fold's bounds (issue #74): at most this many members in
/// any one argument's union, and at most [`UNION_FOLD_COMBINATION_CAP`]
/// combinations across the whole argument list.
///
/// Busting either bound **declines** — [`Cx::try_union_fold`] never truncates the
/// product. A union missing a member is a *wrong* value domain, not a wider one,
/// and widening is the only safe direction (ADR-0002).
///
/// [`Cx::try_union_fold`]: crate::cx::Cx::try_union_fold
pub(crate) const UNION_FOLD_MEMBER_CAP: usize = 4;
pub(crate) const UNION_FOLD_COMBINATION_CAP: usize = 16;

/// The wire spelling of **one non-array value**, or `None` when it has none.
///
/// This is where the fold seam decides what can cross the JSON wire at all, and
/// it decides it **once**. Two recursions consult it — [`fits_fold_budget`]'s
/// gate and [`arg_to_fold_within`]'s encoder — because they need different
/// things from the same verdict (a `bool` and a value), and a rule transcribed
/// into both is a rule that can be extended in one of them. It has been:
/// non-finite floats were caught in review only after the two had drifted apart
/// in exactly that way.
///
/// The declines, each for its own reason:
///
/// * **A non-finite float.** JSON has no token for `INF`/`-INF`/`NAN`, and PHP's
///   lexer mints the first two from source a program can contain: `1e309`
///   overflows to `INF` while staying a literal. There is no encoding that asks
///   the engine the question the source asked — encoding it as `null` made a
///   weak-mode `floor(1e309)` answer `0.0`, which is `floor(null)` — so the fold
///   declines. It is the mirror of the runner's own refusal to *return* a
///   non-finite result.
/// * **A non-UTF-8 string.** The fold wire is JSON, which cannot carry arbitrary
///   bytes (ADR-0080 §2.6), so the fold declines rather than asking PHP about a
///   different string than the source has. Restoring it is ADR-0080 §3.1.
/// * **Anything that is not a literal at all**, each for its own reason. A
///   method call (issue #386) is no more a wire value than a function call: the
///   fold sends proven values, and this one is proven — if at all — by a descent
///   the fold road has no store to run (ADR-0075 §3). A concatenation is not a
///   wire value either: `try_fold` resolves each argument through
///   `resolve_literal` first, so a provable `"a" . $b` arrives here already
///   collapsed to its `Str`, and one that did not resolve is unproven — sending
///   its operands would be sending a different call than was written. A
///   comparison in value position (issue #260) is the same story: decided ones
///   arrive as a `Bool`, undecided ones are unproven. Object-world values
///   (ADR-0043) are unproven here by construction, and a global-constant fetch
///   (issue #168) is too — only the preg flags reader resolves the modeled
///   engine constants, by value.
///
/// An **array** answers `None` here, and that is not the array's verdict: an
/// array's admission is the depth-and-budget walk its two callers own, and each
/// matches the array arm before consulting this. `None` is the conservative
/// answer to a question this function is not asked.
fn scalar_to_fold(v: &ArgValue) -> Option<FoldArg> {
    match v {
        ArgValue::Int(v) => Some(FoldArg::Int(*v)),
        ArgValue::Float(v) => v.is_finite().then_some(FoldArg::Float(*v)),
        ArgValue::Str(v) => Some(FoldArg::Str(v.as_str()?.to_owned())),
        ArgValue::Bool(v) => Some(FoldArg::Bool(*v)),
        ArgValue::Null => Some(FoldArg::Null),
        ArgValue::Array(_)
        | ArgValue::Var(_)
        | ArgValue::Call(..)
        | ArgValue::MethodCall { .. }
        | ArgValue::New(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Closure(_)
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        | ArgValue::Coalesce(..)
        | ArgValue::OffsetRead { .. }
        | ArgValue::Concat(..)
        | ArgValue::Binary { .. }
        | ArgValue::Isset(_)
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::GlobalConst(..)
        | ArgValue::Other => None,
    }
}

/// The wire spelling of **one array-literal key**, or `None` when it has none.
///
/// The nesting reads oddly and is load-bearing: the outer `Option` is the
/// admission, and the inner one is the key's own **absence**. `Some(None)` is
/// `[$a]`'s auto key, which travels as JSON `null` so PHP's own next-int rule
/// assigns it; `None` is a key that cannot be sent at all, and it widens the
/// WHOLE array rather than degrading to an auto key — that would be a different
/// claim about the array than the source makes.
///
/// Two keys have no spelling: one the source did not write as a literal (issue
/// #336) — the seam sends the engine the array that was written or nothing at
/// all — and a non-UTF-8 string key, for the reason [`scalar_to_fold`] gives.
pub(crate) fn array_key_to_fold(k: &ArrayKey) -> Option<Option<FoldKey>> {
    match k {
        ArrayKey::Auto => Some(None),
        ArrayKey::Int(i) => Some(Some(FoldKey::Int(*i))),
        ArrayKey::Str(s) => Some(Some(FoldKey::Str(s.as_str()?.to_owned()))),
        ArrayKey::Expr(_) => None,
    }
}

/// Charge `v` against the fold seam's array budget: `false` when it nests deeper
/// than `depth` or its entries (recursively) exhaust `budget`, or when it is not a
/// self-evident value at all. A scalar literal always fits and costs nothing.
///
/// The budget is **per argument**, and both users of it ([`Cx::try_fold`]'s gate
/// and [`arg_to_fold`]'s encoder) charge identically, so the gate's verdict and the
/// encoder's are the same verdict computed twice — never a gate that admits what
/// the encoder then refuses. Only the *walk* is computed twice now: what a single
/// value or key may be is [`scalar_to_fold`]'s and [`array_key_to_fold`]'s answer,
/// asked here and by the encoder, so a new wire constraint cannot land in one
/// recursion and miss the other.
///
/// [`Cx::try_fold`]: crate::cx::Cx::try_fold
fn fits_fold_budget(v: &ArgValue, depth: u8, budget: &mut usize) -> bool {
    match v {
        ArgValue::Array(items) => {
            if depth == 0 {
                return false;
            }
            for (key, el) in items {
                if *budget == 0 {
                    return false;
                }
                *budget -= 1;
                if array_key_to_fold(key).is_none() {
                    return false;
                }
                if !fits_fold_budget(el, depth - 1, budget) {
                    return false;
                }
            }
            true
        }
        v => scalar_to_fold(v).is_some(),
    }
}

/// The PHP string cast of a proven operand of `.` (issue #59), or `None` when the
/// cast is not one this crate may derive.
///
/// # Why this is derived here and not sent to the sidecar
///
/// The fold seam's standing rule is that PHP semantics are answered by running the
/// project's own PHP (ADR-0004/0028), never re-derived in Rust. Concatenation earns
/// an exception on a narrow, checkable ground: for the operand types admitted below
/// the cast is *total and environment-independent* — byte concatenation of two
/// strings consults no locale/encoding/ini setting, `int` has one decimal
/// spelling, and `bool`/`null` have fixed one-character-or-empty spellings. No
/// configuration makes php-src answer differently, so there is nothing for the
/// sidecar to arbitrate.
///
/// `float` is **excluded** because PHP's float-to-string conversion is governed by
/// the `precision` ini directive (default 14): `0.1 + 0.2` prints as `0.3` on a
/// stock build and `0.30000000000000004` under `precision=17`. A value that
/// depends on runtime configuration is exactly what this crate must not invent —
/// `strval(1.5)` stays on the `foldable` allowlist, answered by the real engine.
///
/// Arrays (`"Array"` plus a warning), objects (`__toString` or an `Error`) and
/// every unresolved carrier widen for the ordinary reason: the result is not
/// proven.
pub(crate) fn concat_cast(v: &ArgValue) -> Option<PhpStr> {
    match v {
        ArgValue::Str(s) => Some(s.clone()),
        ArgValue::Int(i) => Some(PhpStr::from(i.to_string())),
        // `true` is "1", `false` is "" — verified against php-src, not assumed.
        ArgValue::Bool(b) => Some(PhpStr::from(if *b { "1" } else { "" })),
        ArgValue::Null => Some(PhpStr::new()),
        _ => None,
    }
}

/// Whether `arg` may be sent to the sidecar at all: a scalar literal, or an array
/// literal that is concrete all the way down *and* inside the budget.
pub(crate) fn is_fold_arg(arg: &ArgValue) -> bool {
    let mut budget = FOLD_ARRAY_MAX_ENTRIES;
    fits_fold_budget(arg, FOLD_ARRAY_MAX_DEPTH, &mut budget)
}

/// Convert a literal or literal-array [`ArgValue`] to a [`FoldArg`]; anything else
/// (and anything over the budget) yields `None`, which widens.
pub(crate) fn arg_to_fold(arg: &ArgValue) -> Option<FoldArg> {
    let mut budget = FOLD_ARRAY_MAX_ENTRIES;
    arg_to_fold_within(arg, FOLD_ARRAY_MAX_DEPTH, &mut budget)
}

/// The budget-carrying body of [`arg_to_fold`]. Array entries keep their **source
/// order and their raw keys**: an absent key stays absent on the wire, so PHP's own
/// next-int rule assigns it, and a duplicate key is resolved by PHP's own
/// last-wins. Nothing here re-derives array semantics — that is precisely what
/// running the fold on the project's PHP is for (ADR-0004/0028).
fn arg_to_fold_within(arg: &ArgValue, depth: u8, budget: &mut usize) -> Option<FoldArg> {
    match arg {
        // An array literal (issue #39): representable when every element is, and
        // nested literals recurse. One unrepresentable element widens the WHOLE
        // array — `count([1, $x])` is not 2, because `$x` may not be one entry.
        ArgValue::Array(items) => {
            if depth == 0 {
                return None;
            }
            let mut entries = Vec::with_capacity(items.len());
            for (k, v) in items {
                if *budget == 0 {
                    return None;
                }
                *budget -= 1;
                // Both `?`s widen the whole array rather than dropping an entry:
                // a shorter array is a different argument, not a wider one.
                entries.push((array_key_to_fold(k)?, arg_to_fold_within(v, depth - 1, budget)?));
            }
            Some(FoldArg::Array(entries))
        }
        // Every other value is a wire question or it is not, and that is
        // `scalar_to_fold`'s single answer — the same one `fits_fold_budget`
        // gets, which is the point of it living there.
        v => scalar_to_fold(v),
    }
}

/// Convert a folded value back to a literal [`ArgValue`].
///
/// An array result (ADR-0028's 2026-08-14 amendment, issue #330) is **charged
/// against the argument budget on arrival**, deliberately using the argument
/// side's own [`is_fold_arg`] rather than a second bound of its own. The
/// amendment's invariant is that a shape admissible as an argument is admissible
/// as a result; reusing the function makes the runner's pre-encode verdict and
/// this one literally the same computation, so neither can drift into admitting
/// what the other refuses. Over budget yields `None`, which widens.
pub(crate) fn fold_value_to_arg(value: &FoldValue) -> Option<ArgValue> {
    let arg = fold_value_shape(value)?;
    if !is_fold_arg(&arg) {
        return None;
    }
    Some(arg)
}

/// The shape half of [`fold_value_to_arg`], before the budget is charged.
///
/// The recursion is bounded by the decoder that produced `value`: `serde_json`
/// refuses to parse past its own nesting limit, so no reply can hand this an
/// envelope deep enough to exhaust the stack before the budget check runs.
fn fold_value_shape(value: &FoldValue) -> Option<ArgValue> {
    Some(match value {
        FoldValue::Int(v) => ArgValue::Int(*v),
        FoldValue::Float(v) => ArgValue::Float(*v),
        FoldValue::Str(v) => ArgValue::Str(PhpStr::from(v.clone())),
        FoldValue::Bool(v) => ArgValue::Bool(*v),
        FoldValue::Null => ArgValue::Null,
        FoldValue::Array(entries) => {
            let mut items = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let key = match k {
                    FoldKey::Int(i) => ArrayKey::Int(*i),
                    // An integer-like string key is not a key PHP hands back: the
                    // engine casts `"5"` to `5` when the array is built, so a
                    // materialized result cannot carry one. Widening rather than
                    // re-casting keeps the decoder's stance on unreachable
                    // spellings uniform with its `null`-key and duplicate-key
                    // rules — the alternative, letting it through as a string key,
                    // would be a *wrong* fact rather than a wider one.
                    FoldKey::Str(s) if php_canonical_int_string(s).is_some() => return None,
                    FoldKey::Str(s) => ArrayKey::Str(PhpStr::from(s.clone())),
                };
                items.push((key, fold_value_shape(v)?));
            }
            ArgValue::Array(items)
        }
    })
}

#[cfg(test)]
mod php_view_tests {
    use super::*;
    use steins_db::{PhpTarget, PhpTargetSource};

    fn target(floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
        PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: String::new() }
    }

    /// Issue #28: the one seam both A11 and A12 follow (and, since #29, the
    /// PHP_VERSION_ID guard interval).
    #[test]
    fn a_declared_target_overrides_the_runtime() {
        // A range straddling the A12 boundary declines the effective minor
        // (boundary-sensitive literals must decline) and skews the catalog; the
        // version-id interval spans the declared range [8.1.00, 8.99.99].
        let caret81 = target((8, 1), Some((8, u16::MAX)));
        let v = effective_php_view(Some((8, 5)), Some(&caret81));
        assert_eq!((v.effective_minor, v.catalog_skew), (None, true));
        assert_eq!(v.version_id, Some((80100, Some(89999))));
        // A range entirely below the boundary answers with its floor.
        let old = target((8, 1), Some((8, 2)));
        let v = effective_php_view(Some((8, 5)), Some(&old));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 1)), true));
        assert_eq!(v.version_id, Some((80100, Some(80299))));
        // A range entirely at/above the boundary answers with its floor too; an
        // open ceiling is an open interval.
        let new = target((8, 3), None);
        let v = effective_php_view(Some((8, 1)), Some(&new));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 3)), true));
        assert_eq!(v.version_id, Some((80300, None)));
        // A target pinned exactly to the catalog pin carries no skew.
        let pinned = target(steins_catalog::PINNED_PHP, Some(steins_catalog::PINNED_PHP));
        let v = effective_php_view(None, Some(&pinned));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some(steins_catalog::PINNED_PHP), false));
    }

    /// No declaration: the pre-#28 posture, verbatim — runtime minor passthrough,
    /// skew iff the runtime differs from the pin; the version-id interval spans
    /// the runtime's minor (the exact patch is unknown).
    #[test]
    fn no_target_falls_back_to_the_runtime() {
        let v = effective_php_view(Some(steins_catalog::PINNED_PHP), None);
        assert_eq!((v.effective_minor, v.catalog_skew), (Some(steins_catalog::PINNED_PHP), false));
        let v = effective_php_view(Some((8, 1)), None);
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 1)), true));
        assert_eq!(v.version_id, Some((80100, Some(80199))));
        let v = effective_php_view(None, None);
        assert_eq!((v.effective_minor, v.catalog_skew, v.version_id), (None, false, None));
    }
}

#[cfg(test)]
mod fold_wire_tests {
    //! The fold seam's admission, checked where it is now decided *once*.
    //!
    //! [`fits_fold_budget`]'s gate and [`arg_to_fold_within`]'s encoder run over
    //! the same argument at different moments — the gate before `folder.fold` so
    //! an inadmissible literal is never cloned into the memo, the encoder inside
    //! it — and the seam's standing invariant is that they never disagree. A gate
    //! that admits what the encoder refuses asks the engine a question it cannot
    //! be given; a gate that refuses what the encoder would send loses a fold for
    //! no reason. Both now read [`scalar_to_fold`] and [`array_key_to_fold`], so
    //! this asserts the property rather than two transcriptions of it.
    use steins_sidecar::FoldKey;
    use crate::fold_args::{arg_to_fold, array_key_to_fold, is_fold_arg};
    use steins_domain::PhpStr;
    use steins_syntax::{ArgValue, ArrayKey};

    /// A byte string with no UTF-8 reading — `as_str()` is `None`, ADR-0080.
    fn raw_byte_string() -> PhpStr {
        PhpStr::from_bytes(&[0xC0])
    }

    /// Every value the seam has an opinion about, sendable or not.
    fn every_shape() -> Vec<ArgValue> {
        let inner = ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Int(1))]);
        vec![
            ArgValue::Int(1),
            ArgValue::Float(1.5),
            ArgValue::Float(-0.0),
            ArgValue::Float(f64::MAX),
            ArgValue::Float(f64::INFINITY),
            ArgValue::Float(f64::NEG_INFINITY),
            ArgValue::Float(f64::NAN),
            ArgValue::Str(PhpStr::from("ab")),
            ArgValue::Str(raw_byte_string()),
            ArgValue::Bool(true),
            ArgValue::Null,
            ArgValue::Other,
            ArgValue::Array(vec![]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Int(1))]),
            // The hazards, each buried one level down: the array is admissible
            // in every other respect, and one entry has to take it down.
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Float(f64::INFINITY))]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Str(raw_byte_string()))]),
            ArgValue::Array(vec![(ArrayKey::Str(raw_byte_string()), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Expr(Box::new(ArgValue::Other)), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Int(-3), ArgValue::Null)]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Other)]),
            // …and nested twice, since the walk is where the two recursions
            // could still drift apart.
            ArgValue::Array(vec![(ArrayKey::Auto, inner)]),
            ArgValue::Array(vec![(
                ArrayKey::Auto,
                ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Float(f64::NAN))]),
            )]),
        ]
    }

    #[test]
    fn the_gate_admits_exactly_what_the_encoder_sends() {
        for v in every_shape() {
            assert_eq!(
                is_fold_arg(&v),
                arg_to_fold(&v).is_some(),
                "the gate and the encoder disagree about {v:?}"
            );
        }
    }

    /// The three values that have no JSON spelling, named so a future reader
    /// sees WHICH shapes the agreement above is really about.
    #[test]
    fn a_value_with_no_wire_spelling_is_refused_by_both() {
        for v in [
            ArgValue::Float(f64::INFINITY),
            ArgValue::Str(raw_byte_string()),
            ArgValue::Array(vec![(ArrayKey::Str(raw_byte_string()), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Expr(Box::new(ArgValue::Other)), ArgValue::Int(1))]),
        ] {
            assert!(!is_fold_arg(&v), "the gate admits {v:?}");
            assert_eq!(arg_to_fold(&v), None, "the encoder sends {v:?}");
        }
        // The neighbours still travel: this refuses spellings, not types.
        assert!(arg_to_fold(&ArgValue::Float(f64::MAX)).is_some());
        assert!(arg_to_fold(&ArgValue::Str(PhpStr::from("ab"))).is_some());
    }

    /// An absent key is a key the wire carries (`null`, for PHP's next-int
    /// rule); it is not the "no spelling" answer, and the nesting in
    /// [`array_key_to_fold`]'s return type is what keeps them apart.
    #[test]
    fn an_absent_key_is_not_a_refused_key() {
        assert_eq!(array_key_to_fold(&ArrayKey::Auto), Some(None));
        assert_eq!(array_key_to_fold(&ArrayKey::Int(7)), Some(Some(FoldKey::Int(7))));
        assert_eq!(array_key_to_fold(&ArrayKey::Expr(Box::new(ArgValue::Other))), None);
        assert_eq!(array_key_to_fold(&ArrayKey::Str(raw_byte_string())), None);
    }
}
