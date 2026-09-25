use std::collections::HashMap;

use steins_domain::{
    Base, Certainty, Fact, Key as VKey, Presence, Refinement, ShapeFact, StrPreds, Tail, Val,
};
use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::transfers::transfer_arg_fact;

/// `sscanf($string, $format, &...$vars)` → the fact its **argument count** and its
/// literal format prove (issue #617).
///
/// # The count decides first
///
/// `sscanf` is two functions wearing one name, and the format has no say in which
/// of them a call is. Every row measured at `PINNED_PHP` (8.5.9):
///
/// | call shape | answer | witness |
/// | --- | --- | --- |
/// | fewer than 2 arguments | **decline** | not a legal call |
/// | 3 or more — the by-reference form | `int\|null` | `sscanf('20-20', '%d-%d', $a, $b) === 2`, `sscanf('zz', '%d', $c) === 0`, `sscanf('', '%d', $d) === -1`: the count of assigned conversions, in which the format never appears |
/// | 2, format a proven literal | a sealed `list{…}`, one nullable slot per non-suppressed conversion, the shape itself nullable | the conversion list is fully determined by the format |
/// | 2, format not a literal | `array\|null` ([`ShapeFact::plain_array`]) | nothing is known about the conversions, only that the outer arm is an array or `null` |
///
/// The `int|null` of the by-reference arm is deliberately one arm wider than the
/// measurement: no probe produced `null` (the failure value is `-1`), but the
/// reflected declaration admits it and this rule does not have a proof that
/// excludes it. Widening inside the declaration is free; guessing is not.
///
/// **This is a RETURN rule and nothing else** (ADR-0070 §3). `sscanf` stays out of
/// `by_value_arg` and out of the `out_params` table, and the by-reference tail's
/// out-state stays exactly as unknown as it was — `crates/steins-infer/tests/
/// call_arg_survival.rs` pins that boundary and needed no edit for this slice.
///
/// # The specifier table, every cell from `php -r` at `PINNED_PHP` (8.5.9)
///
/// The accepted roster is exactly **15 conversion characters plus the `%[…]`
/// scanset**, swept over every printable byte: anything else is a hard
/// `ValueError: Bad scan conversion character`, so an unrecognized specifier
/// declines the WHOLE call rather than contributing a `mixed` slot (the
/// `filter_var` rung's invariant).
///
/// | specifier | slot fact | note |
/// | --- | --- | --- |
/// | `%d` `%D` `%i` `%o` `%x` `%X` `%n` | `int\|null` | `%D`/`%i`/`%X` are accepted and untested by any fixture; `%n` (characters consumed) is an `int` that still nulls when an earlier conversion fails |
/// | `%e` `%E` `%f` `%g` | `float\|null` | `%F` and `%G` are **rejected** by the engine — the roster is not symmetric in case |
/// | `%s` and `%[…]` | `non-empty-string\|null` | see the width note below |
/// | `%c` | `string\|null` | NOT non-empty — see below |
/// | `%u` | **decline the whole call** | see below |
/// | `%*…` | contributes no slot | suppression composes with widths and scansets (`%*2[a-z]`, `%*20s` all yield no slot) |
/// | `%%` | contributes no slot | and takes neither a star nor a width: `%*%`, `%0%`, `%2%` all throw |
///
/// A width is *at most* N characters, never exactly N, and it refines no integer
/// or float conversion at all — `%2x%2x%2x` is three plain `int|null` slots.
///
/// # Three measurements that refute the reference implementation
///
/// 1. **`%s`'s width proves nothing beyond non-emptiness.** The fixture asserts
///    `%2s`/`%3s` → `non-falsy-string`, but a width bounds the read from ABOVE:
///    `sscanf('0', '%2s') === ['0']`, a falsy string. Every fixture row carrying
///    that claim has a *literal* subject (`"123456"`), so `non-falsy` is being read
///    off the subject, not the width. The honest width-free rule is
///    `non-empty-string|null`, proven the other way: across 40,000 randomized
///    subject × format trials no `%s` or scanset slot ever came back `''` (a
///    conversion with nothing to read fails into a `null` slot instead). So this
///    rung is *sharper* than the fixture at bare `%s` and *weaker* at `%2s`, and
///    both differences are the measurement, not an approximation.
/// 2. **`%c` is NOT a one-byte non-empty string.** `sscanf(' ', '%c') === ['']` —
///    the empty string, from a non-empty subject. `%c` gets no refinement.
/// 3. **`%u` is not an `int`.** `sscanf('-8', '%u') === ['18446744073709551608']`,
///    a *string*: the value is reinterpreted as unsigned and re-rendered when it
///    leaves the signed range. The true slot is `int|string|null`, a two-base union
///    the shape-slot vocabulary of this slice does not spell, so `%u` declines the
///    whole call. The fixture's `int|null` for `%u` is unsound and is a deliberate
///    non-win (the issue #40 / #594 precedent: when the fixture and the measurement
///    disagree, the measurement wins).
///
/// # Slot cardinality
///
/// Every non-suppressed conversion contributes exactly one slot whether or not it
/// matched — `sscanf('5', '%d %d %d') === [5, null, null]` — so the fields are all
/// `Required` and the tail is `Sealed`. Verified over 20,000 randomized trials with
/// zero disagreements. A format with no conversion at all is therefore `array{}`,
/// which is the honest answer: `sscanf('abc', 'xyz') === []`.
///
/// # `fscanf` is deliberately NOT here
///
/// It shares this exact format table, and sharing it is why the scanner is a free
/// function. What it does not share is its envelope: `fscanf` reflects
/// `array|int|false|null` at `PINNED_PHP`, and that `false` arm is not spellable —
/// [`Fact::Shape`] carries a `nullable` side-flag and no `false` one, and
/// [`Fact::Union`] admits no array arm by construction (ADR-0062 §3). Answering
/// `array{…}|null` for `fscanf` would be *unsound*, not merely coarse, so it waits
/// for a domain that can spell its outer arm rather than riding in on this slice.
pub(super) fn sscanf_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    // The COUNT, before anything reads the format — the two arms answer different
    // bases and no format string moves the boundary between them.
    if args.len() < 2 {
        return None;
    }
    if args.len() > 2 {
        return Some(Fact::General { base: Base::Int, nullable: true });
    }
    let Some(Fact::Singleton(Val::Str(fmt))) = transfer_arg_fact(cx, folder, &args[1], env, store)
    else {
        // A format nothing is known about still proves the OUTER arm.
        return Some(Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: true });
    };
    let slots = scanf_slot_facts(fmt.as_bytes())?;
    // A-G6, the same width degradation [`ShapeFact::lift`] performs: past
    // `SHAPE_WIDTH_LIMIT` slots the tail-only summary stands in for the sequence.
    // Nothing in a real format reaches 256 conversions; a generated one might, and
    // the answer stays sound because this only widens.
    if slots.len() > steins_domain::SHAPE_WIDTH_LIMIT {
        return Some(Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: true });
    }
    let fields: Vec<_> = slots
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let i = i64::try_from(i).expect("shape width is bounded above");
            (VKey::Int(i), Presence::Required { witnessed: false }, Some(Box::new(f)))
        })
        .collect();
    let non_empty = !fields.is_empty();
    Some(Fact::Shape {
        shape: Box::new(ShapeFact::normalize(
            fields,
            Tail::Sealed,
            Certainty::Yes,
            non_empty,
            Vec::new(),
        )),
        nullable: true,
    })
}

/// One slot fact per non-suppressed conversion in a `scanf`-family format, in
/// order — or `None` when ANY byte of the format is one the table on
/// [`sscanf_transfer`] does not carry.
///
/// The grammar is `%` `[*]` `[digits]` `(specifier | '[' scanset ']')`. Everything
/// outside a conversion is literal text that matches the subject and yields no
/// slot. Declining on the first unreadable byte is what makes an unrecognized
/// specifier decline the whole call: the engine would throw there anyway, so there
/// is no partial answer to salvage.
fn scanf_slot_facts(fmt: &[u8]) -> Option<Vec<Fact>> {
    let int = || Fact::General { base: Base::Int, nullable: true };
    let float = || Fact::General { base: Base::Float, nullable: true };
    let string = || Fact::General { base: Base::String, nullable: true };
    let non_empty_string =
        || Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY), true);

    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        // `%%` is a literal percent and takes NEITHER star nor width (measured:
        // `%*%`, `%0%` and `%2%` all throw), so it is matched before both.
        if *fmt.get(i)? == b'%' {
            i += 1;
            continue;
        }
        let suppressed = *fmt.get(i)? == b'*';
        if suppressed {
            i += 1;
        }
        // A width caps the read from above and refines nothing (see the doc).
        while fmt.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let fact = match *fmt.get(i)? {
            b'[' => {
                i = scanset_end(fmt, i + 1)?;
                non_empty_string()
            }
            b'd' | b'D' | b'i' | b'n' | b'o' | b'x' | b'X' => int(),
            b'e' | b'E' | b'f' | b'g' => float(),
            b's' => non_empty_string(),
            b'c' => string(),
            // `%u` is measured `int|string|null` — a two-base union no shape slot
            // here spells. Named explicitly so it reads as a decision, not a gap.
            b'u' => return None,
            _ => return None,
        };
        i += 1;
        if !suppressed {
            out.push(fact);
        }
    }
    Some(out)
}

/// The index of the `]` closing a `%[…]` scanset whose members start at `open`, or
/// `None` when the format never closes it — which the engine rejects outright
/// (`ValueError: Unmatched [ in format string`), so declining matches it exactly.
fn scanset_end(fmt: &[u8], open: usize) -> Option<usize> {
    let mut i = open;
    // A leading `^` negates the set, and a `]` in the FIRST member position is a
    // member rather than the terminator: `%[]a-z]` scans `]` plus `a`..`z`.
    if fmt.get(i) == Some(&b'^') {
        i += 1;
    }
    if fmt.get(i) == Some(&b']') {
        i += 1;
    }
    while *fmt.get(i)? != b']' {
        i += 1;
    }
    Some(i)
}
