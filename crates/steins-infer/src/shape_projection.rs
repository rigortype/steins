//! The positional projections over the order-dependent array builtins (ADR-0062
//! §4 / S5): witnessed entries, slices and their key preservation, and the value /
//! key / flip / reverse projections of a shape fact.

use std::collections::HashMap;

use steins_domain::{
    Base, Certainty, Fact, IntRange, PhpStr, Refinement, ShapeFact, Key as VKey, Val,
};
use steins_syntax::{ArgValue, php_canonical_int_string};

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::{fact_admitting_null, join_into, val_of_key};
use crate::builtin_returns::transfer_declaration_admits;
use crate::transfers::transfer_arg_fact;

/// **The positional projections over the order-DECLARED lane** (ADR-0062 §4's
/// `array_values`/`array_keys`/… row, §2's rule, §7's declined import 1).
///
/// A `Fact::Shape` is a key *set*, never a key sequence: field order in the fact
/// is the domain's canonical [`VKey`] order, unrelated to insertion order. So
/// every transfer here is a **sound widening** reading only order-independent
/// structure — the value union, the key union, the key classes, `non_empty`, and
/// the denotational `is_list`. None may produce `list{k1, k2}` in declaration
/// order — the upstream defect (phpstan/phpstan#14940) this ADR declines.
///
/// Concrete arrays never come here: they are order-**witnessed**, and the fold
/// seam runs the real builtin on the real array (ADR-0004/0028) for an exact,
/// order-correct answer.
///
/// **The admission gate** is [`Folder::builtin_return_type`]: the running engine
/// must itself declare the return type this transfer assumes (`array` for the
/// four array-valued projections, `string|int|null` for the key-member pair),
/// carrying the same sidecar-presence and A9 monkey-patch legs the ADR-0061 §2
/// envelope gate does.
///
/// # The read-position family and its arity second leg (ADR-0064 Amendment B)
///
/// The ten names `current reset end next prev key array_pop array_shift
/// array_first array_last` read *a position* rather than restructuring the array,
/// so their answer is drawn from the shape's **value** union (or, for `key`, its
/// key union). `key` reuses the `array_key_first` arm verbatim.
///
/// The nine value forms declare a bare **`mixed`**, which pins nothing on its own.
/// ADR-0064 Amendment B rules that inadmissible and requires the **arity second
/// leg** — [`Folder::builtin_param_counts`] must report `(1, 1)` for all ten
/// (measured at `PINNED_PHP`). An engine that cannot answer the arity withholds
/// the rule exactly as one silent on the declaration does.
///
/// **The internal pointer is not modeled**, the source of each arm's `false`/
/// `null` arm (probes at 8.5.8):
///
/// * `next`/`prev` are **unconditionally** `∪ false` — they step off the end of
///   even a non-empty array (`$a = [1]; next($a) === false`).
/// * `current`/`reset`/`end` add `false` only when the shape may be empty; on a
///   non-empty shape they take the union alone (matching upstream PHPStan),
///   assuming the pointer has not already advanced past the end. Tolerable here
///   since ADR-0062 A-G9's corollary keeps a shape-derived fact out of every
///   proof-layer premise, with the fp-gate as the standing instrument.
/// * `array_pop`/`array_shift`/`array_first`/`array_last` never touch the pointer;
///   they add `null` on a possibly-empty shape.
///
/// A `∪ false` is now sayable (issue #339): `Fact::Union` holds a two-base union,
/// so `next($x)` over an `int`-valued shape answers `int|bool` (not `int|false` —
/// the `Bool` base carries no refinement, so `false` widens to its base). Sound,
/// coarser than the reference implementation (ADR-0085 §5).
///
/// **Mutation is not this function's business, and must not be.** Six of the ten
/// take argument 0 by reference and move or shorten it. The *return* is computed
/// from the pre-call shape, which is correct; the argument's own fact is dropped
/// after the statement by the walk's call-argument invalidation, and
/// `steins_catalog::out_params` carries all six so ADR-0063 §2.3's by-ref
/// coloring agrees.
///
/// # The argument channel (issue #118, ADR-0062 Amendment B)
///
/// The v1 report declined `array_slice` because "the seam is single-argument by
/// construction" — so the implementation grew the seam instead: the rung now
/// receives the CALL's argument list, and an arm may read a sibling argument's
/// fact through [`transfer_arg_fact`] (the DR3 rung's own reader). Arms that
/// don't ask keep the single-shape shape they always had; the §2 order boundary
/// is untouched — arms read only a `$preserve_keys` flag, an offset, a length.
pub(crate) fn shape_projection_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    shape: &ShapeFact,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    /// The `(string)` renderings of `array_key_first`/`array_key_last`/`key`'s
    /// declared return type. PHP 8 renders the union in its own order; both are
    /// accepted so the gate tests the *declaration*, not the engine's spelling of it.
    const KEY_OR_NULL: &[&str] = &["string|int|null", "int|string|null"];
    const ARRAY: &[&str] = &["array"];
    /// The read-position family's declaration: `mixed`, which pins nothing on its
    /// own — every arm using it MUST carry [`ARITY_1`] (ADR-0064 Amendment B, and
    /// the `debug_assert!` at the gate below).
    const MIXED: &[&str] = &["mixed"];
    /// The read-position family's live signature at `PINNED_PHP`: one parameter,
    /// required. Measured, not assumed — see `reflect_reports_the_parameter_counts`.
    const ARITY_1: Option<(u32, u32)> = Some((1, 1));
    /// `array_slice`'s live signature at `PINNED_PHP` (8.5.8): four parameters, two
    /// required — `array_slice(array $array, int $offset, ?int $length = null,
    /// bool $preserve_keys = false)`. Measured, not assumed. Its `array`
    /// declaration is already a real pin, so Amendment B does not *demand* the
    /// second leg here; the arm carries it anyway because it is the one arm that
    /// reads its siblings **positionally**, and a php-src signature that grew a
    /// parameter in front of `$preserve_keys` would make the read stale while the
    /// declaration still said `array`.
    const ARITY_SLICE: Option<(u32, u32)> = Some((4, 2));

    let lower = name.to_ascii_lowercase();
    // ---- The ARGUMENT-READING arm (issue #118), taken first ------------------
    //
    // `array_slice($x, $offset [, $length [, $preserve_keys]])`: the WIDENING FLOOR
    // of the two rungs — see [`slice_widening`] for what it claims and why each
    // part is sound for any offset/length whatever. The exact rung lives in
    // [`witnessed_projection_fact`], on the value lane alone.
    //
    // It is matched *before* the arity binding below because it is the one arm the
    // grown seam gave an argument channel to; everything after this point is
    // single-argument by construction.
    if lower == "array_slice" {
        let out = slice_widening(cx, folder, shape, args, env, store)?;
        return transfer_declaration_admits(cx, folder, name, ARRAY, ARITY_SLICE).then_some(out);
    }
    // `array_fill_keys($keys, $value)` over a DECLARED `$keys` (issue #336 piece
    // 2). Placed with `array_slice` above the single-argument gate for the same
    // reason: it reads a sibling argument.
    //
    // The witnessed lane computes this entry by entry; here only the key CLASS
    // is knowable, and it comes from the array-key cast of the subject's value
    // union — which is what lets `list<decimal-int-string>` key an `int` array
    // even though its values' base is `string`.
    //
    // Unlike `array_flip`, `non_empty` **carries**: every value becomes a key,
    // none is skipped. Probed at 8.5.9 — even an array value survives, as the
    // string key `'Array'` with a warning, and a float as its string rendering.
    // (Those two are exactly what `array_key_cast` declines to name, which costs
    // the key class and never the entry count.)
    if lower == "array_fill_keys" && args.len() == 2 {
        let out = ShapeFact::normalize(
            Vec::new(),
            steins_domain::Tail::Unsealed {
                key: filled_key_class(shape),
                value: transfer_arg_fact(cx, folder, &args[1], env, store).map(Box::new),
            },
            Certainty::Maybe,
            shape.non_empty,
            Vec::new(),
        );
        return transfer_declaration_admits(cx, folder, name, ARRAY, None)
            .then_some(shape_fact(out));
    }
    // The rest of the family reads exactly ONE argument, and a call passing more is
    // a different function than the rule describes: `array_reverse($x, true)`
    // preserves keys, `current($x, 1)` is an `ArgumentCountError`.
    let [_] = args else { return None };
    let (out, declared, arity): (Fact, &[&str], Option<(u32, u32)>) = match lower.as_str() {
        // `array_values($x)`: the values in witnessed order, reindexed. The key
        // structure is gone (a list), the value set is preserved exactly, and an
        // array with an entry still has one after the projection.
        "array_values" => (shape_fact(project_values(shape)), ARRAY, None),
        // `array_keys($x)`: a list whose ELEMENTS are the keys. Enumerable only
        // under a sealed tail, where every present key is a declared key.
        "array_keys" => (shape_fact(project_keys(shape)), ARRAY, None),
        // `array_flip($x)`: keys and values swap. Both bounds widen (see the
        // helper), and `non_empty` is *dropped* — flip silently skips an entry
        // whose value is not `int|string`, so a non-empty input can flip to `[]`.
        "array_flip" => (shape_fact(project_flip(shape)), ARRAY, None),
        // `array_reverse($x)` — the one-argument form only, whose `$preserve_keys`
        // is `false`: string keys survive, integer keys are renumbered.
        "array_reverse" => (shape_fact(project_reverse(shape)), ARRAY, None),
        // `array_key_first`/`array_key_last`: **SOME key of the set**, never the
        // declared-first one — ADR-0062 §2's rule at its sharpest. `null` joins in
        // unless the shape proves the array is non-empty (PHP returns `null` for
        // `[]`).
        //
        // `key($x)` reads the key AT THE INTERNAL POINTER, which is the same
        // widening — some key of the set, or `null` — so it shares this arm
        // verbatim, and shares the real `string|int|null` pin with it (the one
        // member of the read-position family whose declaration says something).
        "array_key_first" | "array_key_last" | "key" => {
            let keys = shape_key_union(shape)?;
            let out = if shape.non_empty { keys } else { fact_admitting_null(&keys)? };
            (out, KEY_OR_NULL, None)
        }
        // ---- The read-position VALUE forms: `mixed` declaration + arity pin ----
        //
        // `array_pop`/`array_shift` take the last/first entry OFF the array and
        // return it; `array_first`/`array_last` (PHP 8.5) read the same entries
        // without mutating. All four ignore the internal pointer, and all four
        // return `null` — not `false` — on an empty array.
        "array_pop" | "array_shift" | "array_first" | "array_last" => {
            (read_position_value(shape, Val::Null)?, MIXED, ARITY_1)
        }
        // `reset`/`end` move the pointer to the first/last entry and return it;
        // `current` returns the entry at wherever the pointer already is. Their
        // empty-array answer is `false` (`current([]) === false`), and on a
        // non-empty shape they take the value union alone — the pointer assumption
        // documented above.
        "current" | "reset" | "end" => {
            (read_position_value(shape, Val::Bool(false))?, MIXED, ARITY_1)
        }
        // `next`/`prev` STEP the pointer, and a step off either end returns `false`
        // — from a non-empty array just as readily as from an empty one
        // (`$a = [1]; next($a) === false`; `$a = [1, 2]; prev($a) === false`). So
        // the `false` arm is unconditional here, and `non_empty` buys nothing.
        "next" | "prev" => {
            let out = if shape.can_be_non_empty() {
                fact_admitting_false(&shape_value_union(shape)?)?
            } else {
                Fact::Singleton(Val::Bool(false))
            };
            (out, MIXED, ARITY_1)
        }
        // Still declined, and the reason has CHANGED (issue #339). It used to be
        // that the value side of `in_array`/`array_search` is a multi-base union
        // (`int|string|false`) the four-layer domain had no single `Fact` for, so
        // the rule could not state its own answer. `Fact::Union` is that form now,
        // and what is missing is the rule itself — `array_search` over a witnessed
        // array with a proven needle is exactly computable, and over a shape it is
        // the key union ∪ false. Unwritten, not unsayable.
        //
        // The pattern is the one `array_slice` set: its v1 decline — "the seam is
        // single-argument by construction" — was answered by growing the seam, not
        // by weakening the rule (ADR-0062 Amendment B).
        _ => return None,
    };
    // ADR-0064 Amendment B, enforced structurally: a `mixed` declaration pin is
    // inadmissible on its own, so every arm declaring it carries an arity pin.
    debug_assert!(
        !declared.iter().any(|d| d.eq_ignore_ascii_case("mixed")) || arity.is_some(),
        "{lower}: a `mixed` declaration pin requires the arity second leg"
    );
    transfer_declaration_admits(cx, folder, name, declared, arity).then_some(out)
}

/// **`array_slice` on the ORDER-WITNESSED lane** (ADR-0062 §2), the exact rung of
/// issue #118's two.
///
/// The subject's fact is a `Singleton(Val::Array)`: a real array whose entries sit
/// in true insertion order, built by observing the construction (a literal, a
/// write, ADR-0001 call-site propagation). Order-dependent results are sound *here
/// and only here*, so this is the one place the projection may be **executed**
/// rather than widened — the same privilege the fold seam exercises when it hands
/// a written literal to the real engine, reached natively because a fold result is
/// scalar-only and this one is an array.
///
/// The window arithmetic is php-src's, probed verbatim at `PINNED_PHP` (8.5.8):
///
/// ```text
/// start = offset < 0 ? max(0, n + offset) : min(offset, n)
/// end   = length is null ? n
///       : length < 0    ? max(start, n + length)
///       :                 min(start + length, n)
/// ```
///
/// with witnesses `array_slice([1,2,3,4,5], 1, -1) === [2,3,4]`,
/// `array_slice([1,2,3,4,5], 1, -10) === []`,
/// `array_slice([1,2,3,4,5], -10) === [1,2,3,4,5]` and
/// `array_slice([1,2,3], -1, -1) === []`.
///
/// Keys follow the same probes: `$preserve_keys = false` (the default) renumbers
/// **integer** keys `0..` in the surviving order and leaves **string** keys alone
/// (`array_slice(['a' => 1, 5 => 2, 'b' => 3, 9 => 4], 1) === [0 => 2, 'b' => 3,
/// 1 => 4]`), while `true` keeps every key as it was.
///
/// **Three declines, and every one of them falls to the widening rather than to
/// silence**: an offset or length that is not a `Singleton` int, a `$preserve_keys`
/// that is not a literal bool, and any other name in the family. The shape the
/// widening then reads is [`ShapeFact::lift`] of the same entries — which is
/// exactly where order-witnessed-ness is honestly lost — so a value-lane subject is
/// never *worse* off than a declared one.
pub(crate) fn witnessed_projection_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    entries: &[(VKey, Val)],
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    /// The one name this lane answers for; see [`shape_projection_fact`]'s own
    /// `array_slice` arm for the widening the others take.
    const ARRAY: &[&str] = &["array"];
    const ARITY_SLICE: Option<(u32, u32)> = Some((4, 2));

    if !name.eq_ignore_ascii_case("array_slice") || !(2..=4).contains(&args.len()) {
        return None;
    }
    if !transfer_declaration_admits(cx, folder, name, ARRAY, ARITY_SLICE) {
        return None;
    }
    match slice_window(cx, folder, args, env, store) {
        Some((offset, length, preserve)) => {
            Some(Fact::Singleton(Val::Array(slice_entries(entries, offset, length, preserve))))
        }
        // The widening reads the LIFT of the same entries — which is exactly where
        // order-witnessed-ness is honestly lost — so a value-lane subject is never
        // worse off than a declared one.
        None => slice_widening(cx, folder, &ShapeFact::lift(entries), args, env, store),
    }
}

/// **The positional projections on the order-witnessed lane** (issue #328) —
/// `array_keys`, `array_values`, `array_reverse`, `array_flip` executed over a
/// subject whose construction order was observed, rather than widened to the
/// order-blind answer a key set deserves.
///
/// # What "order-witnessed" buys, and why it is not the declined import
///
/// ADR-0062 §7 declines *declaration*-order trust in positional projections —
/// phpstan/phpstan#14940's false-positive class, where `array{b: …, a: …}`'s
/// field order is read as runtime order even though the shape admits both
/// insertion orders. Nothing here reads a declaration. The entries arrive in
/// the order the walk *saw the array built*, carried by
/// [`ShapeFact::witnessed_order`] (issue #327), and every admitted value of a
/// sealed all-required witnessed shape has exactly that key sequence. Consuming
/// it is the same move issue #165 made for `isList == Yes`: an order that is a
/// semantic guarantee, never an artifact.
///
/// # The entries, and why the values may be unknown
///
/// `entries` is the witnessed sequence with each slot's fact, `None` where
/// nothing proved one. Three of the four names do not care: they restructure
/// the array, and a slot travels through them unread. That is what lets
/// `array_keys(['a' => $x, 'b' => $y])` answer the exact key sequence — the
/// result's *values* are the subject's *keys*, which are known by construction
/// however little is known about `$x`.
///
/// `array_flip` is the exception and declines instead: the result's *keys* come
/// from the subject's *values*, so an unproven value is an unproven key and
/// there is no honest partial answer. It falls to the widening the shape rung
/// already computes.
///
/// # Probes (PHP 8.5.9), which is where each rule comes from
///
/// * `array_keys(['b' => 1, 'a' => 2]) === [0 => 'b', 1 => 'a']` — the key
///   sequence, reindexed. `array_keys([-5 => 1, 3 => 2]) === [-5, 3]`.
/// * `array_values(['b' => 1, 'a' => 2]) === [0 => 1, 1 => 2]`.
/// * `array_reverse(['a' => 1, 5 => 2, 'b' => 3, 9 => 4]) === [0 => 4,
///   'b' => 3, 1 => 2, 'a' => 1]` — reversed, string keys surviving, integer
///   keys renumbered `0..` **in the new order**. The same rule
///   [`slice_entries`] already implements and probes.
/// * `array_flip(['a', 'b']) === ['a' => 0, 'b' => 1]`;
///   `array_flip(['x' => '1']) === [1 => 'x']` (the value goes through PHP's
///   own key normalization); `array_flip(['a', 'a']) === ['a' => 1]`
///   (last wins); `array_flip(['a', 1.5, 'b']) === ['a' => 0, 'b' => 2]` — a
///   value that is not `int|string` is **skipped**, with a warning, and the
///   survivors keep their original positions as values.
///
/// # The admission gate, and the result's own layer
///
/// The gate is [`transfer_declaration_admits`] over the engine's own `array`
/// declaration, exactly as the shape rung's arm of this family uses. The result
/// is a `Singleton` when every slot it needed was proven — for `array_keys`
/// that is always — and a witnessed `Fact::Shape` otherwise, so an unknown
/// element costs one slot rather than the sequence.
pub(crate) fn witnessed_family_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    entries: &[(VKey, Option<Fact>)],
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    const ARRAY: &[&str] = &["array"];
    /// `array_key_first`/`array_key_last`'s declared return. PHP 8 renders the
    /// union in its own order; both spellings are accepted so the gate tests the
    /// *declaration* rather than the engine's spelling of it.
    const KEY_OR_NULL: &[&str] = &["string|int|null", "int|string|null"];
    /// `array_first`/`array_last` declare a bare `mixed`, which pins nothing on
    /// its own, so they carry the arity second leg (ADR-0064 Amendment B).
    const MIXED: &[&str] = &["mixed"];
    const ARITY_1: Option<(u32, u32)> = Some((1, 1));
    const ARITY_SLICE: Option<(u32, u32)> = Some((4, 2));

    let lower = name.to_ascii_lowercase();

    // ---- The POSITION READERS (issue #328 wave 2) ---------------------------
    //
    // ADR-0062 §4 answers these from the key *set* — "SOME key of the set, never
    // the declared-first one". A witnessed order is the other provenance: the
    // sequence was observed, so first really is first.
    //
    // Probed at 8.5.9: `array_key_first(['b' => 1, 'a' => 2]) === 'b'`,
    // `array_key_last(…) === 'a'`, `array_first(…) === 1`, `array_last(…) === 2`,
    // and all four answer `null` on `[]`.
    //
    // **The pointer family is deliberately excluded.** `key`/`current`/`reset`/
    // `end` read the internal array pointer, which Steins does not model; the
    // existing arm tolerates that only because a shape-derived fact can never
    // premise a proof-layer finding (A-G9's corollary). A witnessed literal is
    // `Verified`, so an exact answer here would ride the pointer assumption into
    // a proof with it — they keep the widening.
    let position: Option<GatedFact<'_>> = match lower.as_str() {
        "array_key_first" | "array_key_last" | "array_first" | "array_last"
            if args.len() == 1 =>
        {
            let last = lower.ends_with("last");
            let entry = if last { entries.last() } else { entries.first() };
            let fact = match entry {
                // PHP answers `null` for the empty array, and the empty array is
                // exactly what an empty witnessed sequence proves.
                None => Fact::Singleton(Val::Null),
                Some((k, slot)) => {
                    if lower.starts_with("array_key") {
                        Fact::Singleton(val_of_key(k))
                    } else {
                        // The value's own fact at whatever layer it was proven —
                        // an unknown slot has no answer, so the arm declines to
                        // the widening rather than claiming `mixed`.
                        slot.clone()?
                    }
                }
            };
            let (declared, arity): (&[&str], Option<(u32, u32)>) =
                if lower.starts_with("array_key") { (KEY_OR_NULL, None) } else { (MIXED, ARITY_1) };
            Some((fact, declared, arity))
        }
        _ => None,
    };
    if let Some((fact, declared, arity)) = position {
        return transfer_declaration_admits(cx, folder, name, declared, arity).then_some(fact);
    }

    // ---- `array_slice` on the witnessed lane, with unknown slots -------------
    //
    // The exact slice already existed for a fully-proven `Val::Array`. It reads
    // offsets and keys and never a value, so `array_slice(['x', $s, 'z'], 1)` is
    // `list{string, 'z'}`, which the value-only rung could not say.
    if lower == "array_slice" && (2..=4).contains(&args.len()) {
        let (offset, length, preserve) = slice_window(cx, folder, args, env, store)?;
        let out = slice_witnessed_entries(entries, offset, length, preserve);
        return transfer_declaration_admits(cx, folder, name, ARRAY, ARITY_SLICE)
            .then(|| witnessed_entries_fact(&out));
    }

    // ---- The TWO-ARRAY names (issue #328 wave 2) ----------------------------
    //
    // Each reads a second witnessed sequence through the same seam the subject
    // came through. All four are pure key work — none inspects a value except to
    // *cast it to a key*, and that cast is measured below rather than recalled.
    if matches!(
        lower.as_str(),
        "array_fill_keys" | "array_combine" | "array_diff_key" | "array_intersect_key"
    ) {
        let [_, second] = args else { return None };
        let out: Vec<(VKey, Option<Fact>)> = match lower.as_str() {
            // `array_fill_keys($keys, $value)`: every value of `$keys` becomes a
            // key, all mapped to the same `$value`. Probed:
            // `array_fill_keys(['1', 2], 'v') === [1 => 'v', 2 => 'v']`,
            // `array_fill_keys(['a', 'a'], 1) === ['a' => 1]`. Its second argument
            // is a plain VALUE, not a sequence, so it is read per-arm.
            "array_fill_keys" => {
                let fill = transfer_arg_fact(cx, folder, second, env, store);
                let mut out: Vec<(VKey, Option<Fact>)> = Vec::with_capacity(entries.len());
                for (_, slot) in entries {
                    let Some(Fact::Singleton(v)) = slot else { return None };
                    let key = array_key_cast(v)?;
                    match out.iter_mut().find(|(ek, _)| *ek == key) {
                        Some(e) => e.1 = fill.clone(),
                        None => out.push((key, fill.clone())),
                    }
                }
                out
            }
            // `array_combine($keys, $values)`: positional zip. PHP raises a
            // `ValueError` on a length mismatch (probed), so a mismatch is a call
            // that does not return at all — no fact, rather than a guessed one.
            // Probed: `array_combine(['1', 'b'], [1, 2]) === [1 => 1, 'b' => 2]`,
            // `array_combine(['a', 'a'], [1, 2]) === ['a' => 2]` (last wins).
            "array_combine" => {
                let other = witnessed_entries_of(cx, folder, second, env, store)?;
                if entries.len() != other.len() {
                    return None;
                }
                let mut out: Vec<(VKey, Option<Fact>)> = Vec::with_capacity(entries.len());
                for ((_, kslot), (_, vslot)) in entries.iter().zip(other.iter()) {
                    let Some(Fact::Singleton(v)) = kslot else { return None };
                    let key = array_key_cast(v)?;
                    match out.iter_mut().find(|(ek, _)| *ek == key) {
                        Some(e) => e.1 = vslot.clone(),
                        None => out.push((key, vslot.clone())),
                    }
                }
                out
            }
            // `array_diff_key` / `array_intersect_key`: pure key set difference and
            // intersection, and **the order comes from the first array** (probed:
            // `array_intersect_key(['b' => 2, 'a' => 1], ['a' => 9, 'b' => 8])
            //    === ['b' => 2, 'a' => 1]`). Key identity is the domain's own
            // normalized `VKey` (`array_diff_key([5 => 1, '5x' => 2], ['5' => 9])
            // === ['5x' => 2]`).
            //
            // Values are never read, so unknown slots cost nothing. The second
            // argument contributes only its **key set**, so unlike `array_combine`
            // it may be a *declared* shape — reading a declaration's key set is
            // not the §7 declined import (order, not set). The set must be
            // certain, which is why [`key_set_of`] insists on a sealed tail and no
            // optional field.
            other_name => {
                let other = key_set_of(cx, folder, second, env, store)?;
                let want = other_name == "array_intersect_key";
                entries
                    .iter()
                    .filter(|(k, _)| other.contains(k) == want)
                    .cloned()
                    .collect()
            }
        };
        return transfer_declaration_admits(cx, folder, name, ARRAY, None)
            .then(|| witnessed_entries_fact(&out));
    }

    // Every name below reads exactly one argument, and a call passing more is a
    // different function than the rule describes (`array_reverse($x, true)`
    // preserves keys; `array_keys($x, $search)` filters by value).
    let [_] = args else { return None };
    let out: Vec<(VKey, Option<Fact>)> = match lower.as_str() {
        // The keys, as values, reindexed `0..`. Always fully proven: the result's
        // values are the subject's keys.
        "array_keys" => entries
            .iter()
            .enumerate()
            .map(|(i, (k, _))| {
                (VKey::Int(i64::try_from(i).unwrap_or(i64::MAX)), Some(Fact::Singleton(val_of_key(k))))
            })
            .collect(),
        // The values, reindexed `0..`. The slots travel unread, unknowns included.
        "array_values" => entries
            .iter()
            .enumerate()
            .map(|(i, (_, slot))| {
                (VKey::Int(i64::try_from(i).unwrap_or(i64::MAX)), slot.clone())
            })
            .collect(),
        // Reversed: string keys survive where they are, integer keys are
        // renumbered `0..` in the NEW order.
        "array_reverse" => {
            let mut next = 0i64;
            entries
                .iter()
                .rev()
                .map(|(k, slot)| {
                    let key = match k {
                        VKey::Str(s) => VKey::Str(s.clone()),
                        VKey::Int(_) => {
                            let at = next;
                            next = next.saturating_add(1);
                            VKey::Int(at)
                        }
                    };
                    (key, slot.clone())
                })
                .collect()
        }
        // Keys and values swap, so every value has to be proven — and to be an
        // `int|string`, or PHP skips that entry entirely.
        "array_flip" => {
            let mut out: Vec<(VKey, Option<Fact>)> = Vec::with_capacity(entries.len());
            for (k, slot) in entries {
                let Some(Fact::Singleton(v)) = slot else { return None };
                let Some(key) = flip_key_of(v) else { continue };
                let value = Some(Fact::Singleton(val_of_key(k)));
                // Last wins, in place — PHP overwrites the entry without moving it.
                match out.iter_mut().find(|(ek, _)| *ek == key) {
                    Some(slot) => slot.1 = value,
                    None => out.push((key, value)),
                }
            }
            out
        }
        _ => return None,
    };
    if !transfer_declaration_admits(cx, folder, name, ARRAY, None) {
        return None;
    }
    Some(witnessed_entries_fact(&out))
}

/// The array key PHP casts a value to for `array_fill_keys` / `array_combine`
/// (issue #328 wave 2), or `None` where this crate declines to say.
///
/// # Three sibling functions, three different casts — measured, not recalled
///
/// At 8.5.9, for the value `1.5`:
///
/// | seam | answer |
/// | --- | --- |
/// | `$a[1.5] = v` ([`offset_key_of`]) | int `1` — truncation, with a deprecation |
/// | `array_fill_keys([1.5], v)` / `array_combine([1.5], [v])` | string `'1.5'` |
/// | `array_flip([1.5])` ([`flip_key_of`]) | the entry is **skipped** |
///
/// No amount of reasoning about "PHP's array key cast" produces these — only
/// running the engine does (ADR-0004). This cast serves exactly the pair probed.
///
/// **The float declines** rather than taking the measured `'1.5'`. PHP renders a
/// float to string under the `precision` ini directive, so the *key* of
/// `array_fill_keys([0.1 + 0.2], v)` depends on the runtime's configuration —
/// the same reason [`concat_cast`] excludes floats. A key this crate cannot
/// state without knowing an ini setting is a key it does not state.
///
/// The rest are measured and fixed: `array_fill_keys(['1', 2], 'v')
/// === [1 => 'v', 2 => 'v']` (a numeric string normalizes, `'01'` does not),
/// `array_fill_keys([true, null], 'v') === [1 => 'v', '' => 'v']`.
///
/// [`offset_key_of`]: crate::offsets::offset_key_of
/// [`concat_cast`]: crate::concat_cast
fn array_key_cast(v: &Val) -> Option<VKey> {
    match v {
        Val::Int(i) => Some(VKey::Int(*i)),
        Val::Bool(b) => Some(VKey::Int(i64::from(*b))),
        Val::Null => Some(VKey::Str(PhpStr::new())),
        Val::Str(s) => Some(match php_canonical_int_string(s) {
            Some(i) => VKey::Int(i),
            None => VKey::Str(s.clone()),
        }),
        // The float's ini-dependent rendering, and the array (a `TypeError`).
        Val::Float(_) | Val::Array(_) => None,
    }
}

/// The witnessed entry sequence an argument denotes (issue #328 wave 2) — the
/// one seam the two-array names read their sibling through.
///
/// Answers for the same two provenances the subject binding accepts: a
/// `Singleton` array is an observed value, a sealed all-required shape carrying
/// an order witness is an observed construction. A declared shape has no
/// sequence and answers `None` — keeping the §7 declined import declined on the
/// *second* argument too.
fn witnessed_entries_of(
    cx: &Cx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Vec<(VKey, Option<Fact>)>> {
    match transfer_arg_fact(cx, folder, arg, env, store)? {
        Fact::Singleton(Val::Array(entries)) => Some(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), Some(Fact::Singleton(v.clone()))))
                .collect(),
        ),
        Fact::Shape { shape, nullable: false } => {
            let order = shape.witnessed_order()?;
            let out: Vec<(VKey, Option<Fact>)> = order
                .iter()
                .filter_map(|k| {
                    shape.field(k).map(|(_, _, slot)| (k.clone(), slot.clone().map(|f| *f)))
                })
                .collect();
            (out.len() == order.len()).then_some(out)
        }
        _ => None,
    }
}

/// A rule's answer together with the admission gate it must pass: the reflected
/// return declarations that count as the signature it was written against, and
/// the arity pin where the declaration alone pins too little (ADR-0064
/// Amendment B).
type GatedFact<'a> = (Fact, &'a [&'a str], Option<(u32, u32)>);

/// The **certain key set** of an argument (issue #328 wave 2) — every key it
/// has, and no key it might not have.
///
/// Weaker than [`witnessed_entries_of`]: no order witness required, none
/// returned. `array_diff_key`/`array_intersect_key` read their second argument's
/// key set only, so a *declared* `array{a: int, b: int}` is a perfectly good
/// second argument even though its field order means nothing (§7's declined
/// import is about reading order, and there is none here).
///
/// The set has to be certain, which is what the two refusals enforce:
///
/// * an **optional** field — a key that may or may not be present decides
///   neither the difference nor the intersection;
/// * an **unsealed** tail — an undeclared key could be anything, so the set is
///   a lower bound rather than the set.
fn key_set_of(
    cx: &Cx,
    folder: &mut dyn Folder,
    arg: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Vec<VKey>> {
    use steins_domain::Tail;
    match transfer_arg_fact(cx, folder, arg, env, store)? {
        Fact::Singleton(Val::Array(entries)) => {
            Some(entries.iter().map(|(k, _)| k.clone()).collect())
        }
        Fact::Shape { shape, nullable: false } => {
            let certain = matches!(shape.tail, Tail::Sealed)
                && shape.fields.iter().all(|(_, p, _)| p.is_required());
            certain.then(|| shape.fields.iter().map(|(k, _, _)| k.clone()).collect())
        }
        _ => None,
    }
}

/// The array key PHP casts a **flipped value** to, or `None` for a value
/// `array_flip` skips (probed: `array_flip(['a', 1.5, 'b'])` drops the float,
/// with a warning, and keeps the survivors' positions).
///
/// Only `int` and `string` flip. The string goes through the same canonical
/// int-string normalization every other key seam uses, so `'1'` becomes the
/// integer key `1` (probed: `array_flip(['x' => '1']) === [1 => 'x']`) while
/// `'01'` stays a string.
fn flip_key_of(v: &Val) -> Option<VKey> {
    match v {
        Val::Int(i) => Some(VKey::Int(*i)),
        Val::Str(s) => Some(match php_canonical_int_string(s) {
            Some(i) => VKey::Int(i),
            None => VKey::Str(s.clone()),
        }),
        Val::Bool(_) | Val::Null | Val::Float(_) | Val::Array(_) => None,
    }
}

/// The fact a witnessed entry sequence denotes: a `Singleton` when every slot is
/// a proven value — the most precise thing the domain has, and what the value
/// lane exists to produce — and a witnessed `Fact::Shape` otherwise.
fn witnessed_entries_fact(entries: &[(VKey, Option<Fact>)]) -> Fact {
    let proven: Option<Vec<(VKey, Val)>> = entries
        .iter()
        .map(|(k, slot)| match slot {
            Some(Fact::Singleton(v)) => Some((k.clone(), v.clone())),
            _ => None,
        })
        .collect();
    match proven {
        Some(vals) => Fact::Singleton(Val::Array(vals)),
        None => shape_fact(ShapeFact::from_witnessed_entries(entries)),
    }
}

/// The exact rung's three premises, read together: a `Singleton` offset, a
/// `Singleton`-or-absent length, and a literal `$preserve_keys`. `None` — the
/// caller takes the widening — as soon as one of them is not proven.
fn slice_window(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(i64, Option<i64>, bool)> {
    let Some(Fact::Singleton(Val::Int(offset))) = transfer_arg_fact(cx, folder, &args[1], env, store)
    else {
        return None;
    };
    let length = match args.get(2) {
        None => None,
        Some(a) => match transfer_arg_fact(cx, folder, a, env, store)? {
            // `$length = null` is the documented "to the end" spelling, and the
            // parameter's own default.
            Fact::Singleton(Val::Null) => None,
            Fact::Singleton(Val::Int(l)) => Some(l),
            _ => return None,
        },
    };
    let preserve = match slice_preserve_keys(cx, folder, args, env, store) {
        PreserveKeys::No => false,
        PreserveKeys::Yes => true,
        PreserveKeys::Unknown => return None,
    };
    Some((offset, length, preserve))
}

/// The witnessed window of `entries`, keyed as PHP keys it (see
/// [`witnessed_projection_fact`] for the probes both halves are written from).
/// [`slice_entries`] over a witnessed sequence whose values may be unknown
/// (issue #328 wave 2).
///
/// The window arithmetic and the key rule are the same computation on the same
/// probes — `array_slice` reads offsets and keys and never a value, which is
/// precisely why the slots can travel through it unread. The two functions are
/// kept separate rather than generified because one carries `Val` and the other
/// `Option<Fact>`; `slice_agrees_with_its_value_only_twin` pins that they answer
/// alike wherever both apply.
fn slice_witnessed_entries(
    entries: &[(VKey, Option<Fact>)],
    offset: i64,
    length: Option<i64>,
    preserve: bool,
) -> Vec<(VKey, Option<Fact>)> {
    let n = i64::try_from(entries.len()).unwrap_or(i64::MAX);
    let start = if offset < 0 { (n.saturating_add(offset)).max(0) } else { offset.min(n) };
    let end = match length {
        None => n,
        Some(l) if l < 0 => n.saturating_add(l).max(start),
        Some(l) => start.saturating_add(l).min(n),
    };
    let (lo, hi) = (usize::try_from(start).unwrap_or(0), usize::try_from(end).unwrap_or(0));
    let mut next = 0i64;
    entries[lo.min(entries.len())..hi.min(entries.len())]
        .iter()
        .map(|(k, slot)| match k {
            VKey::Str(_) => (k.clone(), slot.clone()),
            VKey::Int(_) if preserve => (k.clone(), slot.clone()),
            VKey::Int(_) => {
                let key = VKey::Int(next);
                next += 1;
                (key, slot.clone())
            }
        })
        .collect()
}

fn slice_entries(
    entries: &[(VKey, Val)],
    offset: i64,
    length: Option<i64>,
    preserve: bool,
) -> Vec<(VKey, Val)> {
    let n = i64::try_from(entries.len()).unwrap_or(i64::MAX);
    let start = if offset < 0 { (n.saturating_add(offset)).max(0) } else { offset.min(n) };
    let end = match length {
        None => n,
        Some(l) if l < 0 => n.saturating_add(l).max(start),
        Some(l) => start.saturating_add(l).min(n),
    };
    // `0 <= start <= end <= n` holds by the three arms above, so both casts are
    // in range; the fallback keeps the function total rather than trusting that.
    let (lo, hi) = (usize::try_from(start).unwrap_or(0), usize::try_from(end).unwrap_or(0));
    let mut next = 0i64;
    entries[lo.min(entries.len())..hi.min(entries.len())]
        .iter()
        .map(|(k, v)| match k {
            VKey::Str(_) => (k.clone(), v.clone()),
            VKey::Int(_) if preserve => (k.clone(), v.clone()),
            VKey::Int(_) => {
                let key = VKey::Int(next);
                next += 1;
                (key, v.clone())
            }
        })
        .collect()
}

/// What the call says about `array_slice`'s fourth argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreserveKeys {
    /// Absent, or a literal `false` — the reindexing default.
    No,
    /// A literal `true`.
    Yes,
    /// Present but not a literal bool. Every rule here takes the join of the two
    /// branches, which is the honest widening.
    Unknown,
}

/// Read `$preserve_keys` off the call. A non-literal (including a truthy `int`
/// that weak mode would coerce) is [`PreserveKeys::Unknown`], never guessed.
fn slice_preserve_keys(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> PreserveKeys {
    let Some(arg) = args.get(3) else { return PreserveKeys::No };
    match transfer_arg_fact(cx, folder, arg, env, store) {
        Some(Fact::Singleton(Val::Bool(false))) => PreserveKeys::No,
        Some(Fact::Singleton(Val::Bool(true))) => PreserveKeys::Yes,
        _ => PreserveKeys::Unknown,
    }
}

/// **`array_slice` on the ORDER-DECLARED lane** — the widening floor, sound for
/// *any* offset and length (issue #118, ADR-0062 Amendment B).
///
/// The v1 decline's stated cost was "the shape-only answer carries no more than
/// the reflected `array` envelope already does" — false: `array_slice(list<Foo>,
/// $n)` is a `list<Foo>`, and the envelope says `array`. Six claims, each read
/// from order-INDEPENDENT structure only (§2 — no field declaration order
/// consulted, only a flag, an offset, and a length):
///
/// * **Element bound** — the slice's values are a subset of the subject's, so
///   [`shape_value_union`] carries across unchanged.
/// * **Key class** — a slice never *invents* a key class. `$preserve_keys = true`
///   keeps each surviving key as it was; `false` renumbers integer keys and leaves
///   string keys alone (probe: `array_slice(['a' => 1, 5 => 2], 0) ===
///   ['a' => 1, 0 => 2]`). Either way an all-int subject yields all-int keys and an
///   all-string subject all-string ones, so the class is the subject's own.
/// * **List-ness survives under an absent-or-false flag.** An all-integer-keyed
///   subject sliced with `$preserve_keys` absent-or-false is renumbered `0..n-1`,
///   which *is* a list. Under a truthy — or merely *unknown* — flag it degrades to
///   `Maybe` (`array_slice([1,2,3], 1, null, true) === [1 => 2, 2 => 3]`), and so
///   does any subject that can carry a string key. Never `No`: the empty array a
///   slice can always return is itself a list.
/// * **List-ness survives `preserve_keys = true` from offset 0** (issue #137's
///   first claim). When the subject's shape PROVES `is_list` and the offset is a
///   proven int `0`, a literal `true` keeps the surviving keys `0..k-1` unchanged
///   — still a list, for any length sign (probes:
///   `array_slice([1,2,3], 0, null, true) === [1,2,3]`,
///   `array_slice([1,2,3], 0, -1, true) === [0 => 1, 1 => 2]`,
///   `array_slice([1,2,3], 0, 0, true) === []`). The subject's proven list-ness
///   is load-bearing — all-int keys alone are NOT enough:
///   `array_slice([5 => 2], 0, null, true) === [5 => 2]`, not a list. An unknown
///   offset, like a non-list subject, stays `Maybe` exactly as before.
/// * **A proven zero length is `array{}`** (issue #137's second claim). When the
///   `$length` argument is a proven int `0`, the window is empty for ANY subject,
///   offset, and flag — `array_slice(['a' => 1], 0, 0) === []`,
///   `array_slice([1,2,3], -2, 0, true) === []` — so the answer is the SEALED
///   empty shape rather than this floor's unsealed tail. Literal int `0` only:
///   `null` is the documented "to the end" spelling and must not match, and
///   anything short of a proven `Singleton` int declines to the floor. Taken
///   before every other claim — `array{}` is the sharper fact and is itself a
///   list, so it subsumes the preserved-prefix claim when both apply.
/// * **`non_empty` NEVER survives.** Every possibly-empty result is reachable from
///   a non-empty subject — `array_slice([1,2,3], 10) === []`,
///   `array_slice([1,2,3], 1, 0) === []` — so the flag is dropped unconditionally.
///
/// **The size bound is deliberately not claimed.** Expressing it would need a
/// sealed result shape with keys the projection cannot name; the tail is
/// unsealed instead, the sound direction. Left optional — the widening is worth
/// more than the arithmetic.
fn slice_widening(
    cx: &Cx,
    folder: &mut dyn Folder,
    shape: &ShapeFact,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    use steins_domain::{KeyClass, Tail};
    // Two required, four total (`ARITY_SLICE`) — a call PHP itself rejects with an
    // `ArgumentCountError` has no return value for the rule to describe.
    if !(2..=4).contains(&args.len()) {
        return None;
    }
    // The zero-length claim first: a proven `$length = 0` empties the window for
    // any subject, offset, and flag, and the sealed empty shape it answers is
    // sharper than anything the unsealed floor below could say.
    if slice_arg_is_int_zero(cx, folder, args.get(2), env, store) {
        return Some(shape_fact(ShapeFact::normalize(
            Vec::new(),
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        )));
    }
    let preserve = slice_preserve_keys(cx, folder, args, env, store);
    let (all_int, all_str) = shape_key_classes(shape);
    let key = if all_int {
        KeyClass::Int
    } else if all_str {
        KeyClass::Str
    } else {
        KeyClass::ArrayKey
    };
    // The two combinations list-ness survives: renumbering (absent-or-false flag
    // over all-int keys) and the preserved prefix (a PROVEN list kept from a
    // proven offset 0 under a literal `true`).
    let renumbered = all_int && preserve == PreserveKeys::No;
    let preserved_prefix = shape.is_list == Certainty::Yes
        && preserve == PreserveKeys::Yes
        && slice_arg_is_int_zero(cx, folder, args.get(1), env, store);
    let is_list =
        if renumbered || preserved_prefix { Certainty::Yes } else { Certainty::Maybe };
    Some(shape_fact(ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key, value: shape_value_union(shape).map(Box::new) },
        is_list,
        false,
        Vec::new(),
    )))
}

/// Is this argument a proven int `0`? The two literal reads issue #137's precision
/// claims added share the answer — the offset for the preserved-prefix claim, the
/// length for the zero-length claim — and both take [`transfer_arg_fact`], the same
/// resolution [`slice_window`] and [`slice_preserve_keys`] use. An absent argument
/// is `false`, and so is `Singleton(Val::Null)` (`$length = null` means "to the
/// end"): nothing short of a proven `Singleton` int `0` matches.
fn slice_arg_is_int_zero(
    cx: &Cx,
    folder: &mut dyn Folder,
    arg: Option<&ArgValue>,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> bool {
    arg.is_some_and(|a| {
        matches!(transfer_arg_fact(cx, folder, a, env, store), Some(Fact::Singleton(Val::Int(0))))
    })
}

/// `(every key is an int, every key is a string)` over the shape's declared,
/// non-`Absent` keys **and** its tail class. A `Sealed` tail contributes nothing
/// (it admits no undeclared key), so a sealed shape answers from its fields alone
/// and `array{}` answers `(true, true)` vacuously — which is right: the only array
/// it admits is `[]`.
fn shape_key_classes(shape: &ShapeFact) -> (bool, bool) {
    use steins_domain::{KeyClass, Presence, Tail};
    let declared = |want: fn(&VKey) -> bool| {
        shape.fields.iter().all(|(k, p, _)| matches!(p, Presence::Absent) || want(k))
    };
    let tail = |want: KeyClass| match &shape.tail {
        Tail::Sealed => true,
        Tail::Unsealed { key, .. } => *key == want,
    };
    (
        declared(|k| matches!(k, VKey::Int(_))) && tail(KeyClass::Int),
        declared(|k| matches!(k, VKey::Str(_))) && tail(KeyClass::Str),
    )
}

/// One entry of an admitted array, read by position: the shape's value union, plus
/// `empty` when the shape does not prove the array is non-empty.
///
/// `empty` is the builtin's own empty-array answer — [`Val::Null`] for the
/// `array_pop`/`array_first` half of the family, `false` for the pointer half. A
/// union the addition makes unspellable (`int|false` is two bases) declines.
///
/// A shape that admits **only** `[]` (a sealed tail with no present field) answers
/// with the empty-array value exactly: `current(array{})` is `false`, and
/// `array_first(array{})` is `null`. That case has to be taken before the value
/// union, which is `None` there for the uninformative reason — an empty join, not
/// an unrepresentable one.
fn read_position_value(shape: &ShapeFact, empty: Val) -> Option<Fact> {
    if !shape.can_be_non_empty() {
        return Some(Fact::Singleton(empty));
    }
    let values = shape_value_union(shape)?;
    if shape.non_empty {
        return Some(values);
    }
    match empty {
        Val::Null => fact_admitting_null(&values),
        other => values.join(&Fact::Singleton(other)),
    }
}

/// Add `false` to a fact's denotation. Unlike `null` there is no side-flag for it,
/// so this is the plain domain join: finite layers absorb it as another member, and
/// an abstract non-`bool` base joins into a [`Fact::Union`] (issue #339), where it
/// used to yield `None` for want of a two-base form. The result is `int|bool`
/// rather than `int|false`: `Bool` carries no refinement, so the finite `false`
/// widens to its base on the way into an arm (ADR-0085 §5).
fn fact_admitting_false(f: &Fact) -> Option<Fact> {
    f.join(&Fact::Singleton(Val::Bool(false)))
}

pub(crate) fn shape_fact(shape: ShapeFact) -> Fact {
    Fact::Shape { shape: Box::new(shape), nullable: false }
}

/// The fact **every value** of an admitted array satisfies: the join of every
/// non-`Absent` field's value slot with the tail's value bound. One unknown
/// contributor (or an unrepresentable join) yields `None` — the unknown floor,
/// which admits anything.
pub(crate) fn shape_value_union(shape: &ShapeFact) -> Option<Fact> {
    use steins_domain::{Presence, Tail};
    let mut acc: Option<Fact> = None;
    for (_, presence, slot) in &shape.fields {
        if matches!(presence, Presence::Absent) {
            continue;
        }
        acc = join_into(acc, slot.as_deref()?)?;
    }
    if let Tail::Unsealed { value, .. } = &shape.tail {
        acc = join_into(acc, value.as_deref()?)?;
    }
    acc
}

/// The fact **every key** of an admitted array satisfies. Under a `Sealed` tail
/// the key set is exactly the declared, non-`Absent` keys, so the answer is those
/// key literals (a `Singleton`/`OneOf`, or the domain's own computed widening past
/// [`steins_domain::CAP`] — never a hand-rolled degradation). An `Unsealed` tail
/// contributes its key class, joined with the declared keys; `array-key` (PHP's
/// `int|string`) is not a single-base fact, so it yields the unknown floor.
pub(crate) fn shape_key_union(shape: &ShapeFact) -> Option<Fact> {
    use steins_domain::{KeyClass, Presence, Tail};
    let declared: Vec<Val> = shape
        .fields
        .iter()
        .filter(|(_, p, _)| !matches!(p, Presence::Absent))
        .map(|(k, _, _)| val_of_key(k))
        .collect();
    match &shape.tail {
        Tail::Sealed => Fact::from_vals(declared),
        Tail::Unsealed { key, .. } => {
            let class = match key {
                KeyClass::Int => Fact::General { base: Base::Int, nullable: false },
                KeyClass::Str => Fact::General { base: Base::String, nullable: false },
                // `array-key` is PHP's `int|string`, and that is a fact now
                // (issue #339) where it used to be a two-base union with no
                // form — which is why this arm read `return None`.
                KeyClass::ArrayKey => Fact::union(
                    vec![(Base::Int, None), (Base::String, None)],
                    false,
                )?,
            };
            declared
                .iter()
                .try_fold(class, |acc, v| acc.join(&Fact::Singleton(v.clone())))
        }
    }
}

/// One shape field as the domain stores it — `steins_domain`'s own `Field`
/// alias, which is not exported.
type ShapeField = (VKey, steins_domain::Presence, Option<Box<Fact>>);

/// **The SEQUENCE lane's structural gate** (issue #165): the fields of a
/// *sealed* shape whose own `is_list` fact is `Yes`, verified to spell the
/// sequence the flag claims — keys exactly `0..n-1` (the field order is the
/// canonical [`VKey`] order, which for integer keys *is* the sequence order),
/// with every `Required` position before every `Optional` one.
///
/// `is_list == Yes` is **realizable order**: every admitted value passes
/// `array_is_list`, a semantic guarantee, not the declaration artifact
/// `docs/phpstan-divergences.md` records as PHPStan's real-FP class. Consuming
/// it is sound by the fact's own definition ([`ShapeFact::admits`]).
///
/// The structural verification is deliberate rather than assumed: `is_list` can
/// arrive from a guard on a shape whose declared key set does not cohere with it,
/// and a projection built from such fields would reason from positions no
/// admitted value has. Those shapes decline to the set widening instead.
fn sealed_list_sequence(shape: &ShapeFact) -> Option<&[ShapeField]> {
    use steins_domain::{Presence, Tail};
    if shape.is_list != Certainty::Yes || !matches!(shape.tail, Tail::Sealed) {
        return None;
    }
    let mut seen_optional = false;
    for (i, (k, p, _)) in shape.fields.iter().enumerate() {
        if *k != VKey::Int(i64::try_from(i).ok()?) {
            return None;
        }
        match p {
            Presence::Required { .. } if seen_optional => return None,
            Presence::Required { .. } => {}
            Presence::Optional => seen_optional = true,
            // Unreachable under a sealed tail (`normalize` strips `Absent`
            // there), kept so the gate never trusts that invariant.
            Presence::Absent => return None,
        }
    }
    Some(&shape.fields)
}

/// `array_values($x)`: a list of the value union. `non_empty` carries — the
/// projection preserves the entry count.
///
/// **The SEQUENCE lane** (issue #165): on a proven list the keys are already
/// `0..n-1` in realizable order, so the projection is the **identity** (probed:
/// `array_values(["x", 1]) === ["x", 1]`). No structural gate needed: identity
/// is exact for every value `is_list == Yes` admits, sealed or unsealed alike.
pub(crate) fn project_values(shape: &ShapeFact) -> ShapeFact {
    use steins_domain::{KeyClass, Tail};
    if shape.is_list == Certainty::Yes {
        return shape.clone();
    }
    ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key: KeyClass::Int, value: shape_value_union(shape).map(Box::new) },
        Certainty::Yes,
        shape.non_empty,
        Vec::new(),
    )
}

/// `array_keys($x)`: a list of the key union. `non_empty` carries.
///
/// **The SEQUENCE lane** (issue #165): a proven list's keys are `0..n-1` in
/// that order, so the key list is exact rather than a union —
///
/// * **sealed** (through [`sealed_list_sequence`]'s gate): the literal
///   sequence `list{0, 1, …}` (probed: `array_keys(["x", 1, 2.5]) ===
///   [0, 1, 2]`), each position carrying the subject position's own presence,
///   so a trailing-optional `list{A, 1?: B}` answers `list{0, 1?: 1}` —
///   exactly the two realizable key arrays `[0]` and `[0, 1]` (both probed);
/// * **unsealed** `list<T>`: `list<int<0, max>>`, sharper than the bare `int`
///   class because a list key is never negative.
pub(crate) fn project_keys(shape: &ShapeFact) -> ShapeFact {
    use steins_domain::{KeyClass, Tail};
    if let Some(fields) = sealed_list_sequence(shape) {
        let fields = fields
            .iter()
            .enumerate()
            .map(|(i, (_, p, _))| {
                let i = i64::try_from(i).expect("field width is bounded");
                (VKey::Int(i), *p, Some(Box::new(Fact::Singleton(Val::Int(i)))))
            })
            .collect();
        return ShapeFact::normalize(
            fields,
            Tail::Sealed,
            Certainty::Yes,
            shape.non_empty,
            Vec::new(),
        );
    }
    let value = if shape.is_list == Certainty::Yes {
        Some(Box::new(Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false)))
    } else {
        shape_key_union(shape).map(Box::new)
    };
    ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key: KeyClass::Int, value },
        Certainty::Yes,
        shape.non_empty,
        Vec::new(),
    )
}

/// `array_flip($x)`: the values become keys and the keys become values.
///
/// Two soundness points, both measured against the engine:
///
/// * **The result key class is `int` only when every value is an `int`.** A
///   *string* value does not give a string key: PHP's own array-key cast turns
///   `'5'` into `5` (`array_flip(['a' => '5']) === [5 => 'a']`), so the honest
///   class is `array-key`.
/// * **`non_empty` is dropped.** `array_flip` skips (with a warning) any entry
///   whose value is not `int|string`, so a non-empty input can flip to `[]`.
///
/// `is_list` is left to `normalize` (`Maybe`): whether the values happen to be
/// `0..n-1` is not something the shape knows.
pub(crate) fn project_flip(shape: &ShapeFact) -> ShapeFact {
    use steins_domain::Tail;
    ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed {
            key: flipped_key_class(shape),
            value: shape_key_union(shape).map(Box::new),
        },
        Certainty::Maybe,
        false,
        Vec::new(),
    )
}

/// The key class `array_flip`'s result has, read off the **array-key cast** of
/// the subject's value union (issue #336).
///
/// The cast is what decides this, not the base: a `decimal-int-string` value
/// produces an **integer** key, so `array_flip(list<decimal-int-string>)` is
/// keyed by `int`, which the previous all-int test could not see (it read the
/// values' base, and a string base is not an int).
///
/// Where the cast declines, the answer is a two-base union (`array-key`'s
/// territory, no sharper thing this slot can hold — [`KeyClass`] has three
/// values). Taken knowingly — see [`steins_domain::Fact::array_key_cast`].
/// The key class `array_fill_keys`'s result has (issue #336 piece 2): the
/// array-key cast of the subject's value union, since every value becomes a key.
///
/// Shares [`flipped_key_class`]'s reading of the cast and differs in what it
/// does with a value the cast declines — `array_flip` *skips* such an entry,
/// `array_fill_keys` *keeps* it under an unnamed cast, so the class falls to
/// `array-key` rather than the entry being lost.
fn filled_key_class(shape: &ShapeFact) -> steins_domain::KeyClass {
    use steins_domain::KeyClass;
    let Some(values) = shape_value_union(shape) else { return KeyClass::ArrayKey };
    match values.array_key_cast() {
        Some(Fact::General { base: Base::Int, .. } | Fact::Refined { base: Base::Int, .. }) => {
            KeyClass::Int
        }
        Some(Fact::General { base: Base::String, .. } | Fact::Refined { base: Base::String, .. }) => {
            KeyClass::Str
        }
        _ => KeyClass::ArrayKey,
    }
}

fn flipped_key_class(shape: &ShapeFact) -> steins_domain::KeyClass {
    use steins_domain::KeyClass;
    let Some(values) = shape_value_union(shape) else { return KeyClass::ArrayKey };
    // A finite value set casts key by key, which is exact where the abstract
    // rung declines: `array_flip(['a', '1'])` is keyed by `'a'` and `1`.
    if let Some(members) = values.finite_members() {
        let classes: Vec<KeyClass> = members
            .iter()
            .filter_map(|v| flip_key_of(v).as_ref().map(KeyClass::of_key))
            .collect();
        return match classes.split_first() {
            Some((first, rest)) if rest.iter().all(|c| c == first) => *first,
            _ => KeyClass::ArrayKey,
        };
    }
    match values.array_key_cast() {
        Some(Fact::General { base: Base::Int, .. } | Fact::Refined { base: Base::Int, .. }) => {
            KeyClass::Int
        }
        Some(Fact::General { base: Base::String, .. } | Fact::Refined { base: Base::String, .. }) => {
            KeyClass::Str
        }
        _ => KeyClass::ArrayKey,
    }
}

/// `array_reverse($x)` with the default `$preserve_keys = false`: **string keys
/// keep their keys, integer keys are renumbered `0..n-1`** (measured:
/// `array_reverse(['a' => 1, 5 => 2, 9 => 3]) === [0 => 3, 1 => 2, 'a' => 1]`).
///
/// The result's key SET is the input's string keys plus a fresh integer prefix,
/// so fields are dropped rather than carried, and `is_list` is a three-way read
/// of the input's key structure:
///
/// * **`Yes`** when no admitted array can carry a string key (a sealed tail whose
///   declared keys are all integers, or an `int`-classed unsealed tail with the
///   same): everything is renumbered, so the result is exactly `0..n-1`.
/// * **`No`** when some string key is `Required`: it survives into the result, and
///   an array with a string key is not a list.
/// * **`Maybe`** otherwise — the honest widening.
///
/// `non_empty` carries (reversal preserves the entry count).
///
/// **The SEQUENCE lane** (issue #165), for a sealed, **all-required** proven
/// list only: the result is the reversed sequence — position `i` takes the
/// subject's position `n-1-i` value slot (probed: `array_reverse(["a", "b",
/// "c"]) === ["c", "b", "a"]`). Any `Optional` key declines to the widening
/// below: a variable-length reversal smears every position (`"a"` lands at
/// index 0 in `array_reverse(["a"])` but index 1 in `array_reverse(["a", "b"])`),
/// so the positional claim is not statable.
pub(crate) fn project_reverse(shape: &ShapeFact) -> ShapeFact {
    use steins_domain::{KeyClass, Presence, Tail};
    if let Some(fields) = sealed_list_sequence(shape)
        && fields.iter().all(|(_, p, _)| p.is_required())
    {
        let rev = fields
            .iter()
            .rev()
            .enumerate()
            .map(|(i, (_, p, slot))| {
                (VKey::Int(i64::try_from(i).expect("field width is bounded")), *p, slot.clone())
            })
            .collect();
        return ShapeFact::normalize(rev, Tail::Sealed, Certainty::Yes, shape.non_empty, Vec::new());
    }
    let declared_ints =
        shape.fields.iter().all(|(k, p, _)| matches!(p, Presence::Absent) || matches!(k, VKey::Int(_)));
    let tail_ints = match &shape.tail {
        Tail::Sealed => true,
        Tail::Unsealed { key, .. } => *key == KeyClass::Int,
    };
    let all_int_keys = declared_ints && tail_ints;
    let required_str =
        shape.fields.iter().any(|(k, p, _)| p.is_required() && matches!(k, VKey::Str(_)));
    let is_list = if all_int_keys {
        Certainty::Yes
    } else if required_str {
        Certainty::No
    } else {
        Certainty::Maybe
    };
    ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed {
            key: if all_int_keys { KeyClass::Int } else { KeyClass::ArrayKey },
            value: shape_value_union(shape).map(Box::new),
        },
        is_list,
        shape.non_empty,
        Vec::new(),
    )
}

#[cfg(test)]
mod shape_projection_tests {
    //! ADR-0062 S7 — the positional projections over the order-DECLARED lane
    //! ([`shape_projection_fact`]'s helpers), tested as pure algebra.
    //!
    //! The headline is [`every_projection_admits_the_real_result`]: for every
    //! (shape, array) pair in the universe where the shape admits the array, the
    //! projected shape admits the array the real builtin produces. The reference
    //! results are the measured PHP semantics (`array_reverse` renumbers integer
    //! keys and keeps string ones; `array_flip` skips a non-`int|string` value),
    //! written out here rather than derived from the transfer under test.
    //!
    //! The second discipline is §2's rule: no transfer may read field
    //! declaration order. [`array_key_first_is_never_the_declared_first_key`] is
    //! its negative pin.
    use super::*;
    use crate::shape_projection::{
        project_flip, project_keys, project_reverse, project_values, shape_key_union,
    };
    use steins_domain::{Certainty, KeyClass, Presence, ShapeFact, Tail};

    fn ik(i: i64) -> VKey {
        VKey::Int(i)
    }

    fn sk(s: &str) -> VKey {
        VKey::Str(s.into())
    }

    fn req() -> Presence {
        Presence::Required { witnessed: false }
    }

    fn slot(f: Fact) -> Option<Box<Fact>> {
        Some(Box::new(f))
    }

    fn base_fact(base: Base) -> Fact {
        Fact::General { base, nullable: false }
    }

    /// `array{a: int, b?: string}` — the ADR's own fixture shape.
    fn declared_shape() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(base_fact(Base::Int))),
                (sk("b"), Presence::Optional, slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    /// `list<int>`: an int-classed unsealed tail, denotationally a list.
    fn list_of_int() -> ShapeFact {
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) },
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// `array<string, int>`.
    fn map_str_int() -> ShapeFact {
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Str, value: slot(base_fact(Base::Int)) },
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    /// `list{string, int}` — issue #165's measured-table subject: sealed,
    /// all-required, `is_list == Yes` surviving `normalize`'s sharpening.
    fn sealed_list_str_int() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::String))),
                (ik(1), req(), slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// `list{int, 1?: string}` — the trailing-optional sequence form.
    fn sealed_list_trailing_optional() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(1), Presence::Optional, slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// The concrete arrays the soundness sweep runs over, in *witnessed* order.
    fn arrays() -> Vec<Vec<(VKey, Val)>> {
        vec![
            vec![],
            vec![(ik(0), Val::Int(7))],
            vec![(ik(0), Val::Str("x".into())), (ik(1), Val::Str("y".into()))],
            vec![(ik(5), Val::Int(1)), (ik(9), Val::Int(2))],
            vec![(ik(1), Val::Int(2)), (ik(0), Val::Int(3))],
            vec![(sk("a"), Val::Int(1)), (sk("b"), Val::Str("x".into()))],
            vec![(sk("a"), Val::Int(1))],
            vec![(sk("b"), Val::Str("zz".into())), (sk("a"), Val::Int(4))],
            vec![(ik(0), Val::Int(1)), (sk("a"), Val::Int(2)), (ik(3), Val::Int(3))],
            vec![(ik(0), Val::Str("x".into())), (ik(1), Val::Int(1))],
        ]
    }

    fn shapes() -> Vec<ShapeFact> {
        let mut out = vec![
            declared_shape(),
            list_of_int(),
            map_str_int(),
            ShapeFact::plain_array(),
            sealed_list_str_int(),
            sealed_list_trailing_optional(),
        ];
        out.extend(arrays().iter().map(|a| ShapeFact::lift(a)));
        out
    }

    // ---- The reference results (measured PHP semantics) --------------------

    fn php_array_values(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .enumerate()
            .map(|(i, (_, v))| (ik(i64::try_from(i).expect("small")), v.clone()))
            .collect()
    }

    fn php_array_keys(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .enumerate()
            .map(|(i, (k, _))| (ik(i64::try_from(i).expect("small")), val_of_key(k)))
            .collect()
    }

    /// `array_flip`: values become keys (an `int` value gives an `int` key, a
    /// non-numeric `string` value a string key), and anything else is skipped.
    /// The universe carries no duplicate flipped key, so last-wins never arises.
    fn php_array_flip(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .filter_map(|(k, v)| {
                let nk = match v {
                    Val::Int(i) => VKey::Int(*i),
                    Val::Str(s) => VKey::Str(s.clone()),
                    _ => return None,
                };
                Some((nk, val_of_key(k)))
            })
            .collect()
    }

    /// `array_reverse($a)` with the default `$preserve_keys = false`: walk the
    /// entries backwards, keep string keys, renumber integer ones from 0.
    fn php_array_reverse(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        let mut next = 0i64;
        let mut out = Vec::with_capacity(a.len());
        for (k, v) in a.iter().rev() {
            match k {
                VKey::Str(_) => out.push((k.clone(), v.clone())),
                VKey::Int(_) => {
                    out.push((ik(next), v.clone()));
                    next += 1;
                }
            }
        }
        out
    }

    #[test]
    fn every_projection_admits_the_real_result() {
        let mut checked = 0usize;
        for shape in shapes() {
            for a in arrays() {
                if !shape.admits(&a) {
                    continue;
                }
                checked += 1;
                assert!(
                    project_values(&shape).admits(&php_array_values(&a)),
                    "array_values: {shape:?} on {a:?}"
                );
                assert!(
                    project_keys(&shape).admits(&php_array_keys(&a)),
                    "array_keys: {shape:?} on {a:?}"
                );
                assert!(
                    project_flip(&shape).admits(&php_array_flip(&a)),
                    "array_flip: {shape:?} on {a:?}"
                );
                assert!(
                    project_reverse(&shape).admits(&php_array_reverse(&a)),
                    "array_reverse: {shape:?} on {a:?}"
                );
                // The key-member transfer: `array_key_first`/`_last` return SOME
                // key, or `null` on the empty array — every one of which the
                // transfer's fact must admit.
                if let Some(keys) = shape_key_union(&shape) {
                    let member = if shape.non_empty {
                        keys
                    } else {
                        fact_admitting_null(&keys).expect("representable")
                    };
                    match (a.first(), a.last()) {
                        (Some((f, _)), Some((l, _))) => {
                            assert!(member.admits(&val_of_key(f)), "first: {shape:?} on {a:?}");
                            assert!(member.admits(&val_of_key(l)), "last: {shape:?} on {a:?}");
                        }
                        _ => assert!(member.admits(&Val::Null), "empty: {shape:?}"),
                    }
                }
            }
        }
        // The sweep is only evidence if the pairs exist.
        assert!(checked >= 20, "universe too small: {checked} admitted pairs");
    }

    // ---- §2's rule: declaration order is never read ------------------------

    #[test]
    fn array_key_first_is_never_the_declared_first_key() {
        // Negative soundness test: `array{a: int, b: int}` is a key SET;
        // PHPStan answers `'a'` here (phpstan/phpstan#14940) and is wrong on
        // `['b' => 1, 'a' => 2]`, which the shape admits just as well.
        let shape = ShapeFact::normalize(
            vec![(sk("a"), req(), slot(base_fact(Base::Int))), (sk("b"), req(), slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        let keys = shape_key_union(&shape).expect("enumerable");
        assert_eq!(
            keys,
            Fact::OneOf(vec![Val::Str("a".into()), Val::Str("b".into())])
        );
        assert!(keys.admits(&Val::Str("b".into())));
        // Both fields are Required, so the array cannot be empty and no `null`
        // joins in.
        assert!(shape.non_empty);
    }

    #[test]
    fn a_possibly_empty_shape_admits_null_as_its_key_member() {
        let keys = shape_key_union(&map_str_int()).expect("string class");
        let member = fact_admitting_null(&keys).expect("representable");
        assert_eq!(member, Fact::General { base: Base::String, nullable: true });
    }

    // ---- Per-projection structure -----------------------------------------

    #[test]
    fn array_values_is_a_list_of_the_value_union() {
        // `int ⊔ string` IS one fact now (issue #339), so the value slot carries
        // the union where it used to widen to the unknown floor.
        let p = project_values(&declared_shape());
        assert_eq!(p.is_list, Certainty::Yes);
        assert!(p.non_empty);
        assert!(p.fields.is_empty());
        assert_eq!(
            p.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            }
        );

        // A homogeneous shape keeps its value bound.
        let same = ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(base_fact(Base::Int))),
                (sk("b"), Presence::Optional, slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(
            project_values(&same).tail,
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) }
        );
    }

    #[test]
    fn array_keys_enumerates_a_sealed_shapes_keys_and_widens_an_unsealed_one() {
        assert_eq!(
            project_keys(&declared_shape()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![
                    Val::Str("a".into()),
                    Val::Str("b".into())
                ])),
            }
        );
        // An `array-key`-classed tail is `int|string`, which IS one fact now
        // (issue #339) — the element slot carries it instead of widening to the
        // unknown floor.
        assert_eq!(
            project_keys(&ShapeFact::plain_array()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            }
        );
        // An unsealed Yes-list's keys are `0..n-1` — never negative, so the
        // element bound sharpens past the bare `int` class (issue #165).
        assert_eq!(
            project_keys(&list_of_int()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::refined(
                    Base::Int,
                    Refinement::Int(IntRange::NON_NEGATIVE),
                    false
                )),
            }
        );
    }

    #[test]
    fn array_flip_drops_non_empty_and_only_claims_int_keys_for_int_values() {
        let p = project_flip(&declared_shape());
        // Values are `int|string`; a string value can still produce an INT key
        // (PHP's array-key cast), so the class is `array-key`.
        assert!(matches!(p.tail, Tail::Unsealed { key: KeyClass::ArrayKey, .. }));
        // A non-`int|string` value is skipped by the flip, so the result may be
        // empty even though the input is not.
        assert!(!p.non_empty);

        let ints = ShapeFact::normalize(
            vec![(sk("a"), req(), slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert!(matches!(
            project_flip(&ints).tail,
            Tail::Unsealed { key: KeyClass::Int, .. }
        ));
    }

    #[test]
    fn array_reverse_reads_the_key_structure_three_ways() {
        // All-int keys: everything is renumbered, so the result IS a list.
        assert_eq!(project_reverse(&list_of_int()).is_list, Certainty::Yes);
        // A required string key survives the reversal — never a list.
        assert_eq!(project_reverse(&declared_shape()).is_list, Certainty::No);
        // A string key that may or may not be there: the honest widening.
        let optional_str = ShapeFact::normalize(
            vec![(sk("a"), Presence::Optional, slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(project_reverse(&optional_str).is_list, Certainty::Maybe);
        // The entry count is preserved, so `non_empty` carries.
        assert!(project_reverse(&declared_shape()).non_empty);
    }

    // ---- The SEQUENCE lane (issue #165): isList == Yes is realizable order --

    #[test]
    fn array_values_is_the_identity_on_a_proven_list() {
        // A Yes-list's keys are already `0..n-1` in realizable order (probed:
        // `array_values(["x", 1]) === ["x", 1]`), so the projection returns
        // the subject's own shape — element types, optionality and
        // non-emptiness intact — where the set widening drops the
        // heterogeneous element types to the unknown floor.
        assert_eq!(project_values(&sealed_list_str_int()), sealed_list_str_int());
        assert_eq!(
            project_values(&sealed_list_trailing_optional()),
            sealed_list_trailing_optional()
        );
        // The unsealed forms: `list<T>` and `non-empty-list<T>`.
        assert_eq!(project_values(&list_of_int()), list_of_int());
        let non_empty = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) },
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(project_values(&non_empty), non_empty);
    }

    #[test]
    fn array_keys_of_a_proven_sequence_is_the_literal_key_list() {
        // Probed: `array_keys(["x", 1, 2.5]) === [0, 1, 2]` — a list's keys
        // ARE the sequence `0..n-1`, so the sealed all-required answer is the
        // literal `list{0, 1}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(Fact::Singleton(Val::Int(0)))),
                (ik(1), req(), slot(Fact::Singleton(Val::Int(1)))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_keys(&sealed_list_str_int()), expected);
        // A trailing optional carries per position: `list{A, 1?: B}` realizes
        // as `[A]` or `[A, B]`, whose key arrays are `[0]` and `[0, 1]` (both
        // probed) — exactly `list{0, 1?: 1}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(Fact::Singleton(Val::Int(0)))),
                (ik(1), Presence::Optional, slot(Fact::Singleton(Val::Int(1)))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_keys(&sealed_list_trailing_optional()), expected);
    }

    #[test]
    fn array_reverse_of_a_sealed_all_required_sequence_reverses_it() {
        // Probed at lengths 1, 2 and 3: `array_reverse(["a", "b", "c"]) ===
        // ["c", "b", "a"]` — position `i` takes the subject's position
        // `n-1-i`, so `list{string, int}` reverses to `list{int, string}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(1), req(), slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_reverse(&sealed_list_str_int()), expected);
    }

    #[test]
    fn array_reverse_declines_the_positional_claim_on_an_optional_key() {
        // Probed: `"a"` sits at index 0 in `array_reverse(["a"])` but at
        // index 1 in `array_reverse(["a", "b"])` — a variable-length reversal
        // smears every position, so an optional key keeps today's widening
        // exactly (the value union under an int-classed list tail, `non_empty`
        // carried). The union that value slot carries is `int|string`, which
        // issue #339 made expressible — the widening is the same one, said
        // more precisely.
        let expected = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            },
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(project_reverse(&sealed_list_trailing_optional()), expected);
    }

    #[test]
    fn a_set_subject_keeps_todays_widenings_exactly() {
        // The doctrinal pin (issue #165): `array{a: 1, b: 2}` is a key SET —
        // `['b' => 2, 'a' => 1]` is admitted just as well — so no projection
        // may consume an order from it. `array_values` still answers the
        // value union as a non-empty list: the issue's pinned
        // `non-empty-list<1|2>`.
        let subject = ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(Fact::Singleton(Val::Int(1)))),
                (sk("b"), req(), slot(Fact::Singleton(Val::Int(2)))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(subject.is_list, Certainty::No, "a required string key is never a list");
        let values = project_values(&subject);
        assert_eq!(
            values.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![Val::Int(1), Val::Int(2)])),
            }
        );
        assert!(values.non_empty);
        assert_eq!(values.is_list, Certainty::Yes);
        assert_eq!(
            project_keys(&subject).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![
                    Val::Str("a".into()),
                    Val::Str("b".into())
                ])),
            }
        );
    }

    #[test]
    fn a_guard_flagged_sequence_with_incoherent_fields_declines_the_positional_claims() {
        // `array{0: int, 2?: int}` narrowed by an `array_is_list` guard: the
        // flag is `Yes` (key `2` can then never actually be present), but the
        // FIELDS do not spell the sequence the flag claims. The positional
        // claims decline — `array_keys` answers from the flag alone (a list's
        // keys are never negative), `array_reverse` keeps the widening — while
        // `array_values` stays the identity, exact for every admitted value
        // whatever the fields say.
        let subject = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(2), Presence::Optional, slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(subject.is_list, Certainty::Yes, "the guard flag survives normalize");
        let keys = project_keys(&subject);
        assert!(keys.fields.is_empty(), "no literal key list from incoherent fields");
        assert_eq!(
            keys.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::refined(
                    Base::Int,
                    Refinement::Int(IntRange::NON_NEGATIVE),
                    false
                )),
            }
        );
        assert!(
            project_reverse(&subject).fields.is_empty(),
            "no reversed sequence from incoherent fields"
        );
        assert_eq!(project_values(&subject), subject);
    }
}
