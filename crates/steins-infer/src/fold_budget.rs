//! The fold seam's allocation budget (issue #258, issue #783): whether a call's
//! arguments ask the engine for more memory than a fold is for, priced before
//! the call is dispatched. Consulted by [`crate::fold`]'s admission sequence
//! after the width and shape gates.

use steins_catalog::FoldAllocation;
use steins_domain::php_is_numeric;
use steins_sidecar::FoldArg;

use crate::fold_args::FOLD_ARRAY_MAX_ENTRIES;

/// Bytes-ish. A `str_repeat` result of this size is already past anything a
/// literal fold is for. The runner widens a string result past the same number
/// (`STEINS_FOLD_STRING_MAX_BYTES`), so what this side admits and what that
/// side encodes are one budget.
const FOLD_ALLOCATION_MAX: i64 = 1 << 20;

/// The size an argument may ask the engine to ALLOCATE.
///
/// A fold sends the analysed source's own literals to a real PHP process, and
/// some builtins turn an integer argument into that much memory. `str_repeat`,
/// `str_pad` and `array_fill` are the obvious three, and the cost is not a slow
/// fold: past the engine's `memory_limit` PHP raises a FATAL, which is not
/// catchable, so the resident runner dies mid-NDJSON. ADR-0024's contract is
/// that a lost reply is never retried — respawn recovers the instance, not the
/// answer — so one line of analysed source (`str_repeat("ab", 2000000000)`)
/// costs a fold and prints a degradation notice for the whole run.
///
/// That is availability rather than soundness: the answer widens, it never
/// lies. But it is trivially reachable by anyone whose code Steins analyses,
/// which is the same trust boundary the callback gate is about, and refusing is
/// free — a value of a megabyte is not one this seam should be carrying into
/// the value domain anyway.
///
/// The size-shaped parameters are read from the mined `param_facts`: only the
/// declared NAME tells `str_pad($length)` from `strpos($offset)`, which is why
/// the miner keeps names. `FOLD_ALLOCATION_MAX` is a budget in the same spirit
/// as [`FOLD_ARRAY_MAX_ENTRIES`], and just as arbitrary: big enough that no
/// honest literal reaches it, small enough that the engine never notices.
///
/// A name whose size no single parameter carries has a
/// [`steins_catalog::fold_allocation`] row instead, priced by
/// [`within_allocation_row`]: `range` by the entries it would build, `sprintf`
/// by the bytes its format pads to (issue #783).
///
/// The probe harness learned this the hard way and grew the same rule; the seam
/// had not, and a probe harness is the thing under our control while analysed
/// source is not.
pub(crate) fn fold_within_allocation_budget(name: &str, args: &[FoldArg]) -> bool {
    if !within_allocation_row(name, args) {
        return false;
    }
    let Some(facts) = steins_catalog::param_facts(name) else {
        return true;
    };
    // The unit the count multiplies. `str_repeat("abc", n)` costs 3n bytes and
    // `str_pad("a", n)` costs n, so charging the count alone would bound the
    // wrong number: 2^20 repetitions of a 256-byte literal is 256 MB and a dead
    // child, with a count the size rule would wave through.
    let unit = match args.first() {
        Some(FoldArg::Str(subject)) => subject.len().max(1) as i64,
        _ => 1,
    };
    facts.param_names.iter().enumerate().all(|(i, pname)| {
        if !matches!(*pname, "length" | "times" | "count") {
            return true;
        }
        // A float or a numeric string reaching a size parameter coerces the same
        // way, so all three spellings are charged — and the string spelling is
        // charged through PHP's OWN numeric grammar, not Rust's.
        //
        // A first cut read the string with `parse::<i64>()` and allowed whatever
        // it could not read. PHP is weakly typed and Rust is not: `"2e9"`,
        // `" 2000000000"` and `"2000000000.0"` are all two billion to the
        // engine and all unreadable to that parser, so `str_repeat("x", "2e9")`
        // walked past the budget and killed the child — the exact bomb this
        // function exists to refuse (review finding, 2026-08-17).
        //
        // So: `php_is_numeric` decides what a number is, and a string that is
        // NOT one fails closed. At a size parameter a non-numeric string is a
        // `TypeError` under strict types and a coercion nobody should be
        // guessing at otherwise, and declining costs a fold rather than a child.
        let asked = match args.get(i) {
            Some(FoldArg::Int(v)) => *v,
            // Saturating on the cast, so `1e30` lands above the budget rather
            // than wrapping below it.
            Some(FoldArg::Float(v)) => *v as i64,
            Some(FoldArg::Str(v)) => {
                if !steins_domain::php_is_numeric(v) {
                    return false;
                }
                match v.trim().parse::<f64>() {
                    Ok(f) => f as i64,
                    // Numeric by PHP's grammar and unreadable here: refuse
                    // rather than assume it is small.
                    Err(_) => return false,
                }
            }
            _ => return true,
        };
        asked.saturating_mul(unit) <= FOLD_ALLOCATION_MAX
    })
}

/// The [`FoldAllocation`] half of the budget (issue #783): the shapes whose size
/// no single parameter carries. A name with no row passes.
///
/// Both limits are ones the runner already enforces on the way OUT — it widens
/// an array result past [`FOLD_ARRAY_MAX_ENTRIES`] and a string result past
/// [`FOLD_ALLOCATION_MAX`] bytes — so refusing past them here declines no fold
/// that could have answered. What changes is where the refusal happens: before
/// the call rather than after an allocation the child does not survive.
/// `range(0, 100000000)` asks for about 2 GB of packed entries against a
/// 256 MB `memory_limit`, and the fatal it raises is not a `Throwable`.
///
/// Anything the pricing cannot read declines. A declined fold widens, which is
/// silence rather than a finding; a misread one is a dead child.
fn within_allocation_row(name: &str, args: &[FoldArg]) -> bool {
    match steins_catalog::fold_allocation(name) {
        None => true,
        Some(FoldAllocation::Span { start, end, step }) => {
            range_entries(args.get(start), args.get(end), args.get(step))
                .is_some_and(|n| n <= FOLD_ARRAY_MAX_ENTRIES as u64)
        }
        Some(FoldAllocation::Format { format, values }) => match args.get(format) {
            Some(FoldArg::Str(f)) => format_bytes(f.as_bytes(), args.get(values..).unwrap_or(&[]))
                .is_some_and(|n| n <= FOLD_ALLOCATION_MAX as u64),
            // A format that is not a string literal is not one worth reading.
            _ => false,
        },
    }
}

/// A `range()` endpoint or step as the engine reads it.
#[derive(Debug, Clone, Copy)]
enum RangeNumber {
    Int(i64),
    Float(f64),
}

impl RangeNumber {
    fn as_f64(self) -> f64 {
        match self {
            Self::Int(v) => v as f64,
            Self::Float(v) => v,
        }
    }
}

/// How many entries `range($start, $end, $step)` builds — `floor(|end − start| /
/// |step|) + 1` — or `None` when this cannot be told before the call.
///
/// The endpoints are read through [`range_endpoint`], which is where PHP's
/// coercion lives. A pair of non-numeric strings is a CHARACTER range, at most
/// 256 entries whatever the bytes are; both read as `0` here, which prices one
/// entry and admits it, and nothing larger can hide behind that reading.
///
/// Integers are counted exactly. Anything with a float in it is counted in
/// `f64`, which near `PHP_INT_MAX` is off by a few thousand entries at most:
/// either side of 256 that is memory the engine never notices, and the runner
/// widens past 256 whichever way the count rounded.
fn range_entries(
    start: Option<&FoldArg>,
    end: Option<&FoldArg>,
    step: Option<&FoldArg>,
) -> Option<u64> {
    let start = range_endpoint(start?)?;
    let end = range_endpoint(end?)?;
    let step = range_step(step)?;
    if let (RangeNumber::Int(a), RangeNumber::Int(b), RangeNumber::Int(s)) = (start, end, step) {
        let span = (i128::from(b) - i128::from(a)).unsigned_abs() / s.unsigned_abs() as u128;
        return Some(u64::try_from(span).unwrap_or(u64::MAX).saturating_add(1));
    }
    let span = (end.as_f64() - start.as_f64()).abs() / step.as_f64();
    // A saturating cast: a span past `u64::MAX` prices as the largest count,
    // which declines.
    span.is_finite().then(|| (span.floor() as u64).saturating_add(1))
}

/// One `range()` endpoint, read the way every supported minor reads it — or
/// `None` where the minors disagree in a way that could matter, or where the
/// argument is not one `range` prices at all.
///
/// * An int or a finite float is itself. A non-finite one is a `ValueError`
///   from 8.3 and an oversized range before it; either way there is nothing to
///   price.
/// * A string PHP calls numeric is its number, on every minor.
/// * A string with **no numeric prefix** — `'a'`, `''`, `'  x'` — is `0` facing
///   a number. From 8.3 PHP coerces it with a warning, even under
///   `strict_types`, because `range` declares `string|int|float`; before 8.3
///   the same argument went through `zval_get_long`, which also reads `0`. That
///   is the probe's `range('a', 100000000)`, which killed the child: it is
///   `range(0, 100000000)` with a warning in front.
/// * A string with a numeric PREFIX and trailing bytes — `'12abc'` — declines.
///   8.3 reads it as `0` and 8.1 and 8.2 read it as `12`, so no one number is
///   right for the engine the project runs, and the rare call that writes one
///   costs a fold rather than a guess.
/// * `bool`, `null` and arrays decline: a `TypeError` in strict code, a
///   coercion nobody should price in weak code.
fn range_endpoint(arg: &FoldArg) -> Option<RangeNumber> {
    match arg {
        FoldArg::Int(v) => Some(RangeNumber::Int(*v)),
        FoldArg::Float(v) => v.is_finite().then_some(RangeNumber::Float(*v)),
        FoldArg::Str(s) if php_is_numeric(s) => php_numeric_value(s),
        FoldArg::Str(s) if has_no_numeric_prefix(s) => Some(RangeNumber::Int(0)),
        _ => None,
    }
}

/// The step's magnitude, `1` when absent. PHP takes the absolute value of a
/// negative step, and a zero step is a `ValueError`, so zero declines. The
/// string form must be numeric outright: before 8.3 a leading-numeric step
/// like `'0.5x'` was read as a float but dispatched as an integer, and its
/// integer is `0`.
fn range_step(step: Option<&FoldArg>) -> Option<RangeNumber> {
    let step = match step {
        None => RangeNumber::Int(1),
        // `PHP_INT_MIN` has no absolute value, and PHP refuses it too.
        Some(FoldArg::Int(v)) => RangeNumber::Int(v.checked_abs()?),
        Some(FoldArg::Float(v)) => RangeNumber::Float(v.abs()),
        Some(FoldArg::Str(s)) if php_is_numeric(s) => match php_numeric_value(s)? {
            RangeNumber::Int(v) => RangeNumber::Int(v.checked_abs()?),
            RangeNumber::Float(v) => RangeNumber::Float(v.abs()),
        },
        _ => return None,
    };
    let magnitude = step.as_f64();
    (magnitude.is_finite() && magnitude > 0.0).then_some(step)
}

/// The number a string PHP calls numeric spells — an int when it is one, a
/// finite float otherwise. The caller has already asked [`php_is_numeric`],
/// whose whitespace set is PHP's and is trimmed here the same way.
fn php_numeric_value(s: &str) -> Option<RangeNumber> {
    let digits = s.trim_matches(PHP_SPACE);
    if let Ok(v) = digits.parse::<i64>() {
        return Some(RangeNumber::Int(v));
    }
    digits.parse::<f64>().ok().filter(|v| v.is_finite()).map(RangeNumber::Float)
}

/// php-src's numeric whitespace, which is wider than Rust's ASCII trim.
const PHP_SPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0B', '\x0C'];

/// Whether no reading of `s` as a number starts: the first non-space byte is
/// not a sign, a digit or a `.`, so every minor's conversion is `0`.
fn has_no_numeric_prefix(s: &str) -> bool {
    match s.trim_start_matches(PHP_SPACE).bytes().next() {
        Some(b) => !matches!(b, b'+' | b'-' | b'.' | b'0'..=b'9'),
        None => true,
    }
}

/// The most bytes one value renders to before padding, when it is not a string
/// longer than this: `%f` of `1e308` at PHP's 53-digit precision ceiling is
/// under 400 bytes, and every integer and `Array` is shorter.
const RENDERED_VALUE_MAX: u64 = 512;

/// An upper bound on the bytes `sprintf($format, ...$values)` renders to, or
/// `None` when a conversion spec cannot be read.
///
/// The spec grammar is php-src's own (`php_formatted_print`):
/// `%[argnum$][flags][width][.precision][l]specifier`, where the flags are
/// `-`, `+`, a space, `0`, and `'` followed by any one padding byte. Each
/// conversion is charged its width, its precision and the value it consumes,
/// on top of the format's own bytes. The width is the bomb —
/// `'%2000000000d'` allocates it during the call, and `'%100000000d'` survives
/// the call and kills the child while the reply is encoded. The precision is
/// capped by the engine for floats and truncates a string, so charging it only
/// declines a spec nobody writes. The value is charged because the format can
/// repeat it: `%1$s` a thousand times over a megabyte literal is a gigabyte
/// with no width at all.
///
/// A `*` width or precision takes its number from the values, and declines
/// rather than tracking which value that is. So does every spec PHP itself
/// refuses — a missing or unknown specifier, a zero argnum — since those throw
/// and there is no answer to lose.
fn format_bytes(format: &[u8], values: &[FoldArg]) -> Option<u64> {
    let mut total = format.len() as u64;
    let mut next_value = 0usize;
    let mut i = 0usize;
    while let Some(at) = format[i..].iter().position(|&b| b == b'%') {
        i += at + 1;
        if format.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        let (mut width, mut precision, mut argnum) = (0u64, 0u64, None);
        // php-src skips the modifiers when a letter follows the `%` directly.
        if !format.get(i)?.is_ascii_alphabetic() {
            // `%2$s`: digits and a `$` name the value; digits alone are the width.
            let digits = format[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && format.get(i + digits) == Some(&b'$') {
                let n = usize::try_from(spec_number(&format[i..i + digits])).ok()?;
                argnum = Some(n.checked_sub(1)?);
                i += digits + 1;
            }
            loop {
                match *format.get(i)? {
                    b' ' | b'0' | b'-' | b'+' => i += 1,
                    b'\'' => {
                        format.get(i + 1)?;
                        i += 2;
                    }
                    _ => break,
                }
            }
            if format.get(i) == Some(&b'*') {
                return None;
            }
            let digits = format[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            width = spec_number(&format[i..i + digits]);
            i += digits;
            if format.get(i) == Some(&b'.') {
                i += 1;
                if format.get(i) == Some(&b'*') {
                    return None;
                }
                let digits = format[i..].iter().take_while(|b| b.is_ascii_digit()).count();
                precision = spec_number(&format[i..i + digits]);
                i += digits;
            }
        }
        if format.get(i) == Some(&b'l') {
            i += 1;
        }
        let value = match *format.get(i)? {
            // A `%` after modifiers renders one `%` and consumes no value.
            b'%' => None,
            b'b' | b'c' | b'd' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'h' | b'H' | b'o'
            | b's' | b'u' | b'x' | b'X' => {
                let n = argnum.unwrap_or_else(|| {
                    next_value += 1;
                    next_value - 1
                });
                Some(values.get(n))
            }
            _ => return None,
        };
        i += 1;
        let rendered = match value.flatten() {
            Some(FoldArg::Str(s)) => (s.len() as u64).max(RENDERED_VALUE_MAX),
            Some(_) => RENDERED_VALUE_MAX,
            // `%%`-alike, or a value the call does not pass (an
            // `ArgumentCountError`, which renders nothing).
            None => 0,
        };
        total = total.saturating_add(width).saturating_add(precision).saturating_add(rendered);
    }
    Some(total)
}

/// A run of ASCII digits as a number, saturating rather than wrapping, so an
/// absurd width prices as an absurd width.
fn spec_number(digits: &[u8]) -> u64 {
    digits.iter().fold(0u64, |n, d| n.saturating_mul(10).saturating_add(u64::from(d - b'0')))
}

#[cfg(test)]
mod tests {
    use super::{FoldArg, fold_within_allocation_budget};

    fn i(v: i64) -> FoldArg {
        FoldArg::Int(v)
    }

    fn f(v: f64) -> FoldArg {
        FoldArg::Float(v)
    }

    fn s(v: &str) -> FoldArg {
        FoldArg::Str(v.to_owned())
    }

    fn admits(name: &str, args: &[FoldArg]) -> bool {
        fold_within_allocation_budget(name, args)
    }

    /// The shapes issue #783 measured killing the child, and the two string
    /// spellings the probe found reaching the same allocation — one of them
    /// under `strict_types`, since `range` declares `string|int|float` and 8.3
    /// coerces a non-numeric string facing a number to `0`.
    #[test]
    fn the_range_bombs_decline() {
        for (label, args) in [
            ("range(0, 2000000000)", vec![i(0), i(2_000_000_000)]),
            ("range(0, 100000000)", vec![i(0), i(100_000_000)]),
            ("range(0, -100000000)", vec![i(0), i(-100_000_000)]),
            ("range(0, 2000000000, 3)", vec![i(0), i(2_000_000_000), i(3)]),
            ("range(0, 100000000, 0.5)", vec![i(0), i(100_000_000), f(0.5)]),
            ("range('0', '100000000')", vec![s("0"), s("100000000")]),
            ("range('a', 100000000)", vec![s("a"), i(100_000_000)]),
            ("range(100000000, 'a')", vec![i(100_000_000), s("a")]),
            ("range('a', '100000000')", vec![s("a"), s("100000000")]),
            ("range(' 1e8', 0)", vec![s(" 1e8"), i(0)]),
            ("range(i64::MIN, i64::MAX)", vec![i(i64::MIN), i(i64::MAX)]),
            ("range(0.0, 1e300)", vec![f(0.0), f(1e300)]),
            ("range(0, 1, 1e-300)", vec![i(0), i(1), f(1e-300)]),
        ] {
            assert!(!admits("range", &args), "{label} reached the engine");
        }
    }

    /// The runner widens an array result past 256 entries, so the line sits
    /// exactly there: 256 folds, 257 declines. Anything lower would decline a
    /// fold that answers today.
    #[test]
    fn the_range_budget_is_the_array_result_budget() {
        assert!(admits("range", &[i(0), i(255)]));
        assert!(!admits("range", &[i(0), i(256)]));
        assert!(admits("range", &[i(255), i(0)]), "descending counts the same");
        assert!(admits("range", &[i(0), i(510), i(2)]));
        assert!(!admits("range", &[i(0), i(512), i(2)]));
        assert!(admits("range", &[i(0), i(510), i(-2)]), "a negative step is its magnitude");
        assert!(admits("range", &[f(0.0), f(25.5), f(0.1)]));
        assert!(admits("range", &[i(i64::MAX - 10), i(i64::MAX)]), "no overflow near the edge");
    }

    /// The ranges people write still reach the engine.
    #[test]
    fn an_ordinary_range_is_admitted() {
        for (label, args) in [
            ("range(1, 10)", vec![i(1), i(10)]),
            ("range(1, 9, 2)", vec![i(1), i(9), i(2)]),
            ("range(1, 2, 0.5)", vec![i(1), i(2), f(0.5)]),
            ("range('a', 'e')", vec![s("a"), s("e")]),
            ("range('A', 'z')", vec![s("A"), s("z")]),
            ("range('1', '9')", vec![s("1"), s("9")]),
            ("range('5', 'z')", vec![s("5"), s("z")]),
            ("range(' 1 ', '3')", vec![s(" 1 "), s("3")]),
            ("range(0, 100, '10')", vec![i(0), i(100), s("10")]),
        ] {
            assert!(admits("range", &args), "{label} was declined");
        }
    }

    /// What the pricing cannot read declines. Each of these is either an error
    /// in PHP or a reading the supported minors do not share.
    #[test]
    fn an_unpriceable_range_declines() {
        for (label, args) in [
            ("range(0)", vec![i(0)]),
            ("range(0, 5, 0)", vec![i(0), i(5), i(0)]),
            ("range(0, 5, PHP_INT_MIN)", vec![i(0), i(5), i(i64::MIN)]),
            ("range(0, 5, '0.5x')", vec![i(0), i(5), s("0.5x")]),
            ("range('12abc', 3)", vec![s("12abc"), i(3)]),
            ("range(true, 5)", vec![FoldArg::Bool(true), i(5)]),
            ("range(null, 5)", vec![FoldArg::Null, i(5)]),
            ("range(0, INF)", vec![i(0), f(f64::INFINITY)]),
            ("range(0, NAN)", vec![i(0), f(f64::NAN)]),
            ("range([], 5)", vec![FoldArg::Array(Vec::new()), i(5)]),
        ] {
            assert!(!admits("range", &args), "{label} was priced");
        }
    }

    /// The format shapes: the width issue #783 measured, and the other ways a
    /// format can ask for size without one parameter saying so.
    #[test]
    fn the_format_bombs_decline() {
        let megabyte = "x".repeat(1 << 20);
        for (label, args) in [
            ("sprintf('%2000000000d', 1)", vec![s("%2000000000d"), i(1)]),
            ("sprintf('%100000000d', 1)", vec![s("%100000000d"), i(1)]),
            ("sprintf('%-100000000s', 'x')", vec![s("%-100000000s"), s("x")]),
            ("sprintf(\"%'*100000000s\", 'x')", vec![s("%'*100000000s"), s("x")]),
            ("sprintf('%1$100000000s', 'x')", vec![s("%1$100000000s"), s("x")]),
            ("sprintf('%99999999999999999999999d', 1)", vec![s("%99999999999999999999999d"), i(1)]),
            ("sprintf('%1$s%1$s', <1 MiB>)", vec![s("%1$s%1$s"), s(&megabyte)]),
        ] {
            assert!(!admits("sprintf", &args), "{label} reached the engine");
        }
        // Many modest widths add up the same as one large one.
        let many = "%1000d".repeat(2000);
        assert!(!admits("sprintf", &[s(&many), i(1)]), "the widths are summed");
        // The engine caps a float's precision at 53 digits, so this one would
        // survive; it is charged anyway, which costs a spec nobody writes.
        assert!(!admits("sprintf", &[s("%.100000000f"), f(1.5)]));
    }

    #[test]
    fn an_ordinary_format_is_admitted() {
        for (label, args) in [
            ("sprintf('%d items', 3)", vec![s("%d items"), i(3)]),
            ("sprintf('%05.2f', 1.5)", vec![s("%05.2f"), f(1.5)]),
            ("sprintf('%-10s|', 'x')", vec![s("%-10s|"), s("x")]),
            ("sprintf(\"%'*10s\", 'x')", vec![s("%'*10s"), s("x")]),
            ("sprintf('%2$s %1$s', 'a', 'b')", vec![s("%2$s %1$s"), s("a"), s("b")]),
            ("sprintf('100%%')", vec![s("100%%")]),
            ("sprintf('%5%')", vec![s("%5%")]),
            ("sprintf('%x', -1)", vec![s("%x"), i(-1)]),
            ("sprintf('%ld', 5)", vec![s("%ld"), i(5)]),
            ("sprintf('no specs')", vec![s("no specs")]),
        ] {
            assert!(admits("sprintf", &args), "{label} was declined");
        }
    }

    /// A spec that takes its width from the values, or that PHP itself
    /// refuses, declines rather than being guessed at.
    #[test]
    fn an_unreadable_format_declines() {
        for (label, args) in [
            ("sprintf('%*d', 5, 1)", vec![s("%*d"), i(5), i(1)]),
            ("sprintf('%.*f', 2, 1.5)", vec![s("%.*f"), i(2), f(1.5)]),
            ("sprintf('%')", vec![s("%")]),
            ("sprintf('%5')", vec![s("%5")]),
            ("sprintf('%y', 1)", vec![s("%y"), i(1)]),
            ("sprintf('%0$s', 'x')", vec![s("%0$s"), s("x")]),
            ("sprintf(\"%'\")", vec![s("%'")]),
            ("sprintf(5)", vec![i(5)]),
        ] {
            assert!(!admits("sprintf", &args), "{label} was priced");
        }
    }

    /// The named-parameter rule is unchanged underneath the rows.
    #[test]
    fn the_named_size_rule_still_charges() {
        assert!(!admits("str_repeat", &[s("ab"), i(2_000_000_000)]));
        assert!(!admits("str_pad", &[s("a"), s("2e9")]));
        assert!(admits("str_repeat", &[s("ab"), i(3)]));
        assert!(admits("strtoupper", &[s("still alive")]));
    }
}
